//! ERC20 balance storage slot computation.
//!
//! Standard Solidity `mapping(address => uint256)` at slot N stores
//! `balances[holder]` at `keccak256(abi.encode(holder, N))`.
//!
//! Most ERC20s (OpenZeppelin) use slot 0. Known exceptions are hardcoded.

use alloy_primitives::{address, keccak256, Address, B256, U256};
use alloy_sol_types::SolValue;

/// Known tokens with non-standard balance mapping slots.
const SLOT_OVERRIDES: &[(Address, u64)] = &[
    // USDT — slot 2
    (address!("dAC17F958D2ee523a2206206994597C13D831ec7"), 2),
    // WETH9 — slot 3
    (address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), 3),
];

/// Compute the storage slot for `balances[holder]` in an ERC20 contract.
///
/// Uses the standard mapping slot (0) unless the token has a known override.
pub fn balance_storage_slot(token: Address, holder: Address) -> B256 {
    let mapping_slot = slot_for_token(token);
    compute_mapping_slot(holder, mapping_slot)
}

/// Look up the balance mapping slot for a token. Returns 0 for standard tokens.
fn slot_for_token(token: Address) -> u64 {
    for &(addr, slot) in SLOT_OVERRIDES {
        if addr == token {
            return slot;
        }
    }
    0
}

/// `keccak256(abi.encode(key, mapping_slot))`
fn compute_mapping_slot(key: Address, mapping_slot: u64) -> B256 {
    let encoded = (key, U256::from(mapping_slot)).abi_encode();
    keccak256(&encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn standard_token_uses_slot_0() {
        let token = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"); // USDC
        let holder = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let slot = balance_storage_slot(token, holder);
        // Should be keccak256(abi.encode(holder, 0))
        let expected = compute_mapping_slot(holder, 0);
        assert_eq!(slot, expected);
    }

    #[test]
    fn usdt_uses_slot_2() {
        let usdt = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
        let holder = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let slot = balance_storage_slot(usdt, holder);
        let expected = compute_mapping_slot(holder, 2);
        assert_eq!(slot, expected);
    }

    #[test]
    fn weth_uses_slot_3() {
        let weth = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        let holder = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
        let slot = balance_storage_slot(weth, holder);
        let expected = compute_mapping_slot(holder, 3);
        assert_eq!(slot, expected);
    }
}
