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

/// Control message types for socket communication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Update the pool whitelist
    UpdateWhitelist(WhitelistUpdate),

    /// Block boundary: Start of block processing
    /// Consumer should buffer all PoolUpdates until EndBlock
    BeginBlock {
        block_number: u64,
        block_timestamp: u64,
        /// If true, this block's events are reverts (from ChainReorged or ChainReverted)
        is_revert: bool,
    },

    /// Pool state update (can be forward or revert based on parent BeginBlock)
    PoolUpdate(PoolUpdateMessage),

    /// Block boundary: End of block processing
    /// Consumer should now apply all buffered updates atomically and recalculate paths
    EndBlock {
        block_number: u64,
        /// Number of pool updates sent for this block (for validation)
        num_updates: u64,
    },

    /// Heartbeat / keepalive
    Ping,
    Pong,
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
}
