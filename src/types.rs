// Pool State Update Types
//
// This module defines all message types sent over Unix socket from ExEx to Orderbook Engine

use alloy_primitives::{Address, I256, U256};
use serde::{Deserialize, Serialize};

/// Main envelope for all pool update messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolUpdateMessage {
    /// Pool identifier (contract address for V2/V3, poolId for V4)
    pub pool_id: PoolIdentifier,

    /// Protocol version
    pub protocol: Protocol,

    /// Type of update
    pub update_type: UpdateType,

    /// Block information
    pub block_number: u64,
    pub block_timestamp: u64,

    /// Transaction position
    pub tx_index: u64,
    pub log_index: u64,

    /// Whether this is a revert (due to chain reorg)
    /// If true, the consumer should apply the inverse of this update
    pub is_revert: bool,

    /// The actual update data
    pub update: PoolUpdate,
}

/// Pool identifier - can be address (V2/V3) or bytes32 (V4)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoolIdentifier {
    Address(Address),
    PoolId([u8; 32]), // V4 uses bytes32 poolId
}

impl PoolIdentifier {
    pub fn as_address(&self) -> Option<Address> {
        match self {
            PoolIdentifier::Address(addr) => Some(*addr),
            PoolIdentifier::PoolId(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_pool_id(&self) -> Option<[u8; 32]> {
        match self {
            PoolIdentifier::Address(_) => None,
            PoolIdentifier::PoolId(id) => Some(*id),
        }
    }
}

/// Protocol type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Protocol {
    UniswapV2,
    UniswapV3,
    UniswapV4,
    Ekubo,
    CurveStable,
    CurveTwoCrypto,
    CurveTricrypto,
    BalancerV2Weighted,
    Fluid,
}

/// Update type - which event triggered this update
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UpdateType {
    Swap,
    Mint,
    Burn,
}

/// Slot0-like post-state shared by swap and reorg-epilogue messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Slot0State {
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
}

/// Full Fluid reserve snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FluidState {
    pub col_token0_real: u128,
    pub col_token1_real: u128,
    pub col_token0_imaginary: u128,
    pub col_token1_imaginary: u128,
    pub debt_token0_real: u128,
    pub debt_token1_real: u128,
    pub debt_token0_imaginary: u128,
    pub debt_token1_imaginary: u128,
    pub center_price: u128,
    pub fee: u128,
}

