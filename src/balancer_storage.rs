//! Balancer V2 Vault + WeightedPool storage slot layout and decoders.
//!
//! Mirrors `scrape_reth::balancer_storage` (the defi-side reader) so the ExEx can
//! hydrate Balancer V2 weighted pools directly from a reth `StateProvider` at the
//! startup anchor (and on live `.add`). The slot math is the empirically-verified
//! layout documented there:
//!
//! - MinimalSwapInfo (spec 0x0001): per-token balances in
//!   `_minimalSwapInfoPoolsBalances` (Vault slot 7), one packed word per token.
//! - TwoToken (spec 0x0002): both balances packed in one `sharedCash` word under
//!   `_twoTokenPoolsTokens` (Vault slot 9).
//! - Pool contract slot 7: `_swapFeePercentage` (1e18 scale).
//!
//! Weights, token order and scaling factors are Solidity `immutable`s embedded in
//! pool bytecode (NOT in storage); they are sourced from whitelist metadata.

use alloy_primitives::{keccak256, Address, B256, U256};

/// Vault base slot for `_minimalSwapInfoPoolsBalances`.
const VAULT_MINIMAL_SWAP_INFO_BALANCES_SLOT: u64 = 7;
/// Pool contract slot for `_swapFeePercentage` (plain uint256 in base `WeightedPool`
/// and later single-slot implementations).
const POOL_SWAP_FEE_SLOT: u64 = 7;
/// Pool contract slot for `WeightedPool2Tokens._miscData`, which packs the swap fee
/// (empirically verified: bits [86:150]).
const POOL_MISC_DATA_SLOT: u64 = 8;
/// Vault base slot for `_twoTokenPoolsTokens`.
const VAULT_TWO_TOKEN_TOKENS_SLOT: u64 = 9;

/// Balancer weighted-pool swap-fee bounds (1e18 scale): min 0.0001%, max 10%.
/// Used to disambiguate which storage layout a pool uses without hard-coding the
/// implementation version — an out-of-range read means "wrong slot for this impl".
const MIN_SWAP_FEE: u64 = 1_000_000_000_000;
const MAX_SWAP_FEE: u64 = 100_000_000_000_000_000;

/// Slot holding `WeightedPool2Tokens._miscData` (packed pool config incl. swap fee).
pub fn misc_data_slot() -> U256 {
    U256::from(POOL_MISC_DATA_SLOT)
}

/// Extract the swap fee from a `WeightedPool2Tokens._miscData` word — bits [86:150]
/// (1e18 scale). Verified against mainnet pools 0x96646936 (0.3%) and 0x0b09dea (0.05%).
pub fn decode_two_token_swap_fee(misc: U256) -> u64 {
    ((misc >> 86_usize) & U256::from(u64::MAX)).as_limbs()[0]
}

/// Whether a decoded value is a plausible Balancer weighted-pool swap fee. Lets the
/// caller try known storage layouts and reject a slot that clearly isn't the fee.
pub fn is_plausible_swap_fee(fee: u64) -> bool {
    (MIN_SWAP_FEE..=MAX_SWAP_FEE).contains(&fee)
}

/// Slot holding the newer `BasePool._poolState` word (WeightedPool v2+), which packs
/// the swap fee in bits [192:256) — empirically verified against the whitelisted
/// v2/v3/v4 pools (e.g. 0xb814ca71 at 0.3%, 0x31aa8cc6 at 2%). Numerically the same
/// slot index as `WeightedPool2Tokens._miscData`, decoded differently.
pub fn pool_state_slot() -> U256 {
    U256::from(POOL_MISC_DATA_SLOT)
}

/// Extract the swap fee from a v2+ `BasePool._poolState` word — bits [192:256)
/// (1e18 scale).
pub fn decode_pool_state_swap_fee(word: U256) -> u64 {
    (word >> 192_usize).as_limbs()[0]
}

/// Where a Balancer weighted-pool implementation stores its swap fee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalancerFeeLayout {
    /// Original `WeightedPool`: plain uint256 at pool slot 7.
    Slot7,
    /// `WeightedPool2Tokens`: packed in `_miscData` (slot 8) bits [86:150).
    MiscData,
    /// v2+ `BasePool`: packed in `_poolState` (slot 8) bits [192:256).
    PoolState,
}

/// Map a whitelist `additional_data.version` value to its fee-storage layout.
/// Versions are classified at DB ingestion (`populate_balancer_v2_pools.py`):
/// `"v1"` | `"2tokens"` | `"v2"`/`"v3"`/... (any v ≥ 2 shares the `_poolState`
/// layout). Unknown strings return `None` so the caller refuses to guess.
pub fn fee_layout_for_version(version: &str) -> Option<BalancerFeeLayout> {
    match version {
        "v1" => Some(BalancerFeeLayout::Slot7),
        "2tokens" => Some(BalancerFeeLayout::MiscData),
        _ => version
            .strip_prefix('v')
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| *n >= 2)
            .map(|_| BalancerFeeLayout::PoolState),
    }
}

