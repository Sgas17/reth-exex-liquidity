//! ExEx-side live apply for the shadow arena (ITE-16, step 3c).
//!
//! Ports arena_service's `apply_live_event_internal` so the shadow arena applies
//! each committed block's pool updates exactly as arena_service does over the
//! socket — keeping the two writers in lockstep for the pre-cutover diff. Reuses
//! the shared [`arena_writer`] mutation API.
//!
//! The Curve/Fluid/V2 Sync "full-state" variants carry absolute post-state, so
//! applying them needs no swap math. Balancer still folds Vault deltas into
//! current arena state; V2 reorg recovery uses a final absolute reserve epilogue.

use crate::types::{
    PoolIdentifier, PoolUpdate, PoolUpdateMessage, Protocol, ReorgEpilogueUpdate, UpdateType,
};
use alloy_primitives::U256;
use arena_writer::{SharedArenaWriter, WriterError};

/// Apply failure: a writer error (e.g. the pool is not in the shadow topology),
/// arithmetic overflow folding a delta, or an unsupported legacy variant.
#[derive(Debug)]
pub enum ApplyError {
    Writer(WriterError),
    Overflow(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Writer(e) => write!(f, "writer: {e}"),
            ApplyError::Overflow(what) => write!(f, "overflow applying {what}"),
            ApplyError::Unsupported(what) => write!(f, "unsupported update: {what}"),
        }
    }
}

impl std::error::Error for ApplyError {}

impl From<WriterError> for ApplyError {
    fn from(e: WriterError) -> Self {
        ApplyError::Writer(e)
    }
}

type Result<T> = std::result::Result<T, ApplyError>;

/// Negate a liquidity delta when applying a revert.
fn maybe_negate_liquidity_delta(delta: i128, is_revert: bool) -> Result<i128> {
    if is_revert {
        delta
            .checked_neg()
            .ok_or(ApplyError::Overflow("liquidity delta"))
    } else {
        Ok(delta)
    }
}

/// Slot0-style post-state extracted from a V3/V4/Ekubo swap event.
struct Slot0 {
    sqrt_price_x96: U256,
    tick: i32,
    liquidity: u128,
}

/// Mirrors arena_service `extract_slot0_update`: only swaps carry slot0 state.
fn extract_slot0(event: &PoolUpdateMessage) -> Option<Slot0> {
    if event.update_type != UpdateType::Swap {
        return None;
    }
    match &event.update {
        PoolUpdate::V3Swap {
            sqrt_price_x96,
            liquidity,
            tick,
        }
        | PoolUpdate::V4Swap {
            sqrt_price_x96,
            liquidity,
            tick,
        } => Some(Slot0 {
            sqrt_price_x96: *sqrt_price_x96,
            tick: *tick,
            liquidity: *liquidity,
        }),
        PoolUpdate::EkuboSwap {
            sqrt_ratio,
            liquidity,
            tick,
        } => Some(Slot0 {
            // Ekubo sqrtRatio is native uint96 stored in a U256, not Q64.96.
            sqrt_price_x96: *sqrt_ratio,
            tick: *tick,
            liquidity: *liquidity,
        }),
        // EkuboLiquidity (PositionUpdated) also carries post-state but is emitted
        // with a Mint/Burn update_type, so it is applied directly in its arm
        // rather than through this swap-gated helper.
        _ => None,
    }
}

struct LiquidityChange {
    tick_lower: i32,
    tick_upper: i32,
    liquidity_delta: i128,
}

/// Mirrors arena_service `extract_liquidity_update`: only V3/V4 mint/burn carry
/// a tick-range liquidity delta. (Ekubo's PositionUpdated is handled in its own
/// arm via the post-state slot0, not through this helper.)
fn extract_liquidity(event: &PoolUpdateMessage) -> Option<LiquidityChange> {
    match event.update_type {
        UpdateType::Mint | UpdateType::Burn => {}
        UpdateType::Swap => return None,
    }
    match &event.update {
        PoolUpdate::V3Liquidity {
            tick_lower,
            tick_upper,
            liquidity_delta,
        }
        | PoolUpdate::V4Liquidity {
            tick_lower,
            tick_upper,
            liquidity_delta,
        } => Some(LiquidityChange {
            tick_lower: *tick_lower,
            tick_upper: *tick_upper,
            liquidity_delta: *liquidity_delta,
        }),
        _ => None,
    }
}

/// Unpack Tricrypto packed price_scale: `ps[0]` in the lower 128 bits, `ps[1]`
/// in the upper 128.
fn unpack_tricrypto_price_scale(packed: U256) -> [u128; 2] {
    let mask128 = U256::from(u128::MAX);
    [
        (packed & mask128).to::<u128>(),
        (packed >> 128u32).to::<u128>(),
    ]
}

