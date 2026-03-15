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

// PancakeSwap V3 uses a Swap event with two extra trailing uint128 fields.
// Signature: Swap(address,address,int256,int256,uint160,uint128,int24,uint128,uint128)
mod v3_pancake {
    use super::*;

    sol! {
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            address indexed recipient,
            int256 amount0,
            int256 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick,
            uint128 protocolFeesToken0,
            uint128 protocolFeesToken1
        );
    }
}

use v3_pancake::Swap as PancakeV3Swap;

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
            int24 tick,
            uint24 fee
        );

        /// V4 ModifyLiquidity - replaces separate Mint/Burn
        /// liquidityDelta is positive for mint, negative for burn
        #[derive(Debug)]
        event ModifyLiquidity(
            bytes32 indexed poolId,
            address indexed sender,
            int24 tickLower,
            int24 tickUpper,
            int256 liquidityDelta,
            bytes32 salt
        );
    }
}

// Re-export with namespaced names
use v4::{ModifyLiquidity as UniswapV4ModifyLiquidity, Swap as UniswapV4Swap};

// ============================================================================
// FLUID DEX EVENTS (from Liquidity Layer singleton)
// ============================================================================

mod fluid {
    use super::*;

    sol! {
        /// LogOperate — emitted by the Fluid Liquidity Layer when any protocol
        /// (DEX pool, fToken, Vault) calls `operate()`.
        ///
        /// For DEX swaps the `user` is the Fluid pool address.
        /// We only need user + token as a "pool was touched" signal; the
        /// arena subscriber fetches fresh reserves from the DexReservesResolver.
        #[derive(Debug)]
        event LogOperate(
            address indexed user,
            address indexed token,
            int256 supplyAmount,
            int256 borrowAmount,
            address withdrawTo,
            address borrowTo,
            uint256 totalAmounts,
            uint256 exchangePricesAndConfig
        );
    }
}

// ============================================================================
// EKUBO EVENTS
// ============================================================================
// Ekubo Core (0x00000000000014aA86C5d3c41765bb24e11bd701) emits:
//   - Swaps: anonymous log0 (no topics), 116 bytes packed data
//   - PositionUpdated: standard event with signature

// ============================================================================
// CURVE STABLESWAP-NG EVENTS
// ============================================================================
// TokenExchange is emitted by each individual pool contract (not a singleton).
// AddLiquidity / RemoveLiquidity are handled by re-scraping balances.
// RampA and ApplyNewFee are rare parameter-change events.

mod curve {
    use super::*;

    sol! {
        /// TokenExchange(address buyer, int128 sold_id, uint256 tokens_sold, int128 bought_id, uint256 tokens_bought)
        #[derive(Debug)]
        event TokenExchange(
            address indexed buyer,
            int128 sold_id,
            uint256 tokens_sold,
            int128 bought_id,
            uint256 tokens_bought
        );

        /// AddLiquidity(address provider, uint256[] token_amounts, uint256[] fees, uint256 invariant, uint256 token_supply)
        /// NOTE: DynArray args in Vyper ABI are encoded as dynamic arrays.
        /// We only need the event signature for detection; balances are re-scraped.
        #[derive(Debug)]
        event AddLiquidity(
            address indexed provider,
            uint256[] token_amounts,
            uint256[] fees,
            uint256 invariant,
            uint256 token_supply
        );

        /// RemoveLiquidity(address provider, uint256[] token_amounts, uint256[] fees, uint256 token_supply)
        #[derive(Debug)]
        event RemoveLiquidity(
            address indexed provider,
            uint256[] token_amounts,
            uint256[] fees,
            uint256 token_supply
        );

        /// RemoveLiquidityOne(address provider, int128 token_id, uint256 token_amount, uint256 coin_amount, uint256 token_supply)
        #[derive(Debug)]
        event RemoveLiquidityOne(
            address indexed provider,
            int128 token_id,
            uint256 token_amount,
            uint256 coin_amount,
            uint256 token_supply
        );

        /// RemoveLiquidityImbalance(address provider, uint256[] token_amounts, uint256[] fees, uint256 invariant, uint256 token_supply)
        #[derive(Debug)]
        event RemoveLiquidityImbalance(
            address indexed provider,
            uint256[] token_amounts,
            uint256[] fees,
            uint256 invariant,
            uint256 token_supply
        );

        /// RampA(uint256 old_A, uint256 new_A, uint256 initial_time, uint256 future_time)
        #[derive(Debug)]
        event RampA(
            uint256 old_A,
            uint256 new_A,
            uint256 initial_time,
            uint256 future_time
        );

        /// ApplyNewFee(uint256 fee, uint256 offpeg_fee_multiplier)
        #[derive(Debug)]
        event ApplyNewFee(
            uint256 fee,
            uint256 offpeg_fee_multiplier
        );
    }
}

