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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    pub fn as_pool_id(&self) -> Option<[u8; 32]> {
        match self {
            PoolIdentifier::Address(_) => None,
            PoolIdentifier::PoolId(id) => Some(*id),
        }
    }
}

/// Protocol type
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Protocol {
    UniswapV2,
    UniswapV3,
    UniswapV4,
}

/// Update type - which event triggered this update
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UpdateType {
    Swap,
    Mint,
    Burn,
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
/// Backward compatibility notes:
/// - Legacy variants (`BeginBlock`, `PoolUpdate`, `EndBlock`) are retained.
/// - V2 sequenced variants add `stream_seq` and explicit reorg boundaries.
/// - Producers should emit V2 variants; consumers may accept both during migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Update the pool whitelist
    UpdateWhitelist(WhitelistUpdate),

    /// Legacy block boundary start (v1)
    BeginBlock {
        block_number: u64,
        block_timestamp: u64,
        /// If true, this block's events are reverts (from ChainReorged or ChainReverted)
        is_revert: bool,
    },

    /// Legacy pool update (v1)
    PoolUpdate(PoolUpdateMessage),

    /// Legacy block boundary end (v1)
    EndBlock {
        block_number: u64,
        /// Number of pool updates sent for this block (for validation)
        num_updates: u64,
    },

    /// Heartbeat / keepalive
    Ping,
    Pong,

    /// V2 block boundary start with monotonic stream sequence.
    BeginBlockV2 {
        stream_seq: u64,
        block_number: u64,
        block_timestamp: u64,
        is_revert: bool,
    },

    /// V2 pool update wrapper with monotonic stream sequence.
    PoolUpdateV2 {
        stream_seq: u64,
        event: PoolUpdateMessage,
    },

    /// V2 block boundary end with monotonic stream sequence.
    EndBlockV2 {
        stream_seq: u64,
        block_number: u64,
        num_updates: u64,
    },

    /// Reorg boundary: emitted exactly once when a reorg batch starts.
    ReorgStart {
        stream_seq: u64,
        old_range: ReorgRange,
        new_range: ReorgRange,
    },

    /// Reorg boundary: emitted exactly once after the final EndBlock for that reorg batch.
    ReorgComplete {
        stream_seq: u64,
        final_tip_block: u64,
        /// Pools that require slot0 resync after the reorg.
        ///
        /// Emitted deterministically from reverted V3/V4 swap events.
        slot0_resync_required: Vec<PoolIdentifier>,
    },
}

impl ControlMessage {
    /// Returns stream sequence for V2-sequenced messages.
    pub fn stream_seq(&self) -> Option<u64> {
        match self {
            ControlMessage::BeginBlockV2 { stream_seq, .. }
            | ControlMessage::PoolUpdateV2 { stream_seq, .. }
            | ControlMessage::EndBlockV2 { stream_seq, .. }
            | ControlMessage::ReorgStart { stream_seq, .. }
            | ControlMessage::ReorgComplete { stream_seq, .. } => Some(*stream_seq),
            ControlMessage::UpdateWhitelist(_)
            | ControlMessage::BeginBlock { .. }
            | ControlMessage::PoolUpdate(_)
            | ControlMessage::EndBlock { .. }
            | ControlMessage::Ping
            | ControlMessage::Pong => None,
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
    fn test_control_message_stream_seq_v2() {
        let msg = ControlMessage::BeginBlockV2 {
            stream_seq: 42,
            block_number: 1000,
            block_timestamp: 123,
            is_revert: false,
        };

        assert_eq!(msg.stream_seq(), Some(42));
    }

    #[test]
    fn test_control_message_stream_seq_v1_none() {
        let msg = ControlMessage::BeginBlock {
            block_number: 1000,
            block_timestamp: 123,
            is_revert: false,
        };

        assert_eq!(msg.stream_seq(), None);
    }

    #[test]
    fn test_reorg_complete_roundtrip() {
        let msg = ControlMessage::ReorgComplete {
            stream_seq: 7,
            final_tip_block: 12345,
            slot0_resync_required: vec![PoolIdentifier::PoolId([1u8; 32])],
        };

        let encoded = bincode::serialize(&msg).expect("serialize");
        let decoded: ControlMessage = bincode::deserialize(&encoded).expect("deserialize");

        match decoded {
            ControlMessage::ReorgComplete {
                stream_seq,
                final_tip_block,
                slot0_resync_required,
            } => {
                assert_eq!(stream_seq, 7);
                assert_eq!(final_tip_block, 12345);
                assert_eq!(slot0_resync_required.len(), 1);
            }
            other => panic!("unexpected decoded variant: {other:?}"),
        }
    }
}