/// Pool specialization extracted from poolId bytes [20..22].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolSpecialization {
    General,
    MinimalSwapInfo,
    TwoToken,
}

impl PoolSpecialization {
    pub fn from_pool_id(pool_id: &[u8; 32]) -> Self {
        match u16::from_be_bytes([pool_id[20], pool_id[21]]) {
            1 => PoolSpecialization::MinimalSwapInfo,
            2 => PoolSpecialization::TwoToken,
            _ => PoolSpecialization::General,
        }
    }
}

/// Extract pool contract address from the first 20 bytes of a poolId.
pub fn pool_address(pool_id: &[u8; 32]) -> Address {
    Address::from_slice(&pool_id[..20])
}

fn b256_to_u256(slot: B256) -> U256 {
    U256::from_be_bytes(slot.0)
}

/// Storage key for a MinimalSwapInfo pool's per-token balance:
/// `keccak256(token ‖ keccak256(poolId ‖ baseSlot))`.
pub fn vault_balance_slot(pool_id: &[u8; 32], token: &Address) -> U256 {
    let mut inner_input = [0u8; 64];
    inner_input[..32].copy_from_slice(pool_id);
    inner_input[56..64].copy_from_slice(&VAULT_MINIMAL_SWAP_INFO_BALANCES_SLOT.to_be_bytes());
    let inner = keccak256(inner_input);

    let mut outer_input = [0u8; 64];
    outer_input[12..32].copy_from_slice(token.as_slice());
    outer_input[32..64].copy_from_slice(inner.as_slice());
    b256_to_u256(keccak256(outer_input))
}

/// Pool swap-fee storage key (pool contract slot 7).
pub fn pool_fee_slot() -> U256 {
    U256::from(POOL_SWAP_FEE_SLOT)
}

/// `struct_base = keccak256(poolId ‖ 9)` for a TwoToken pool.
fn two_token_struct_base(pool_id: &[u8; 32]) -> U256 {
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(pool_id);
    input[56..64].copy_from_slice(&VAULT_TWO_TOKEN_TOKENS_SLOT.to_be_bytes());
    b256_to_u256(keccak256(input))
}

/// Slot for tokenA in a TwoToken pool: `struct_base + 0`.
pub fn two_token_token_a_slot(pool_id: &[u8; 32]) -> U256 {
    two_token_struct_base(pool_id)
}

/// Slot for tokenB in a TwoToken pool: `struct_base + 1`.
pub fn two_token_token_b_slot(pool_id: &[u8; 32]) -> U256 {
    two_token_struct_base(pool_id) + U256::from(1u64)
}

/// `pairHash = keccak256(abi.encodePacked(tokenA, tokenB))`, address-sorted.
pub fn two_token_pair_hash(token_a: &Address, token_b: &Address) -> B256 {
    let mut input = [0u8; 40];
    input[..20].copy_from_slice(token_a.as_slice());
    input[20..].copy_from_slice(token_b.as_slice());
    keccak256(input)
}

/// Slot for `sharedCash`: `keccak256(pairHash ‖ (struct_base + 2))`.
pub fn two_token_shared_cash_slot(pool_id: &[u8; 32], pair_hash: B256) -> U256 {
    let balance_mapping_base = two_token_struct_base(pool_id) + U256::from(2u64);
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(pair_hash.as_slice());
    input[32..].copy_from_slice(&balance_mapping_base.to_be_bytes::<32>());
    b256_to_u256(keccak256(input))
}

/// Decode a MinimalSwapInfo packed balance word into `(cash, managed, block)`.
/// Packing: `[32b lastChangeBlock | 112b managed | 112b cash]`.
pub fn decode_packed_balance(packed: U256) -> (u128, u128, u32) {
    let mask_112: U256 = (U256::from(1u64) << 112) - U256::from(1u64);
    let cash_u256 = packed & mask_112;
    let managed_u256 = (packed >> 112_usize) & mask_112;
    let block_u256 = packed >> 224_usize;
    let cash = cash_u256.as_limbs()[0] as u128 | ((cash_u256.as_limbs()[1] as u128) << 64);
    let managed = managed_u256.as_limbs()[0] as u128 | ((managed_u256.as_limbs()[1] as u128) << 64);
    (cash, managed, block_u256.as_limbs()[0] as u32)
}