use fluid::LogOperate as FluidLogOperate;

use curve::{
    AddLiquidity as CurveAddLiquidity,
    ApplyNewFee as CurveApplyNewFee,
    RampA as CurveRampA,
    RemoveLiquidity as CurveRemoveLiquidity,
    RemoveLiquidityImbalance as CurveRemoveLiquidityImbalance,
    RemoveLiquidityOne as CurveRemoveLiquidityOne,
    TokenExchange as CurveTokenExchange,
};

// ============================================================================
// CURVE TWOCRYPTO-NG EVENTS
// ============================================================================
// TwoCryptoNG uses uint256 indices (not int128), and has additional fields.
// Event signatures are different from StableSwap-NG.

mod twocrypto {
    use super::*;

    sol! {
        /// TokenExchange(address indexed buyer, uint256 sold_id, uint256 tokens_sold,
        ///               uint256 bought_id, uint256 tokens_bought, uint256 fee, uint256 packed_price_scale)
        #[derive(Debug)]
        event TokenExchange(
            address indexed buyer,
            uint256 sold_id,
            uint256 tokens_sold,
            uint256 bought_id,
            uint256 tokens_bought,
            uint256 fee,
            uint256 packed_price_scale
        );

        /// AddLiquidity(address indexed provider, uint256[2] token_amounts,
        ///              uint256 fee, uint256 token_supply, uint256 packed_price_scale)
        #[derive(Debug)]
        event AddLiquidity(
            address indexed provider,
            uint256[2] token_amounts,
            uint256 fee,
            uint256 token_supply,
            uint256 packed_price_scale
        );

        /// RemoveLiquidity(address indexed provider, uint256[2] token_amounts, uint256 token_supply)
        #[derive(Debug)]
        event RemoveLiquidity(
            address indexed provider,
            uint256[2] token_amounts,
            uint256 token_supply
        );

        /// RemoveLiquidityOne(address indexed provider, uint256 token_id,
        ///                    uint256 token_amount, uint256 approx_fee, uint256 packed_price_scale)
        #[derive(Debug)]
        event RemoveLiquidityOne(
            address indexed provider,
            uint256 token_id,
            uint256 token_amount,
            uint256 approx_fee,
            uint256 packed_price_scale
        );

        /// NewParameters(uint256 mid_fee, uint256 out_fee, uint256 fee_gamma,
        ///               uint256 allowed_extra_profit, uint256 adjustment_step,
        ///               uint256 ma_time, uint256 xcp_profit_a)
        #[derive(Debug)]
        event NewParameters(
            uint256 mid_fee,
            uint256 out_fee,
            uint256 fee_gamma,
            uint256 allowed_extra_profit,
            uint256 adjustment_step,
            uint256 ma_time,
            uint256 xcp_profit_a
        );

        /// RampAgamma(uint256 initial_A, uint256 future_A, uint256 initial_gamma,
        ///            uint256 future_gamma, uint256 initial_time, uint256 future_time)
        #[derive(Debug)]
        event RampAgamma(
            uint256 initial_A,
            uint256 future_A,
            uint256 initial_gamma,
            uint256 future_gamma,
            uint256 initial_time,
            uint256 future_time
        );
    }
}

use twocrypto::{
    AddLiquidity as TwoCryptoAddLiquidity,
    NewParameters as TwoCryptoNewParameters,
    RampAgamma as TwoCryptoRampAgamma,
    RemoveLiquidity as TwoCryptoRemoveLiquidity,
    RemoveLiquidityOne as TwoCryptoRemoveLiquidityOne,
    TokenExchange as TwoCryptoTokenExchange,
};

mod ekubo {
    use super::*;

    sol! {
        /// PositionUpdated(address locker, bytes32 poolId, bytes32 positionId,
        ///                 int128 liquidityDelta, bytes32 balanceUpdate, bytes32 stateAfter)
        #[derive(Debug)]
        event PositionUpdated(
            address locker,
            bytes32 poolId,
            bytes32 positionId,
            int128 liquidityDelta,
            bytes32 balanceUpdate,
            bytes32 stateAfter
        );
    }
}

