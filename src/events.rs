// Event Decoders for Uniswap V2/V3/V4
//
// This module defines all liquidity events and provides decoding logic

use alloy_primitives::{Address, Log, U256};
use alloy_sol_types::{sol, SolEvent};

// ============================================================================
// UNISWAP V2 EVENTS
// ============================================================================

sol! {
    /// V2 Swap event
    #[derive(Debug)]
    event UniswapV2Swap(
        address indexed sender,
        uint256 amount0In,
        uint256 amount1In,
        uint256 amount0Out,
        uint256 amount1Out,
        address indexed to
    );

    /// V2 Mint event
    #[derive(Debug)]
    event UniswapV2Mint(
        address indexed sender,
        uint256 amount0,
        uint256 amount1
    );

    /// V2 Burn event
    #[derive(Debug)]
    event UniswapV2Burn(
        address indexed sender,
        uint256 amount0,
        uint256 amount1,
        address indexed to
    );
}

// ============================================================================
// UNISWAP V3 EVENTS
// ============================================================================

sol! {
    /// V3 Swap event
    #[derive(Debug)]
    event UniswapV3Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );

    /// V3 Mint event
    #[derive(Debug)]
    event UniswapV3Mint(
        address sender,
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );

    /// V3 Burn event
    #[derive(Debug)]
    event UniswapV3Burn(
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );
}

// ============================================================================
// UNISWAP V4 EVENTS (from PoolManager singleton)
// ============================================================================

sol! {
    /// V4 Swap event (includes poolId as first indexed parameter)
    #[derive(Debug)]
    event UniswapV4Swap(
        bytes32 indexed poolId,
        address indexed sender,
        int128 amount0,
        int128 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );

    /// V4 ModifyLiquidity event (replaces separate Mint/Burn)
    /// liquidityDelta is positive for mint, negative for burn
    #[derive(Debug)]
    event UniswapV4ModifyLiquidity(
        bytes32 indexed poolId,
        address indexed sender,
        int24 tickLower,
        int24 tickUpper,
        int256 liquidityDelta
    );
}

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
        // Verify event signatures match expected values
        assert_eq!(
            UniswapV2Swap::SIGNATURE_HASH.to_string(),
            "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
        );

        assert_eq!(
            UniswapV3Swap::SIGNATURE_HASH.to_string(),
            "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
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

        // Should decode successfully (even if data is dummy)
        let decoded = decode_log(&log);
        assert!(matches!(decoded, Some(DecodedEvent::V2Swap { .. })));
    }
}
