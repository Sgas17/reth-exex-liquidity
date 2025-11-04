// Event Decoders for Uniswap V2/V3/V4
//
// This module defines all liquidity events and provides decoding logic

use alloy_primitives::{Address, Log, U256};
use alloy_sol_types::{sol, SolEvent};

// ============================================================================
// UNISWAP V2 EVENTS
// ============================================================================
// NOTE: Event names in sol! macro MUST match on-chain names for signature calculation
// All Uniswap events are simply named "Swap", "Mint", "Burn" - not "UniswapV2Swap", etc.

mod v2 {
    use super::*;

    sol! {
        /// V2 Swap - event name MUST be "Swap" to match on-chain signature
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );

        /// V2 Mint - event name MUST be "Mint" to match on-chain signature
        #[derive(Debug)]
        event Mint(
            address indexed sender,
            uint256 amount0,
            uint256 amount1
        );

        /// V2 Burn - event name MUST be "Burn" to match on-chain signature
        #[derive(Debug)]
        event Burn(
            address indexed sender,
            uint256 amount0,
            uint256 amount1,
            address indexed to
        );
    }
}

// Re-export with namespaced names to avoid conflicts
use v2::{Burn as UniswapV2Burn, Mint as UniswapV2Mint, Swap as UniswapV2Swap};

// ============================================================================
// UNISWAP V3 EVENTS
// ============================================================================

mod v3 {
    use super::*;

    sol! {
        /// V3 Swap - event name MUST be "Swap" to match on-chain signature
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            address indexed recipient,
            int256 amount0,
            int256 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );

        /// V3 Mint - event name MUST be "Mint" to match on-chain signature
        #[derive(Debug)]
        event Mint(
            address sender,
            address indexed owner,
            int24 indexed tickLower,
            int24 indexed tickUpper,
            uint128 amount,
            uint256 amount0,
            uint256 amount1
        );

        /// V3 Burn - event name MUST be "Burn" to match on-chain signature
        #[derive(Debug)]
        event Burn(
            address indexed owner,
            int24 indexed tickLower,
            int24 indexed tickUpper,
            uint128 amount,
            uint256 amount0,
            uint256 amount1
        );
    }
}

// Re-export with namespaced names to avoid conflicts
use v3::{Burn as UniswapV3Burn, Mint as UniswapV3Mint, Swap as UniswapV3Swap};

// ============================================================================
// UNISWAP V4 EVENTS (from PoolManager singleton)
// ============================================================================

mod v4 {
    use super::*;

    sol! {
        /// V4 Swap - event name MUST be "Swap" to match on-chain signature
        /// (includes poolId as first indexed parameter)
        #[derive(Debug)]
        event Swap(
            bytes32 indexed poolId,
            address indexed sender,
            int128 amount0,
            int128 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );

        /// V4 ModifyLiquidity - replaces separate Mint/Burn
        /// liquidityDelta is positive for mint, negative for burn
        #[derive(Debug)]
        event ModifyLiquidity(
            bytes32 indexed poolId,
            address indexed sender,
            int24 tickLower,
            int24 tickUpper,
            int256 liquidityDelta
        );
    }
}

// Re-export with namespaced names
use v4::{ModifyLiquidity as UniswapV4ModifyLiquidity, Swap as UniswapV4Swap};

// ============================================================================
// EVENT DECODING LOGIC
// ============================================================================

/// Decoded event with source information
#[derive(Debug, Clone)]
pub enum DecodedEvent {
    V2Swap {
        pool: Address,
        amount0_in: U256,
        amount1_in: U256,
        amount0_out: U256,
        amount1_out: U256,
    },
    V2Mint {
        pool: Address,
        amount0: U256,
        amount1: U256,
    },
    V2Burn {
        pool: Address,
        amount0: U256,
        amount1: U256,
    },
    V3Swap {
        pool: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },
    V3Mint {
        pool: Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
    },
    V3Burn {
        pool: Address,
        tick_lower: i32,
        tick_upper: i32,
        amount: u128,
    },
    V4Swap {
        pool_id: [u8; 32],
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },
    V4ModifyLiquidity {
        pool_id: [u8; 32],
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
    },
}