use ekubo::PositionUpdated as EkuboPositionUpdated;

/// Ekubo Core contract address on Ethereum mainnet.
pub const EKUBO_CORE: Address = Address::new([
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0xaA,
    0x86, 0xC5, 0xd3, 0xc4, 0x17, 0x65, 0xbB, 0x24,
    0xe1, 0x1b, 0xd7, 0x01,
]);

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
    /// Ekubo swap decoded from anonymous log0.
    EkuboSwap {
        pool_id: [u8; 32],
        /// Ekubo native uint96 sqrtRatio (NOT Q64.96).
        sqrt_ratio: U256,
        liquidity: u128,
        tick: i32,
    },
    /// Ekubo liquidity change from PositionUpdated event.
    /// Tick bounds extracted from positionId: salt(24B) | tickLower(4B) | tickUpper(4B).
    EkuboPositionUpdated {
        pool_id: [u8; 32],
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
        sqrt_ratio: U256,
        liquidity: u128,
        tick: i32,
    },
    /// Curve StableSwap-NG TokenExchange event.
    CurveSwap {
        pool: Address,
        sold_id: u8,
        tokens_sold: u128,
        bought_id: u8,
        tokens_bought: u128,
    },
    /// Curve liquidity event (AddLiquidity, RemoveLiquidity, etc).
    /// We don't decode the amounts — balances will be re-scraped from storage.
    CurveLiquidityChange {
        pool: Address,
    },
    /// Curve RampA parameter change.
    CurveRampA {
        pool: Address,
        old_a: u64,
        new_a: u64,
        initial_time: u64,
        future_time: u64,
    },
    /// Curve ApplyNewFee event.
    CurveApplyNewFee {
        pool: Address,
        fee: u64,
        offpeg_fee_multiplier: u64,
    },
    /// TwoCryptoNG TokenExchange event.
    TwoCryptoSwap {
        pool: Address,
        sold_id: u8,
        tokens_sold: u128,
        bought_id: u8,
        tokens_bought: u128,
        packed_price_scale: U256,
    },
    /// TwoCryptoNG liquidity event (AddLiquidity, RemoveLiquidity, RemoveLiquidityOne).
    TwoCryptoLiquidityChange {
        pool: Address,
    },
    /// TwoCryptoNG RampAgamma parameter change.
    TwoCryptoRampAgamma {
        pool: Address,
        initial_a: u64,
        future_a: u64,
        initial_gamma: u128,
        future_gamma: u128,
        initial_time: u64,
        future_time: u64,
    },
    /// TwoCryptoNG NewParameters event.
    TwoCryptoNewParameters {
        pool: Address,
        mid_fee: u64,
        out_fee: u64,
        fee_gamma: u128,
    },
    /// Fluid Liquidity Layer `LogOperate` — signals a tracked Fluid DEX
    /// pool changed reserves. `user` = pool address, `token` = asset involved.
    FluidOperate {
        pool: Address,
        token: Address,
    },
}

/// Check if a log is a Fluid `LogOperate` for a specific pool address
/// using only indexed topics — no ABI decoding required.
///
/// `LogOperate(address indexed user, address indexed token, ...)`
///   - topics[0] = event signature
///   - topics[1] = user (pool address, left-padded to 32 bytes)
///   - topics[2] = token
#[inline]
pub fn is_fluid_log_operate_for_pool(log: &Log, pool: &Address) -> bool {
    let topics = log.topics();
    topics.len() >= 2
        && topics[0] == FluidLogOperate::SIGNATURE_HASH
        && topics[1].as_slice()[12..] == pool.as_slice()[..]
}

/// Extract the pool address from a Fluid `LogOperate` log's indexed topic
/// without full ABI decoding. Returns `None` if the log isn't a `LogOperate`.
#[inline]
pub fn fluid_log_operate_pool(log: &Log) -> Option<Address> {
    let topics = log.topics();
    if topics.len() >= 2 && topics[0] == FluidLogOperate::SIGNATURE_HASH {
        Some(Address::from_slice(&topics[1].as_slice()[12..]))
    } else {
        None
    }
}