/// Pool update data - enum of all possible update types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolUpdate {
    /// V2 Swap Update (reserve deltas - one positive, one negative)
    V2Swap { amount0: I256, amount1: I256 },

    /// V2 Liquidity Update (Mint or Burn)
    /// Positive amounts for mint, negative amounts for burn
    V2Liquidity { amount0: I256, amount1: I256 },

    /// V3 Swap Update (sqrtPriceX96, liquidity, tick)
    V3Swap {
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },

    /// V3 Liquidity Update (Mint or Burn)
    V3Liquidity {
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128, // Positive for mint, negative for burn
    },

    /// V4 Swap Update (same as V3 but from singleton contract)
    V4Swap {
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },

    /// V4 Liquidity Update (Mint or Burn from singleton)
    V4Liquidity {
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
    },

    /// Ekubo Swap Update (from anonymous log0 on Core contract).
    ///
    /// sqrtRatio is Ekubo's native uint96 stored as U256 — NOT Q64.96.
    /// Downstream Ekubo swap math reads it as u128.
    EkuboSwap {
        sqrt_ratio: U256,
        liquidity: u128,
        tick: i32,
    },

    /// Ekubo Liquidity Update (PositionUpdated event).
    ///
    /// Unlike V3/V4, Ekubo does not emit separate Mint/Burn events.
    /// PositionUpdated carries tick bounds (packed in positionId),
    /// `liquidityDelta`, and the full post-state (sqrtRatio, tick, liquidity).
    EkuboLiquidity {
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        /// Post-state from stateAfter — Ekubo native uint96, NOT Q64.96.
        sqrt_ratio: U256,
        liquidity: u128,
        tick: i32,
    },

    /// Legacy Curve StableSwap-NG delta swap update.
    ///
    /// Hard cutover: the producer now emits full post-state via `CurveLiquidity`
    /// even for TokenExchange events. This variant remains only so the mirrored
    /// socket enums stay source-compatible during the cutover.
    CurveSwap {
        sold_id: u8,
        tokens_sold: u128,
        bought_id: u8,
        tokens_bought: u128,
    },

    /// Curve StableSwap-NG full post-state update.
    /// Used for swaps and liquidity events so the arena never reconstructs
    /// stable pool balances locally.
    CurveLiquidity {
        effective_balances: Vec<u128>,
        fee: u64,
        offpeg_fee_multiplier: u64,
        initial_a: u64,
        future_a: u64,
        initial_a_time: u64,
        future_a_time: u64,
    },

    /// Curve StableSwap-NG RampA event.
    CurveRampA {
        initial_a: u64,
        future_a: u64,
        initial_a_time: u64,
        future_a_time: u64,
    },

    /// Curve StableSwap-NG ApplyNewFee event.
    CurveFeeUpdate {
        fee: u64,
        offpeg_fee_multiplier: u64,
    },

    /// Curve TwoCryptoNG full post-state update.
    /// Used for swaps and liquidity events so the arena never replays
    /// TwoCrypto balances locally.
    TwoCryptoState {
        balances: [u128; 2],
        price_scale: U256,
        d: U256,
    },

    /// Curve TwoCryptoNG RampAgamma event.
    TwoCryptoRampAgamma {
        initial_a: u64,
        future_a: u64,
        initial_gamma: u128,
        future_gamma: u128,
        initial_time: u64,
        future_time: u64,
    },

    /// Curve TwoCryptoNG NewParameters event.
    TwoCryptoNewParameters {
        mid_fee: u64,
        out_fee: u64,
        fee_gamma: u128,
    },

    /// Curve TricryptoNG full post-state update.
    /// Used for swaps and liquidity events so the arena never replays
    /// Tricrypto balances locally.
    TricryptoState {
        balances: [u128; 3],
        /// Packed price_scale: ps[0] in lower 128, ps[1] in upper 128.
        packed_price_scale: U256,
        d: U256,
    },

    /// Curve TricryptoNG RampAgamma event.
    TricryptoRampAgamma {
        initial_a: u64,
        future_a: u64,
        initial_gamma: u128,
        future_gamma: u128,
        initial_time: u64,
        future_time: u64,
    },

    /// Curve TricryptoNG NewParameters event.
    TricryptoNewParameters {
        mid_fee: u64,
        out_fee: u64,
        fee_gamma: u128,
    },

    /// Balancer V2 Vault Swap event.
    /// tokenIn/tokenOut identify which pair within the multi-token pool was swapped.
    /// amountIn/amountOut are raw token amounts (not scaled to 18 dec).
    BalancerSwap {
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        amount_out: U256,
    },

    /// Balancer V2 PoolBalanceChanged (join/exit).
    /// Signed deltas per token (positive = entering pool, negative = leaving).
    /// `tokens` is parallel to `deltas` (Vault event order); apply matches tokens
    /// to the pool's stored order rather than trusting positional index.
    BalancerLiquidity {
        tokens: Vec<Address>,
        deltas: Vec<i128>,
    },

    /// Balancer V2 SwapFeePercentageChanged event.
    BalancerFeeUpdate { swap_fee_percentage: u64 },

    /// Fluid DEX full reserve snapshot.
    ///
    /// Decoded from 8 storage slots post-`LogOperate`. Contains the
    /// complete reserve state — no further RPC calls needed by the arena.
    /// All reserve values in 1e12 decimals (resolver format).
    FluidState { state: FluidState },
}

/// Reorg-epilogue-only canonical state updates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReorgEpilogueUpdate {
    /// Definitive final slot0 state read from storage after a reorg settles.
    Slot0Final {
        pool_id: PoolIdentifier,
        protocol: Protocol,
        state: Slot0State,
    },

    /// Definitive final Fluid reserve state for pools not covered by replayed
    /// new-chain block updates.
    FluidStateFinal {
        pool_id: PoolIdentifier,
        state: FluidState,
    },
}

/// Token metadata from the rich whitelist.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenMetadata {
    pub address: Address,
    pub decimals: u8,
}

/// Pool metadata from whitelist
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMetadata {
    pub pool_id: PoolIdentifier,
    pub token0: Address,
    pub token1: Address,
    pub protocol: Protocol,
    pub factory: Address,

    /// V3/V4 specific fields
    pub tick_spacing: Option<i32>,
    pub fee: Option<u32>,

    /// Token decimals, sourced from the rich (`.full`) whitelist (ITE-16).
    /// `None` when parsed from the legacy minimal/address-only whitelist; arena
    /// hydration must skip pools whose decimals are unknown (data-integrity rule).
    pub token0_decimals: Option<u8>,
    pub token1_decimals: Option<u8>,

    /// Additional token metadata for multi-token pools. `token0` and `token1`
    /// remain the first two coins; this vector starts at coin index 2.
    #[serde(default)]
    pub extra_tokens: Vec<TokenMetadata>,

    /// Curve TwoCrypto storage-layout version from whitelist `additional_data.version`.
    /// `None` means the default v2.1.x layout; `Some("v2.0.0")` uses the legacy slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twocrypto_version: Option<String>,

    /// Ekubo fee: 0.64 fixed-point. Required for Ekubo arena hydration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ekubo_fee: Option<u64>,

    /// Ekubo PoolConfig type_config (packed u32). Required for Ekubo arena hydration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ekubo_type_config: Option<u32>,

    /// Balancer V2 normalized weights (1e18 scale), ordered token0, token1, then
    /// `extra_tokens`. Immutable pool bytecode (not in storage), so sourced from
    /// whitelist `additional_data.weights`. Required for Balancer arena hydration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_weights: Option<Vec<u64>>,

    /// Balancer V2 swap fee (1e18 scale) from whitelist `additional_data.swap_fee`.
    /// The fee has no stable contract storage slot across pool implementations
    /// (WeightedPool2Tokens packs it in `_miscData`; others use a plain uint256), so
    /// the resolved whitelist value is authoritative for arena hydration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_swap_fee: Option<u64>,

    /// Balancer V2 weighted-pool implementation version from whitelist
    /// `additional_data.version` ("v1", "2tokens", "v2", "v3", ...), classified at
    /// DB ingestion by matching the fee getter against each candidate storage slot.
    /// Identifies which slot holds the swap fee (see
    /// `balancer_storage::fee_layout_for_version`). `None` = unknown layout: the
    /// published `balancer_swap_fee` is then the only trusted fee source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balancer_version: Option<String>,
}