/// Try to decode a log as any supported event type
pub fn decode_log(log: &Log) -> Option<DecodedEvent> {
    let pool = log.address;

    // Try V2 events
    if let Ok(event) = UniswapV2Swap::decode_log_data(&log.data) {
        return Some(DecodedEvent::V2Swap {
            pool,
            amount0_in: event.amount0In,
            amount1_in: event.amount1In,
            amount0_out: event.amount0Out,
            amount1_out: event.amount1Out,
        });
    }

    if let Ok(event) = UniswapV2Mint::decode_log_data(&log.data) {
        return Some(DecodedEvent::V2Mint {
            pool,
            amount0: event.amount0,
            amount1: event.amount1,
        });
    }

    if let Ok(event) = UniswapV2Burn::decode_log_data(&log.data) {
        return Some(DecodedEvent::V2Burn {
            pool,
            amount0: event.amount0,
            amount1: event.amount1,
        });
    }

    // Try V3 events
    if let Ok(event) = UniswapV3Swap::decode_log_data(&log.data) {
        return Some(DecodedEvent::V3Swap {
            pool,
            sqrt_price_x96: U256::from(event.sqrtPriceX96),
            liquidity: event.liquidity,
            tick: event.tick.as_i32(),
        });
    }

    if let Ok(event) = UniswapV3Mint::decode_log_data(&log.data) {
        return Some(DecodedEvent::V3Mint {
            pool,
            tick_lower: event.tickLower.as_i32(),
            tick_upper: event.tickUpper.as_i32(),
            amount: event.amount,
        });
    }

    if let Ok(event) = UniswapV3Burn::decode_log_data(&log.data) {
        return Some(DecodedEvent::V3Burn {
            pool,
            tick_lower: event.tickLower.as_i32(),
            tick_upper: event.tickUpper.as_i32(),
            amount: event.amount,
        });
    }

    // Try V4 events
    if let Ok(event) = UniswapV4Swap::decode_log_data(&log.data) {
        let pool_id: [u8; 32] = event.poolId.into();
        return Some(DecodedEvent::V4Swap {
            pool_id,
            sqrt_price_x96: U256::from(event.sqrtPriceX96),
            liquidity: event.liquidity,
            tick: event.tick.as_i32(),
        });
    }

    if let Ok(event) = UniswapV4ModifyLiquidity::decode_log_data(&log.data) {
        let pool_id: [u8; 32] = event.poolId.into();

        // Convert i256 to i128 (safe because liquidity deltas won't overflow i128)
        let liquidity_delta = if event.liquidityDelta >= alloy_primitives::I256::ZERO {
            // Positive value
            let abs = event.liquidityDelta.into_raw();
            // Take lower 128 bits for positive value
            i128::try_from(abs.saturating_to::<u128>()).unwrap_or(i128::MAX)
        } else {
            // Negative value
            let abs = (-event.liquidityDelta).into_raw();
            // Take lower 128 bits and negate
            -i128::try_from(abs.saturating_to::<u128>()).unwrap_or(i128::MAX)
        };

        return Some(DecodedEvent::V4ModifyLiquidity {
            pool_id,
            tick_lower: event.tickLower.as_i32(),
            tick_upper: event.tickUpper.as_i32(),
            liquidity_delta,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::LogData;

    #[test]
    fn test_event_signatures() {
        // V2 Event Signatures
        // Swap(address,uint256,uint256,uint256,uint256,address)
        assert_eq!(
            UniswapV2Swap::SIGNATURE_HASH.to_string(),
            "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
        );

        // Mint(address,uint256,uint256)
        assert_eq!(
            UniswapV2Mint::SIGNATURE_HASH.to_string(),
            "0x4c209b5fc8ad50758f13e2e1088ba56a560dff690a1c6fef26394f4c03821c4f"
        );

        // Burn(address,uint256,uint256,address)
        assert_eq!(
            UniswapV2Burn::SIGNATURE_HASH.to_string(),
            "0xdccd412f0b1252819cb1fd330b93224ca42612892bb3f4f789976e6d81936496"
        );

        // V3 Event Signatures
        // Swap(address,address,int256,int256,uint160,uint128,int24)
        assert_eq!(
            UniswapV3Swap::SIGNATURE_HASH.to_string(),
            "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
        );

        // Mint(address,address,int24,int24,uint128,uint256,uint256)
        assert_eq!(
            UniswapV3Mint::SIGNATURE_HASH.to_string(),
            "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
        );

        // Burn(address,int24,int24,uint128,uint256,uint256)
        assert_eq!(
            UniswapV3Burn::SIGNATURE_HASH.to_string(),
            "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"
        );

        // V4 Event Signatures
        // Swap(bytes32,address,int128,int128,uint160,uint128,int24)
        assert_eq!(
            UniswapV4Swap::SIGNATURE_HASH.to_string(),
            "0x9cd312f3503782cb1d29f4114896ca5405e9cf41adf9a23b76f74203d292296e"
        );

        // ModifyLiquidity(bytes32,address,int24,int24,int256)
        assert_eq!(
            UniswapV4ModifyLiquidity::SIGNATURE_HASH.to_string(),
            "0x541c041c2cce48e614b3de043c9280f06b6164c0a1741649e2de3c3d375f7974"
        );
    }

    #[test]
    fn test_decode_v2_swap() {
        // Create a minimal log with V2 Swap signature
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV2Swap::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // sender
                    alloy_primitives::B256::ZERO, // to
                ],
                vec![0u8; 160].into(), // 5 uint256 values
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V2Swap { .. })));
    }

    #[test]
    fn test_decode_v2_mint() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV2Mint::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // sender
                ],
                vec![0u8; 64].into(), // 2 uint256 values
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V2Mint { .. })));
    }

    #[test]
    fn test_decode_v2_burn() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV2Burn::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // sender
                    alloy_primitives::B256::ZERO, // to
                ],
                vec![0u8; 64].into(), // 2 uint256 values
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V2Burn { .. })));
    }

    #[test]
    fn test_decode_v3_swap() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV3Swap::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // sender
                    alloy_primitives::B256::ZERO, // recipient
                ],
                vec![0u8; 224].into(), // int256, int256, uint160, uint128, int24
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V3Swap { .. })));
    }

    #[test]
    fn test_decode_v3_mint() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV3Mint::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // owner
                    alloy_primitives::B256::ZERO, // tickLower
                    alloy_primitives::B256::ZERO, // tickUpper
                ],
                vec![0u8; 160].into(), // sender (32), amount (16), amount0 (32), amount1 (32)
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V3Mint { .. })));
    }

    #[test]
    fn test_decode_v3_burn() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV3Burn::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // owner
                    alloy_primitives::B256::ZERO, // tickLower
                    alloy_primitives::B256::ZERO, // tickUpper
                ],
                vec![0u8; 128].into(), // amount (16), amount0 (32), amount1 (32)
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V3Burn { .. })));
    }

    #[test]
    fn test_decode_v4_swap() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV4Swap::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // poolId
                    alloy_primitives::B256::ZERO, // sender
                ],
                vec![0u8; 224].into(), // int128, int128, uint160, uint128, int24
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V4Swap { .. })));
    }

    #[test]
    fn test_decode_v4_modify_liquidity() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    UniswapV4ModifyLiquidity::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // poolId
                    alloy_primitives::B256::ZERO, // sender
                ],
                vec![0u8; 128].into(), // int24, int24, int256
            ),
        };

        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V4ModifyLiquidity { .. })));
    }

    #[test]
    fn test_decode_unknown_event() {
        // Log with unknown signature
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![alloy_primitives::B256::from([0xff; 32])],
                vec![].into(),
            ),
        };

        let decoded = decode_log(&log);
        assert!(decoded.is_none());
    }

    #[test]
    fn test_decode_real_v3_swap_event() {
        // Real V3 Swap event from USDC/WETH 0.3% pool (0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640)
        // Transaction: 0x... (example from mainnet)
        // This is a real event structure to verify our decoder works correctly

        use alloy_primitives::{hex, B256};

        // Event signature for Swap(address,address,int256,int256,uint160,uint128,int24)
        let signature = UniswapV3Swap::SIGNATURE_HASH;

        // Verify signature matches expected
        assert_eq!(
            signature.to_string(),
            "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
        );

        // Create a realistic V3 Swap log structure
        let pool_address = alloy_primitives::address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");

        // Topics: [signature, sender, recipient]
        let topics = vec![
            signature,
            B256::from(hex!("000000000000000000000000e592427a0aece92de3edee1f18e0157c05861564")), // sender (router)
            B256::from(hex!("000000000000000000000000e592427a0aece92de3edee1f18e0157c05861564")), // recipient
        ];

        // Data: amount0, amount1, sqrtPriceX96, liquidity, tick (simplified example)
        let data = hex!(
            "0000000000000000000000000000000000000000000000000000000000000064" // amount0 (100 in simplified form)
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffce" // amount1 (-50 in two's complement)
            "00000000000000000000000000000001000000000000000000000000000000ff" // sqrtPriceX96
            "00000000000000000000000000000000000000000000000000000000deadbeef" // liquidity
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff8ad0" // tick (-30000 in two's complement)
        ).to_vec();

        let log = Log {
            address: pool_address,
            data: LogData::new_unchecked(topics, data.into()),
        };

        // Decode the event
        let decoded = decode_log(&log);

        // Verify it decoded successfully as V3Swap
        assert!(decoded.is_some(), "Failed to decode real V3 Swap event");

        match decoded.unwrap() {
            DecodedEvent::V3Swap { pool, sqrt_price_x96, liquidity, tick } => {
                assert_eq!(pool, pool_address);
                assert!(sqrt_price_x96 > U256::ZERO);
                assert!(liquidity > 0);
                // Tick should be negative in this example
                assert!(tick < 0);
            }
            other => panic!("Expected V3Swap, got {:?}", other),
        }
    }

    #[test]
    fn test_decode_real_v3_mint_event() {
        // Real V3 Mint event structure
        use alloy_primitives::{hex, B256};

        let signature = UniswapV3Mint::SIGNATURE_HASH;
        let pool_address = alloy_primitives::address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");

        // Topics: [signature, owner, tickLower, tickUpper]
        let topics = vec![
            signature,
            B256::from(hex!("000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88")), // owner (position manager)
            B256::from(hex!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff8ad0")), // tickLower (-30000)
            B256::from(hex!("0000000000000000000000000000000000000000000000000000000000007530")), // tickUpper (30000)
        ];

        // Data: sender, amount, amount0, amount1
        let data = hex!(
            "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88" // sender
            "00000000000000000000000000000000000000000000000000000000000f4240" // amount (1000000)
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000" // amount0
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000" // amount1
        ).to_vec();

        let log = Log {
            address: pool_address,
            data: LogData::new_unchecked(topics, data.into()),
        };

        let decoded = decode_log(&log);
        assert!(decoded.is_some(), "Failed to decode real V3 Mint event");

        match decoded.unwrap() {
            DecodedEvent::V3Mint { pool, tick_lower, tick_upper, amount } => {
                assert_eq!(pool, pool_address);
                assert_eq!(tick_lower, -30000);
                assert_eq!(tick_upper, 30000);
                assert!(amount > 0);
            }
            other => panic!("Expected V3Mint, got {:?}", other),
        }
    }
}