/// Try to decode a log as any supported event type
pub fn decode_log(log: &Log) -> Option<DecodedEvent> {
    let pool = log.address;

    // Log the signature we're trying to decode (for debugging)
    if let Some(sig) = log.topics().first() {
        use tracing::debug;
        debug!(
            "Attempting to decode log with signature: {:#x} from pool: {:?}",
            sig, pool
        );
    }

    // Try V2 events - using decode_log() to validate signature (topic[0])
    if let Ok(event) = UniswapV2Swap::decode_log(log) {
        return Some(DecodedEvent::V2Swap {
            pool,
            amount0_in: event.data.amount0In,
            amount1_in: event.data.amount1In,
            amount0_out: event.data.amount0Out,
            amount1_out: event.data.amount1Out,
        });
    }

    if let Ok(event) = UniswapV2Mint::decode_log(log) {
        return Some(DecodedEvent::V2Mint {
            pool,
            amount0: event.data.amount0,
            amount1: event.data.amount1,
        });
    }

    if let Ok(event) = UniswapV2Burn::decode_log(log) {
        return Some(DecodedEvent::V2Burn {
            pool,
            amount0: event.data.amount0,
            amount1: event.data.amount1,
        });
    }

    // Try V3 events - using decode_log() to validate signature (topic[0])
    if let Ok(event) = UniswapV3Swap::decode_log(log) {
        return Some(DecodedEvent::V3Swap {
            pool,
            sqrt_price_x96: U256::from(event.data.sqrtPriceX96),
            liquidity: event.data.liquidity,
            tick: event.data.tick.as_i32(),
        });
    }

    // PancakeSwap V3 swap variant with extra protocol fee fields.
    if let Ok(event) = PancakeV3Swap::decode_log(log) {
        return Some(DecodedEvent::V3Swap {
            pool,
            sqrt_price_x96: U256::from(event.data.sqrtPriceX96),
            liquidity: event.data.liquidity,
            tick: event.data.tick.as_i32(),
        });
    }

    if let Ok(event) = UniswapV3Mint::decode_log(log) {
        return Some(DecodedEvent::V3Mint {
            pool,
            tick_lower: event.data.tickLower.as_i32(),
            tick_upper: event.data.tickUpper.as_i32(),
            amount: event.data.amount,
        });
    }

    if let Ok(event) = UniswapV3Burn::decode_log(log) {
        return Some(DecodedEvent::V3Burn {
            pool,
            tick_lower: event.data.tickLower.as_i32(),
            tick_upper: event.data.tickUpper.as_i32(),
            amount: event.data.amount,
        });
    }

    // Try Fluid LogOperate - emitted by the Liquidity Layer singleton.
    // topics[0] = signature, topics[1] = user (pool), topics[2] = token
    if let Ok(event) = FluidLogOperate::decode_log(log) {
        let (_, user, token) = event.topics();
        return Some(DecodedEvent::FluidOperate {
            pool: Address(*user),
            token: Address(*token),
        });
    }

    // Try V4 events - poolId is indexed (in topics), not in data!
    // topics[0] = event signature, topics[1] = poolId (indexed), topics[2] = sender (indexed)
    // Must validate topic0 against the expected signature first — decode_log_data
    // only parses the data section and does NOT check the event signature.
    if log.topics().len() >= 3 {
        if log.topics()[0] == UniswapV4Swap::SIGNATURE_HASH {
            if let Ok(event) = UniswapV4Swap::decode_log_data(&log.data) {
                let pool_id: [u8; 32] = log.topics()[1].into();
                return Some(DecodedEvent::V4Swap {
                    pool_id,
                    sqrt_price_x96: U256::from(event.sqrtPriceX96),
                    liquidity: event.liquidity,
                    tick: event.tick.as_i32(),
                });
            }
        }

        if log.topics()[0] == UniswapV4ModifyLiquidity::SIGNATURE_HASH {
            if let Ok(event) = UniswapV4ModifyLiquidity::decode_log_data(&log.data) {
                let pool_id: [u8; 32] = log.topics()[1].into();

                // Convert i256 to i128 (safe because liquidity deltas won't overflow i128)
                let liquidity_delta = if event.liquidityDelta >= alloy_primitives::I256::ZERO {
                    let abs = event.liquidityDelta.into_raw();
                    i128::try_from(abs.saturating_to::<u128>()).unwrap_or(i128::MAX)
                } else {
                    let abs = (-event.liquidityDelta).into_raw();
                    -i128::try_from(abs.saturating_to::<u128>()).unwrap_or(i128::MAX)
                };

                return Some(DecodedEvent::V4ModifyLiquidity {
                    pool_id,
                    tick_lower: event.tickLower.as_i32(),
                    tick_upper: event.tickUpper.as_i32(),
                    liquidity_delta,
                });
            }
        }
    }

    // ── Curve StableSwap-NG events ───────────────────────────────────────
    // TokenExchange carries the swap data directly.
    // Liquidity events (Add/Remove/etc) just trigger a re-scrape.
    // RampA and ApplyNewFee are rare but must be tracked.

    if let Ok(event) = CurveTokenExchange::decode_log(log) {
        return Some(DecodedEvent::CurveSwap {
            pool,
            sold_id: event.data.sold_id as u8,
            tokens_sold: event.data.tokens_sold.saturating_to::<u128>(),
            bought_id: event.data.bought_id as u8,
            tokens_bought: event.data.tokens_bought.saturating_to::<u128>(),
        });
    }

    if let Ok(_event) = CurveAddLiquidity::decode_log(log) {
        return Some(DecodedEvent::CurveLiquidityChange { pool });
    }

    if let Ok(_event) = CurveRemoveLiquidity::decode_log(log) {
        return Some(DecodedEvent::CurveLiquidityChange { pool });
    }

    if let Ok(_event) = CurveRemoveLiquidityOne::decode_log(log) {
        return Some(DecodedEvent::CurveLiquidityChange { pool });
    }

    if let Ok(_event) = CurveRemoveLiquidityImbalance::decode_log(log) {
        return Some(DecodedEvent::CurveLiquidityChange { pool });
    }

    if let Ok(event) = CurveRampA::decode_log(log) {
        return Some(DecodedEvent::CurveRampA {
            pool,
            old_a: event.data.old_A.saturating_to::<u64>(),
            new_a: event.data.new_A.saturating_to::<u64>(),
            initial_time: event.data.initial_time.saturating_to::<u64>(),
            future_time: event.data.future_time.saturating_to::<u64>(),
        });
    }

    if let Ok(event) = CurveApplyNewFee::decode_log(log) {
        return Some(DecodedEvent::CurveApplyNewFee {
            pool,
            fee: event.data.fee.saturating_to::<u64>(),
            offpeg_fee_multiplier: event.data.offpeg_fee_multiplier.saturating_to::<u64>(),
        });
    }

    // ── Curve TwoCryptoNG events ─────────────────────────────────────────
    // Different event signatures from StableSwap-NG (uint256 indices, extra fields).

    if let Ok(event) = TwoCryptoTokenExchange::decode_log(log) {
        return Some(DecodedEvent::TwoCryptoSwap {
            pool,
            sold_id: event.data.sold_id.saturating_to::<u8>(),
            tokens_sold: event.data.tokens_sold.saturating_to::<u128>(),
            bought_id: event.data.bought_id.saturating_to::<u8>(),
            tokens_bought: event.data.tokens_bought.saturating_to::<u128>(),
            packed_price_scale: event.data.packed_price_scale,
        });
    }

    if let Ok(_event) = TwoCryptoAddLiquidity::decode_log(log) {
        return Some(DecodedEvent::TwoCryptoLiquidityChange { pool });
    }

    if let Ok(_event) = TwoCryptoRemoveLiquidity::decode_log(log) {
        return Some(DecodedEvent::TwoCryptoLiquidityChange { pool });
    }

    if let Ok(_event) = TwoCryptoRemoveLiquidityOne::decode_log(log) {
        return Some(DecodedEvent::TwoCryptoLiquidityChange { pool });
    }

    if let Ok(event) = TwoCryptoRampAgamma::decode_log(log) {
        return Some(DecodedEvent::TwoCryptoRampAgamma {
            pool,
            initial_a: event.data.initial_A.saturating_to::<u64>(),
            future_a: event.data.future_A.saturating_to::<u64>(),
            initial_gamma: event.data.initial_gamma.saturating_to::<u128>(),
            future_gamma: event.data.future_gamma.saturating_to::<u128>(),
            initial_time: event.data.initial_time.saturating_to::<u64>(),
            future_time: event.data.future_time.saturating_to::<u64>(),
        });
    }

    if let Ok(event) = TwoCryptoNewParameters::decode_log(log) {
        return Some(DecodedEvent::TwoCryptoNewParameters {
            pool,
            mid_fee: event.data.mid_fee.saturating_to::<u64>(),
            out_fee: event.data.out_fee.saturating_to::<u64>(),
            fee_gamma: event.data.fee_gamma.saturating_to::<u128>(),
        });
    }

    // ── Ekubo events ──────────────────────────────────────────────────────
    // Ekubo Core uses anonymous log0 for swaps and standard events for liquidity.

    if log.address == EKUBO_CORE {
        // Anonymous swap log0: no topics, exactly 116 bytes data.
        // Layout: locker(20) | poolId(32) | balanceUpdate(32) | stateAfter(32)
        if log.topics().is_empty() && log.data.data.len() == 116 {
            let data = &log.data.data;

            let mut pool_id = [0u8; 32];
            pool_id.copy_from_slice(&data[20..52]);

            // stateAfter (bytes 84..116): sqrtRatio(uint96) | tick(int32) | liquidity(uint128)
            let state = &data[84..116];
            // sqrtRatio: top 12 bytes (96 bits) of the 32-byte word
            let sqrt_ratio = U256::from_be_bytes::<32>({
                let mut buf = [0u8; 32];
                buf[20..32].copy_from_slice(&state[0..12]);
                buf
            });
            // tick: bytes 12..16 (int32, sign-extended)
            let tick = i32::from_be_bytes(state[12..16].try_into().unwrap());
            // liquidity: bytes 16..32 (uint128)
            let liquidity = u128::from_be_bytes(state[16..32].try_into().unwrap());

            return Some(DecodedEvent::EkuboSwap {
                pool_id,
                sqrt_ratio,
                liquidity,
                tick,
            });
        }

        // PositionUpdated: standard event with signature
        if !log.topics().is_empty() && log.topics()[0] == EkuboPositionUpdated::SIGNATURE_HASH {
            if let Ok(event) = EkuboPositionUpdated::decode_log_data(&log.data) {
                let pool_id: [u8; 32] = event.poolId.into();

                // Decode positionId: salt(24B) | tickLower(4B) | tickUpper(4B)
                let pos_bytes: [u8; 32] = event.positionId.into();
                let tick_lower =
                    i32::from_be_bytes(pos_bytes[24..28].try_into().unwrap());
                let tick_upper =
                    i32::from_be_bytes(pos_bytes[28..32].try_into().unwrap());

                // Decode stateAfter packed bytes32: sqrtRatio(12B) | tick(4B) | liquidity(16B)
                let state_bytes: [u8; 32] = event.stateAfter.into();
                let sqrt_ratio = U256::from_be_bytes::<32>({
                    let mut buf = [0u8; 32];
                    buf[20..32].copy_from_slice(&state_bytes[0..12]);
                    buf
                });
                let tick = i32::from_be_bytes(state_bytes[12..16].try_into().unwrap());
                let liquidity = u128::from_be_bytes(state_bytes[16..32].try_into().unwrap());

                return Some(DecodedEvent::EkuboPositionUpdated {
                    pool_id,
                    tick_lower,
                    tick_upper,
                    liquidity_delta: event.liquidityDelta,
                    sqrt_ratio,
                    liquidity,
                    tick,
                });
            }
        }
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

        // Pancake V3 Swap(address,address,int256,int256,uint160,uint128,int24,uint128,uint128)
        assert_eq!(
            PancakeV3Swap::SIGNATURE_HASH.to_string(),
            "0x19b47279256b2a23a1665c810c8d55a1758940ee09377d4f8d26497a3577dc83"
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
        // Swap(bytes32,address,int128,int128,uint160,uint128,int24,uint24)
        assert_eq!(
            UniswapV4Swap::SIGNATURE_HASH.to_string(),
            "0x40e9cecb9f5f1f1c5b9c97dec2917b7ee92e57ba5563708daca94dd84ad7112f"
        );

        // ModifyLiquidity(bytes32,address,int24,int24,int256)
        assert_eq!(
            UniswapV4ModifyLiquidity::SIGNATURE_HASH.to_string(),
            "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec"
        );

        // Fluid LogOperate signature
        // LogOperate(address,address,int256,int256,address,address,uint256,uint256)
        println!(
            "FluidLogOperate: {}",
            FluidLogOperate::SIGNATURE_HASH
        );
        // Verify it's a valid keccak256 hash (not zero)
        assert_ne!(
            FluidLogOperate::SIGNATURE_HASH,
            alloy_primitives::B256::ZERO,
            "FluidLogOperate signature should not be zero"
        );
    }

    #[test]
    fn test_decode_fluid_log_operate() {
        // Simulate a LogOperate event from the Fluid Liquidity Layer
        let liquidity_layer = Address::from([0x52; 20]); // simplified
        let pool_addr = Address::from([0xAA; 20]);
        let token_addr = Address::from([0xBB; 20]);

        // Build topic entries: topics[1] = user (pool), topics[2] = token
        let user_topic = {
            let mut b = [0u8; 32];
            b[12..].copy_from_slice(pool_addr.as_slice());
            alloy_primitives::B256::from(b)
        };
        let token_topic = {
            let mut b = [0u8; 32];
            b[12..].copy_from_slice(token_addr.as_slice());
            alloy_primitives::B256::from(b)
        };

        // data: int256 supplyAmount, int256 borrowAmount,
        //       address withdrawTo, address borrowTo,
        //       uint256 totalAmounts, uint256 exchangePricesAndConfig
        // = 6 x 32 bytes = 192 bytes
        let log = Log {
            address: liquidity_layer,
            data: LogData::new_unchecked(
                vec![
                    FluidLogOperate::SIGNATURE_HASH,
                    user_topic,
                    token_topic,
                ],
                vec![0u8; 192].into(),
            ),
        };

        let decoded = decode_log(&log);
        assert!(
            matches!(decoded, Some(DecodedEvent::FluidOperate { .. })),
            "Should decode as FluidOperate"
        );

        if let Some(DecodedEvent::FluidOperate { pool, token }) = decoded {
            assert_eq!(pool, pool_addr);
            assert_eq!(token, token_addr);
        }
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
    fn test_decode_v3_swap_pancake() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![
                    PancakeV3Swap::SIGNATURE_HASH,
                    alloy_primitives::B256::ZERO, // sender
                    alloy_primitives::B256::ZERO, // recipient
                ],
                vec![0u8; 224].into(), // + two uint128 protocol fee fields
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
        assert!(matches!(
            decoded,
            Some(DecodedEvent::V4ModifyLiquidity { .. })
        ));
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
            B256::from(hex!(
                "000000000000000000000000e592427a0aece92de3edee1f18e0157c05861564"
            )), // sender (router)
            B256::from(hex!(
                "000000000000000000000000e592427a0aece92de3edee1f18e0157c05861564"
            )), // recipient
        ];

        // Data: amount0, amount1, sqrtPriceX96, liquidity, tick (simplified example)
        let data = hex!(
            "0000000000000000000000000000000000000000000000000000000000000064" // amount0 (100 in simplified form)
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffce" // amount1 (-50 in two's complement)
            "00000000000000000000000000000001000000000000000000000000000000ff" // sqrtPriceX96
            "00000000000000000000000000000000000000000000000000000000deadbeef" // liquidity
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff8ad0" // tick (-30000 in two's complement)
        )
        .to_vec();

        let log = Log {
            address: pool_address,
            data: LogData::new_unchecked(topics, data.into()),
        };

        // Decode the event
        let decoded = decode_log(&log);

        // Verify it decoded successfully as V3Swap
        assert!(decoded.is_some(), "Failed to decode real V3 Swap event");

        match decoded.unwrap() {
            DecodedEvent::V3Swap {
                pool,
                sqrt_price_x96,
                liquidity,
                tick,
            } => {
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
            B256::from(hex!(
                "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"
            )), // owner (position manager)
            B256::from(hex!(
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff8ad0"
            )), // tickLower (-30000)
            B256::from(hex!(
                "0000000000000000000000000000000000000000000000000000000000007530"
            )), // tickUpper (30000)
        ];

        // Data: sender, amount, amount0, amount1
        let data = hex!(
            "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88" // sender
            "00000000000000000000000000000000000000000000000000000000000f4240" // amount (1000000)
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000" // amount0
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000" // amount1
        )
        .to_vec();

        let log = Log {
            address: pool_address,
            data: LogData::new_unchecked(topics, data.into()),
        };

        let decoded = decode_log(&log);
        assert!(decoded.is_some(), "Failed to decode real V3 Mint event");

        match decoded.unwrap() {
            DecodedEvent::V3Mint {
                pool,
                tick_lower,
                tick_upper,
                amount,
            } => {
                assert_eq!(pool, pool_address);
                assert_eq!(tick_lower, -30000);
                assert_eq!(tick_upper, 30000);
                assert!(amount > 0);
            }
            other => panic!("Expected V3Mint, got {:?}", other),
        }
    }
}