/// Decode a TwoToken shared-balance word into `(balance_a, balance_b, block)`.
/// Packing: `[32b lastChangeBlock | 112b balanceB | 112b balanceA]`.
pub fn decode_two_token_shared(packed: U256) -> (u128, u128, u32) {
    let mask_112: U256 = (U256::from(1u64) << 112) - U256::from(1u64);
    let a_u256 = packed & mask_112;
    let b_u256 = (packed >> 112_usize) & mask_112;
    let block_u256 = packed >> 224_usize;
    let a = a_u256.as_limbs()[0] as u128 | ((a_u256.as_limbs()[1] as u128) << 64);
    let b = b_u256.as_limbs()[0] as u128 | ((b_u256.as_limbs()[1] as u128) << 64);
    (a, b, block_u256.as_limbs()[0] as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_layout_mapping_from_version() {
        assert_eq!(fee_layout_for_version("v1"), Some(BalancerFeeLayout::Slot7));
        assert_eq!(
            fee_layout_for_version("2tokens"),
            Some(BalancerFeeLayout::MiscData)
        );
        for v in ["v2", "v3", "v4", "v5"] {
            assert_eq!(
                fee_layout_for_version(v),
                Some(BalancerFeeLayout::PoolState),
                "{v} should use the _poolState layout"
            );
        }
        // Unknown strings must refuse rather than guess a slot.
        assert_eq!(fee_layout_for_version(""), None);
        assert_eq!(fee_layout_for_version("v0"), None);
        assert_eq!(fee_layout_for_version("weighted"), None);
        assert_eq!(fee_layout_for_version("vX"), None);
    }

    #[test]
    fn decode_pool_state_fee_bits() {
        // Empirically verified: v4 pool 0xb814ca71 _poolState with fee 0.3% (3e15)
        // packed in the top 64 bits.
        let word = U256::from(3_000_000_000_000_000_u64) << 192_usize;
        assert_eq!(decode_pool_state_swap_fee(word), 3_000_000_000_000_000);
        // Lower-bit noise (other packed state) must not leak into the fee.
        let word = word | U256::from(u128::MAX);
        assert_eq!(decode_pool_state_swap_fee(word), 3_000_000_000_000_000);
    }

    #[test]
    fn pool_specialization_from_id() {
        let mut pid = [0u8; 32];
        pid[21] = 0x01;
        assert_eq!(
            PoolSpecialization::from_pool_id(&pid),
            PoolSpecialization::MinimalSwapInfo
        );
        pid[21] = 0x02;
        assert_eq!(
            PoolSpecialization::from_pool_id(&pid),
            PoolSpecialization::TwoToken
        );
        pid[21] = 0x00;
        assert_eq!(
            PoolSpecialization::from_pool_id(&pid),
            PoolSpecialization::General
        );
    }

    #[test]
    fn verified_3token_balance_slot() {
        // Verified empirically against scrape_reth: DPI balance slot for the
        // 3-token pool 0x61d5dc...0001.
        let mut pid = [0u8; 32];
        hex::decode_to_slice(
            "61d5dc44849c9c87b0856a2a311536205c96c7fd000100000000000000000001",
            &mut pid,
        )
        .expect("pid");
        let dpi: Address = "0x1494CA1F11D487c2bBe4543E90080AeBa4BA3C2b"
            .parse()
            .expect("addr");
        let slot = vault_balance_slot(&pid, &dpi);
        assert_eq!(
            format!("{:?}", B256::from(slot.to_be_bytes::<32>())),
            "0xc7d8c89ef4000fccfa858df52763251bcee53da3e7460189b1772c71668a03c3"
        );
    }

    #[test]
    fn two_token_shared_decode_matches_mainnet() {
        // Verified DAI/WETH sharedCash word (scrape_reth test vector).
        let raw = U256::from_be_slice(
            &hex::decode("017954da0000000000027265e31b261e7bac000000000d7ab9cd751fcbfa74b5")
                .expect("hex"),
        );
        let (cash_a, cash_b, _) = decode_two_token_shared(raw);
        assert_eq!(cash_a, 63654655540344622052533u128);
        assert_eq!(cash_b, 45136732546133818284u128);
    }

    #[test]
    fn two_token_swap_fee_decode() {
        // Verified mainnet _miscData words (slot 8) for two_token WeightedPool2Tokens.
        let usdc_weth =
            U256::from_str_radix("2aa1efb94e00031dea44ef4b04e568127ae", 16).expect("hex");
        assert_eq!(decode_two_token_swap_fee(usdc_weth), 3_000_000_000_000_000); // 0.3%
        let dai_weth = U256::from_str_radix("71afd498d00037b6a44f0e3046cd41039d", 16).expect("hex");
        assert_eq!(decode_two_token_swap_fee(dai_weth), 500_000_000_000_000); // 0.05%

        assert!(is_plausible_swap_fee(3_000_000_000_000_000));
        assert!(is_plausible_swap_fee(500_000_000_000_000));
        assert!(!is_plausible_swap_fee(0)); // two_token slot 7 reads 0 — rejected
        assert!(!is_plausible_swap_fee(u64::MAX));
    }

    #[test]
    fn packed_balance_decode() {
        let cash = U256::from(500u64);
        let managed = U256::from(200u64) << 112;
        let block = U256::from(99u64) << 224;
        let (c, m, b) = decode_packed_balance(block | managed | cash);
        assert_eq!((c, m, b), (500, 200, 99));
    }
}