/// Apply one committed-block pool update to the shadow arena, mirroring
/// arena_service's `apply_live_event_internal`.
///
/// Returns `Ok(true)` if applied, `Ok(false)` if the event targets a pool not
/// present in the shadow topology (e.g. live-added but not yet hydrated) for the
/// delta-folding protocols. Absolute-state writes propagate
/// [`WriterError::PoolNotFound`] for the same condition; the caller downgrades
/// it to a debug skip.
/// `overflowed` is set to `true` when a tick-liquidity update could not fit the
/// pool's tier (its tick array overflowed) — the caller queues such pools for a
/// re-tier (promotion).
pub fn apply_live_event(
    writer: &mut SharedArenaWriter,
    event: &PoolUpdateMessage,
    overflowed: &mut bool,
) -> Result<bool> {
    match &event.update {
        // ── Uniswap V2: absolute reserve writes only ───────────────────
        PoolUpdate::V2Sync { reserve0, reserve1 } => {
            if event.is_revert {
                return Ok(false);
            }
            let PoolIdentifier::Address(addr) = &event.pool_id else {
                return Ok(false);
            };
            let addr = addr.into_array();
            if writer.get_v2_pool(&addr).is_none() {
                return Ok(false);
            }
            writer.update_v2_reserves(addr, *reserve0, *reserve1)?;
        }
        PoolUpdate::V2Swap { .. } | PoolUpdate::V2Liquidity { .. } => {
            return Err(ApplyError::Unsupported(
                "V2 delta update; expected V2Sync or V2ReservesFinal absolute reserves",
            ));
        }

        // ── Uniswap V3/V4 swap: absolute slot0 post-state ───────────────
        PoolUpdate::V3Swap { .. } | PoolUpdate::V4Swap { .. } => {
            if event.is_revert {
                // Reorg epilogue slot0-final provides definitive post-reorg state.
            } else if let Some(s) = extract_slot0(event) {
                match &event.pool_id {
                    PoolIdentifier::Address(addr) => {
                        writer.update_v3_slot0(
                            addr.into_array(),
                            s.sqrt_price_x96,
                            s.tick,
                            s.liquidity,
                        )?;
                    }
                    PoolIdentifier::PoolId(id) => {
                        writer.update_v4_slot0(*id, s.sqrt_price_x96, s.tick, s.liquidity)?;
                    }
                }
            }
        }

        // ── Uniswap V3/V4 mint/burn: tick-range liquidity delta ─────────
        PoolUpdate::V3Liquidity { .. } | PoolUpdate::V4Liquidity { .. } => {
            if let Some(liq) = extract_liquidity(event) {
                let delta = maybe_negate_liquidity_delta(liq.liquidity_delta, event.is_revert)?;
                match &event.pool_id {
                    PoolIdentifier::Address(addr) => {
                        *overflowed |= writer.update_v3_tick_liquidity(
                            addr.into_array(),
                            liq.tick_lower,
                            liq.tick_upper,
                            delta,
                        )?;
                    }
                    PoolIdentifier::PoolId(id) => {
                        *overflowed |= writer.update_v4_tick_liquidity(
                            *id,
                            liq.tick_lower,
                            liq.tick_upper,
                            delta,
                        )?;
                    }
                }
            }
        }

        // ── Ekubo ───────────────────────────────────────────────────────
        PoolUpdate::EkuboSwap { .. } => {
            if event.is_revert {
                // Reorg epilogue slot0-final provides definitive post-reorg state.
            } else if let Some(s) = extract_slot0(event) {
                if let PoolIdentifier::PoolId(id) = &event.pool_id {
                    writer.update_ekubo_slot0(*id, s.sqrt_price_x96, s.tick, s.liquidity)?;
                }
            }
        }
        PoolUpdate::EkuboLiquidity {
            tick_lower,
            tick_upper,
            liquidity_delta,
            sqrt_ratio,
            liquidity,
            tick,
        } => {
            // PositionUpdated carries both a tick-range liquidity delta and the
            // post-state (`stateAfter`), but is emitted with a Mint/Burn
            // update_type. Downstream Ekubo quote context is built from the arena
            // tick array + bitmap, so always fold the (revert-negated) tick delta.
            //
            // slot0 (`stateAfter`) is authoritative only for the FORWARD apply. On
            // a revert the `stateAfter` belongs to the reverted fork, so writing it
            // would pin slot0 to old-fork state; instead the pool is added to the
            // affected-slot0 set (see `record_affected_slot0_pool`) and the reorg
            // slot0-final epilogue restores the canonical post-reorg slot0 — the
            // same model as V3/V4/Ekubo swap reverts.
            if let PoolIdentifier::PoolId(id) = &event.pool_id {
                let delta = maybe_negate_liquidity_delta(*liquidity_delta, event.is_revert)?;
                *overflowed |=
                    writer.update_ekubo_tick_liquidity(*id, *tick_lower, *tick_upper, delta)?;
                if !event.is_revert {
                    writer.update_ekubo_slot0(*id, *sqrt_ratio, *tick, *liquidity)?;
                }
            }
        }

        // ── Curve StableSwap-NG ─────────────────────────────────────────
        PoolUpdate::CurveSwap { .. } => {
            return Err(ApplyError::Unsupported(
                "legacy CurveSwap delta update; expected full-state CurveLiquidity",
            ));
        }
        PoolUpdate::CurveLiquidity {
            effective_balances,
            fee,
            offpeg_fee_multiplier,
            initial_a,
            future_a,
            initial_a_time,
            future_a_time,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_stable_state(
                    addr.into_array(),
                    effective_balances,
                    *fee,
                    *offpeg_fee_multiplier,
                    *initial_a,
                    *future_a,
                    *initial_a_time,
                    *future_a_time,
                )?;
            }
        }
        PoolUpdate::CurveRampA {
            initial_a,
            future_a,
            initial_a_time,
            future_a_time,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_stable_a(
                    addr.into_array(),
                    *initial_a,
                    *future_a,
                    *initial_a_time,
                    *future_a_time,
                )?;
            }
        }
        PoolUpdate::CurveFeeUpdate {
            fee,
            offpeg_fee_multiplier,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_stable_fees(addr.into_array(), *fee, *offpeg_fee_multiplier)?;
            }
        }

        // ── Curve TwoCryptoNG ───────────────────────────────────────────
        PoolUpdate::TwoCryptoState {
            balances,
            price_scale,
            d,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_twocrypto_state(
                    addr.into_array(),
                    balances[0],
                    balances[1],
                    d.saturating_to::<u128>(),
                    price_scale.saturating_to::<u128>(),
                )?;
            }
        }
        PoolUpdate::TwoCryptoRampAgamma {
            initial_a,
            future_a,
            initial_gamma,
            future_gamma,
            initial_time,
            future_time,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_twocrypto_a_gamma(
                    addr.into_array(),
                    *initial_a,
                    *initial_gamma,
                    *future_a,
                    *future_gamma,
                    *initial_time,
                    *future_time,
                )?;
            }
        }
        PoolUpdate::TwoCryptoNewParameters {
            mid_fee,
            out_fee,
            fee_gamma,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_twocrypto_fees(
                    addr.into_array(),
                    *mid_fee,
                    *out_fee,
                    *fee_gamma,
                )?;
            }
        }

        // ── Curve TricryptoNG ───────────────────────────────────────────
        PoolUpdate::TricryptoState {
            balances,
            packed_price_scale,
            d,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                let [ps0, ps1] = unpack_tricrypto_price_scale(*packed_price_scale);
                writer.update_curve_tricrypto_state(
                    addr.into_array(),
                    *balances,
                    d.saturating_to::<u128>(),
                    [ps0, ps1],
                )?;
            }
        }
        PoolUpdate::TricryptoRampAgamma {
            initial_a,
            future_a,
            initial_gamma,
            future_gamma,
            initial_time,
            future_time,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_tricrypto_a_gamma(
                    addr.into_array(),
                    *initial_a,
                    *initial_gamma,
                    *future_a,
                    *future_gamma,
                    *initial_time,
                    *future_time,
                )?;
            }
        }
        PoolUpdate::TricryptoNewParameters {
            mid_fee,
            out_fee,
            fee_gamma,
        } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_curve_tricrypto_fees(
                    addr.into_array(),
                    *mid_fee,
                    *out_fee,
                    *fee_gamma,
                )?;
            }
        }

        // ── Balancer V2: fold per-token balance deltas ──────────────────
        PoolUpdate::BalancerSwap {
            token_in,
            token_out,
            amount_in,
            amount_out,
        } => {
            let PoolIdentifier::PoolId(id) = &event.pool_id else {
                return Ok(false);
            };
            let Some(pool) = writer.get_balancer_v2_pool(id) else {
                return Ok(false);
            };
            let n = pool.n_tokens as usize;
            let mut new_balances = pool.balances[..n].to_vec();
            let amt_in = amount_in.saturating_to::<u128>();
            let amt_out = amount_out.saturating_to::<u128>();
            let token_in_bytes = token_in.into_array();
            let token_out_bytes = token_out.into_array();
            for (i, token) in pool.tokens.iter().enumerate().take(n) {
                if *token == token_in_bytes {
                    new_balances[i] = if event.is_revert {
                        new_balances[i].saturating_sub(amt_in)
                    } else {
                        new_balances[i].saturating_add(amt_in)
                    };
                }
                if *token == token_out_bytes {
                    new_balances[i] = if event.is_revert {
                        new_balances[i].saturating_add(amt_out)
                    } else {
                        new_balances[i].saturating_sub(amt_out)
                    };
                }
            }
            writer.update_balancer_v2_balances(id, &new_balances)?;
        }
        PoolUpdate::BalancerLiquidity { tokens, deltas } => {
            let PoolIdentifier::PoolId(id) = &event.pool_id else {
                return Ok(false);
            };
            let Some(pool) = writer.get_balancer_v2_pool(id) else {
                return Ok(false);
            };
            let n = pool.n_tokens as usize;
            let mut new_balances = pool.balances[..n].to_vec();
            // Match each event (token, delta) to the pool's stored token order —
            // do not assume the Vault event order equals the arena/whitelist order.
            for (token, delta) in tokens.iter().zip(deltas.iter()) {
                let token_bytes = token.into_array();
                let Some(i) = pool.tokens.iter().take(n).position(|t| *t == token_bytes) else {
                    continue;
                };
                let effective = if event.is_revert { -*delta } else { *delta };
                new_balances[i] = if effective >= 0 {
                    new_balances[i].saturating_add(effective as u128)
                } else {
                    new_balances[i].saturating_sub(effective.unsigned_abs())
                };
            }
            writer.update_balancer_v2_balances(id, &new_balances)?;
        }
        PoolUpdate::BalancerFeeUpdate {
            swap_fee_percentage,
        } => {
            if let PoolIdentifier::PoolId(id) = &event.pool_id {
                writer.update_balancer_v2_fee(id, *swap_fee_percentage)?;
            }
        }

        // ── Fluid DEX: absolute reserve snapshot ────────────────────────
        PoolUpdate::FluidState { state } => {
            if let PoolIdentifier::Address(addr) = &event.pool_id {
                writer.update_fluid_reserves(
                    addr.into_array(),
                    state.col_token0_real,
                    state.col_token1_real,
                    state.col_token0_imaginary,
                    state.col_token1_imaginary,
                    state.debt_token0_real,
                    state.debt_token1_real,
                    state.debt_token0_imaginary,
                    state.debt_token1_imaginary,
                    state.center_price,
                    state.fee,
                )?;
            }
        }
    }

    Ok(true)
}