/// Whitelist control message sent from dynamicWhitelist to ExEx
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistUpdate {
    pub chain: String,
    pub generated_at: String,
    pub pools: Vec<PoolMetadata>,
}

/// Compact block-range summary used by reorg boundary messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorgRange {
    pub first_block: Option<u64>,
    pub last_block: Option<u64>,
    pub block_count: u64,
}

/// Control message types for socket communication.
///
/// V1 legacy variants were removed after cutover.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Update the pool whitelist
    UpdateWhitelist(WhitelistUpdate),

    /// Block boundary start with monotonic stream sequence.
    BeginBlock {
        stream_seq: u64,
        block_number: u64,
        block_timestamp: u64,
        /// EIP-1559 base fee in wei. Always present post-London.
        base_fee_per_gas: u64,
        /// If true, this block's events are reverts (from ChainReorged or ChainReverted)
        is_revert: bool,
    },

    /// Pool update wrapper with monotonic stream sequence.
    PoolUpdate {
        stream_seq: u64,
        event: PoolUpdateMessage,
    },

    /// Block boundary end with monotonic stream sequence.
    EndBlock {
        stream_seq: u64,
        block_number: u64,
        /// Number of pool updates sent for this block (for validation)
        num_updates: u64,
    },

    /// Heartbeat / keepalive
    Ping,
    Pong,

    /// Reorg boundary: emitted exactly once when a reorg batch starts.
    ReorgStart {
        stream_seq: u64,
        old_range: ReorgRange,
        new_range: ReorgRange,
    },

    /// Reorg epilogue update: emitted after replayed blocks, outside any
    /// BeginBlock/EndBlock envelope, while the reorg is still open.
    ReorgEpilogue {
        stream_seq: u64,
        final_tip_block: u64,
        final_tip_timestamp: u64,
        update: ReorgEpilogueUpdate,
    },

    /// Reorg boundary: emitted exactly once after all epilogue updates.
    ReorgComplete {
        stream_seq: u64,
        final_tip_block: u64,
    },
}

impl ControlMessage {
    /// Returns stream sequence for sequenced messages.
    #[allow(dead_code)]
    pub fn stream_seq(&self) -> Option<u64> {
        match self {
            ControlMessage::BeginBlock { stream_seq, .. }
            | ControlMessage::PoolUpdate { stream_seq, .. }
            | ControlMessage::EndBlock { stream_seq, .. }
            | ControlMessage::ReorgStart { stream_seq, .. }
            | ControlMessage::ReorgEpilogue { stream_seq, .. }
            | ControlMessage::ReorgComplete { stream_seq, .. } => Some(*stream_seq),
            ControlMessage::UpdateWhitelist(_) | ControlMessage::Ping | ControlMessage::Pong => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_identifier_serialization() {
        let addr = PoolIdentifier::Address(Address::ZERO);
        let serialized = bincode::serialize(&addr).unwrap();
        let deserialized: PoolIdentifier = bincode::deserialize(&serialized).unwrap();
        assert!(matches!(deserialized, PoolIdentifier::Address(_)));
    }

    #[test]
    fn test_v4_pool_id() {
        let pool_id = [0u8; 32];
        let id = PoolIdentifier::PoolId(pool_id);
        assert_eq!(id.as_pool_id(), Some(pool_id));
        assert_eq!(id.as_address(), None);
    }

    #[test]
    fn test_control_message_stream_seq() {
        let msg = ControlMessage::BeginBlock {
            stream_seq: 42,
            block_number: 1000,
            block_timestamp: 123,
            base_fee_per_gas: 1_000_000_000,
            is_revert: false,
        };

        assert_eq!(msg.stream_seq(), Some(42));
    }

    #[test]
    fn test_reorg_complete_roundtrip() {
        let msg = ControlMessage::ReorgComplete {
            stream_seq: 7,
            final_tip_block: 12345,
        };

        let encoded = bincode::serialize(&msg).expect("serialize");
        let decoded: ControlMessage = bincode::deserialize(&encoded).expect("deserialize");

        match decoded {
            ControlMessage::ReorgComplete {
                stream_seq,
                final_tip_block,
            } => {
                assert_eq!(stream_seq, 7);
                assert_eq!(final_tip_block, 12345);
            }
            other => panic!("unexpected decoded variant: {other:?}"),
        }
    }
}