/// Apply a reorg-epilogue update (ITE-16 step 3d) to the shadow arena, mirroring
/// arena_service's `apply_reorg_epilogue_updates`. The epilogue carries the
/// definitive post-reorg state read from chain at the settled tip, so it is an
/// authoritative absolute write (no replay guard). Returns `Ok(false)` for an
/// update whose pool-id kind does not match its protocol's slot.
pub fn apply_reorg_epilogue(
    writer: &mut SharedArenaWriter,
    update: &ReorgEpilogueUpdate,
) -> Result<bool> {
    match update {
        ReorgEpilogueUpdate::Slot0Final {
            pool_id,
            protocol,
            state,
        } => match pool_id {
            PoolIdentifier::Address(addr) => {
                writer.update_v3_slot0(
                    addr.into_array(),
                    state.sqrt_price_x96,
                    state.tick,
                    state.liquidity,
                )?;
            }
            PoolIdentifier::PoolId(id) => {
                if *protocol == Protocol::Ekubo {
                    writer.update_ekubo_slot0(
                        *id,
                        state.sqrt_price_x96,
                        state.tick,
                        state.liquidity,
                    )?;
                } else {
                    writer.update_v4_slot0(
                        *id,
                        state.sqrt_price_x96,
                        state.tick,
                        state.liquidity,
                    )?;
                }
            }
        },
        ReorgEpilogueUpdate::FluidStateFinal { pool_id, state } => {
            let PoolIdentifier::Address(addr) = pool_id else {
                return Ok(false);
            };
            writer.update_fluid_reserves(
                addr.into_array(),
                state.col_token0_real,
                state.col_token1_real,
                state.col_token0_imaginary,
                state.col_token1_imaginary,
                state.debt_token0_real,
                state.debt_token1_real,
                state.debt_token0_imaginary,
                state.debt_token1_imaginary,
                state.center_price,
                state.fee,
            )?;
        }
        ReorgEpilogueUpdate::V2ReservesFinal {
            pool_id,
            reserve0,
            reserve1,
        } => {
            let PoolIdentifier::Address(addr) = pool_id else {
                return Ok(false);
            };
            let addr = addr.into_array();
            if writer.get_v2_pool(&addr).is_none() {
                return Ok(false);
            }
            writer.update_v2_reserves(addr, *reserve0, *reserve1)?;
        }
    }
    Ok(true)
}
