// Reth ExEx: Liquidity Pool Event Decoder with Unix Socket Output
//
// This ExEx:
// 1. Subscribes to pool whitelist updates from dynamicWhitelist (via NATS or file)
// 2. Decodes Uniswap V2/V3/V4 Swap/Mint/Burn events from tracked pools
// 3. Sends pool state updates via Unix Domain Socket to orderbook engine
//
// Architecture:
//   Reth ExEx → Event Decoder → Pool State Extractor → Unix Socket → Orderbook Engine

// Use jemalloc as the global allocator to avoid glibc robust mutex crashes
// with MDBX. Without this, glibc's pthread_mutex_lock can abort() the process
// when a thread dies while holding an MDBX reader table lock (ESRCH).
#[global_allocator]
static ALLOC: reth_cli_util::allocator::Allocator = reth_cli_util::allocator::new_allocator();

mod balance_monitor;
mod events;
mod fluid_decoder;
mod nats_client;
mod pool_tracker;
mod shadow_arena;
#[allow(dead_code)]
mod socket;
mod swap_monitor;
#[allow(dead_code)]
mod transfers;
mod types;

use alloy_consensus::{BlockHeader, TxReceipt};
use alloy_primitives::{Address, I256, U256};
use arena_layout::ekubo::EkuboPoolData;
use arena_layout::{
    AnyEkuboPool, AnyUniswapV3Pool, AnyUniswapV4Pool, CurveStablePoolData, CurveTricryptoPoolData,
    CurveTwoCryptoPoolData, PoolTier, UniswapV3PoolData, UniswapV4PoolData,
};
use events::{decode_log, fluid_log_operate_pool, DecodedEvent};
use fluid_decoder::FluidPoolConfig;
use futures::{StreamExt, TryStreamExt};
use nats_client::WhitelistNatsClient;
use pool_tracker::PoolTracker;
use reth::providers::StateProviderFactory;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::FullNodeComponents;
use reth_node_ethereum::EthereumNode;
use reth_provider::StateProvider;
use shadow_arena::{
    CurveStableHydration, CurveTricryptoHydration, CurveTwoCryptoHydration, EkuboHydration,
    FluidHydration, ShadowArena, UniswapV3Hydration, UniswapV4Hydration, V2Hydration,
};
use socket::PoolUpdateSocketServer;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use types::{
    ControlMessage, FluidState, PoolIdentifier, PoolMetadata, PoolUpdate, PoolUpdateMessage,
    Protocol, ReorgEpilogueUpdate, ReorgRange, Slot0State, TokenMetadata, UpdateType,
};

/// Main ExEx state
struct LiquidityExEx {
    /// Pool tracker (shared, can be updated from whitelist subscription)
    pool_tracker: Arc<RwLock<PoolTracker>>,

    /// Socket sender for outgoing messages
    socket_tx: tokio::sync::mpsc::Sender<ControlMessage>,

    /// In-process shadow arena writer (ITE-16). `None` unless `SHADOW_ARENA_PATH`
    /// is set; when present, block boundaries are mirrored into the shadow arena.
    shadow: Option<ShadowArena>,

    /// Statistics
    events_processed: u64,
    blocks_processed: u64,
}

fn v2_swap_deltas(
    amount0_in: U256,
    amount1_in: U256,
    amount0_out: U256,
    amount1_out: U256,
) -> (I256, I256) {
    let delta0 = I256::try_from(amount0_in).unwrap_or(I256::ZERO)
        - I256::try_from(amount0_out).unwrap_or(I256::ZERO);
    let delta1 = I256::try_from(amount1_in).unwrap_or(I256::ZERO)
        - I256::try_from(amount1_out).unwrap_or(I256::ZERO);
    (delta0, delta1)
}

impl LiquidityExEx {
    fn new(
        socket_tx: tokio::sync::mpsc::Sender<ControlMessage>,
        shadow: Option<ShadowArena>,
    ) -> Self {
        Self {
            pool_tracker: Arc::new(RwLock::new(PoolTracker::new())),
            socket_tx,
            shadow,
            events_processed: 0,
            blocks_processed: 0,
        }
    }

    /// Mirror a block end into the shadow arena (signal), if enabled.
    fn shadow_end_block(&mut self, block_number: u64) {
        if let Some(shadow) = self.shadow.as_mut() {
            shadow.end_block(block_number);
        }
    }

    /// Convert a decoded event into a PoolUpdateMessage
    fn create_pool_update(
        &self,
        event: DecodedEvent,
        block_number: u64,
        block_timestamp: u64,
        tx_index: u64,
        log_index: u64,
        is_revert: bool,
        state: &dyn StateProvider,
        _pool_tracker: &PoolTracker,
    ) -> Option<PoolUpdateMessage> {
        match event {
            // ============================================================================
            // UNISWAP V2 EVENTS
            // ============================================================================
            DecodedEvent::V2Swap {
                pool,
                amount0_in,
                amount1_in,
                amount0_out,
                amount1_out,
            } => {
                // V2 reserve movement is the net of all four event fields.
                // Do not branch on amount0_in == 0: rare swaps can have both an input and
                // an output amount populated on the same token side, and dropping one side
                // creates persistent reserve drift.
                let (amount0, amount1) =
                    v2_swap_deltas(amount0_in, amount1_in, amount0_out, amount1_out);

                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::UniswapV2,
                    update_type: UpdateType::Swap,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::V2Swap { amount0, amount1 },
                })
            }

            DecodedEvent::V2Mint {
                pool,
                amount0,
                amount1,
            } => {
                // Mint: positive deltas (liquidity added)
                let delta0 = I256::try_from(amount0).unwrap_or(I256::ZERO);
                let delta1 = I256::try_from(amount1).unwrap_or(I256::ZERO);

                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::UniswapV2,
                    update_type: UpdateType::Mint,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::V2Liquidity {
                        amount0: delta0,
                        amount1: delta1,
                    },
                })
            }

            DecodedEvent::V2Burn {
                pool,
                amount0,
                amount1,
            } => {
                // Burn: negative deltas (liquidity removed)
                let delta0 = -I256::try_from(amount0).unwrap_or(I256::ZERO);
                let delta1 = -I256::try_from(amount1).unwrap_or(I256::ZERO);

                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::UniswapV2,
                    update_type: UpdateType::Burn,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::V2Liquidity {
                        amount0: delta0,
                        amount1: delta1,
                    },
                })
            }

            // ============================================================================
            // UNISWAP V3 EVENTS
            // ============================================================================
            DecodedEvent::V3Swap {
                pool,
                sqrt_price_x96,
                liquidity,
                tick,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::UniswapV3,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::V3Swap {
                    sqrt_price_x96,
                    liquidity,
                    tick,
                },
            }),

            DecodedEvent::V3Mint {
                pool,
                tick_lower,
                tick_upper,
                amount,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::UniswapV3,
                update_type: UpdateType::Mint,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::V3Liquidity {
                    tick_lower,
                    tick_upper,
                    liquidity_delta: i128::try_from(amount).unwrap_or_else(|_| {
                        warn!(amount, "V3 Mint liquidity overflows i128, clamping");
                        i128::MAX
                    }),
                },
            }),

            DecodedEvent::V3Burn {
                pool,
                tick_lower,
                tick_upper,
                amount,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::UniswapV3,
                update_type: UpdateType::Burn,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::V3Liquidity {
                    tick_lower,
                    tick_upper,
                    liquidity_delta: i128::try_from(amount).map(|v| -v).unwrap_or_else(|_| {
                        warn!(amount, "V3 Burn liquidity overflows i128, clamping");
                        i128::MIN
                    }),
                },
            }),

            // ============================================================================
            // UNISWAP V4 EVENTS
            // ============================================================================
            DecodedEvent::V4Swap {
                pool_id,
                sqrt_price_x96,
                liquidity,
                tick,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::PoolId(pool_id),
                protocol: Protocol::UniswapV4,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::V4Swap {
                    sqrt_price_x96,
                    liquidity,
                    tick,
                },
            }),

            DecodedEvent::V4ModifyLiquidity {
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
            } => {
                let update_type = if liquidity_delta > 0 {
                    UpdateType::Mint
                } else {
                    UpdateType::Burn
                };

                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::PoolId(pool_id),
                    protocol: Protocol::UniswapV4,
                    update_type,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::V4Liquidity {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                    },
                })
            }

            // ============================================================================
            // EKUBO EVENTS
            // ============================================================================
            DecodedEvent::EkuboSwap {
                pool_id,
                sqrt_ratio,
                liquidity,
                tick,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::PoolId(pool_id),
                protocol: Protocol::Ekubo,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::EkuboSwap {
                    sqrt_ratio,
                    liquidity,
                    tick,
                },
            }),

            DecodedEvent::EkuboPositionUpdated {
                pool_id,
                tick_lower,
                tick_upper,
                liquidity_delta,
                sqrt_ratio,
                liquidity,
                tick,
            } => {
                let update_type = if liquidity_delta > 0 {
                    UpdateType::Mint
                } else {
                    UpdateType::Burn
                };

                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::PoolId(pool_id),
                    protocol: Protocol::Ekubo,
                    update_type,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::EkuboLiquidity {
                        tick_lower,
                        tick_upper,
                        liquidity_delta,
                        sqrt_ratio,
                        liquidity,
                        tick,
                    },
                })
            }

            // ============================================================================
            // CURVE STABLESWAP-NG EVENTS
            // ============================================================================
            DecodedEvent::CurveSwap { pool } => {
                let curve_state = read_curve_stable_liquidity_state(state, pool);
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::CurveStable,
                    update_type: UpdateType::Swap,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::CurveLiquidity {
                        effective_balances: curve_state.effective_balances,
                        fee: curve_state.fee,
                        offpeg_fee_multiplier: curve_state.offpeg_fee_multiplier,
                        initial_a: curve_state.initial_a,
                        future_a: curve_state.future_a,
                        initial_a_time: curve_state.initial_a_time,
                        future_a_time: curve_state.future_a_time,
                    },
                })
            }

            DecodedEvent::CurveLiquidityChange { pool } => {
                let curve_state = read_curve_stable_liquidity_state(state, pool);
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::CurveStable,
                    update_type: UpdateType::Mint,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::CurveLiquidity {
                        effective_balances: curve_state.effective_balances,
                        fee: curve_state.fee,
                        offpeg_fee_multiplier: curve_state.offpeg_fee_multiplier,
                        initial_a: curve_state.initial_a,
                        future_a: curve_state.future_a,
                        initial_a_time: curve_state.initial_a_time,
                        future_a_time: curve_state.future_a_time,
                    },
                })
            }

            DecodedEvent::CurveRampA {
                pool,
                old_a,
                new_a,
                initial_time,
                future_time,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::CurveStable,
                update_type: UpdateType::Swap, // No specific type for param changes
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::CurveRampA {
                    initial_a: old_a,
                    future_a: new_a,
                    initial_a_time: initial_time,
                    future_a_time: future_time,
                },
            }),

            DecodedEvent::CurveApplyNewFee {
                pool,
                fee,
                offpeg_fee_multiplier,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::CurveStable,
                update_type: UpdateType::Swap, // No specific type for param changes
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::CurveFeeUpdate {
                    fee,
                    offpeg_fee_multiplier,
                },
            }),

            // ============================================================================
            // CURVE TWOCRYPTO-NG EVENTS
            // ============================================================================
            // ============================================================================
            // CURVE TWOCRYPTO / TRICRYPTO EVENTS (shared signatures)
            // ============================================================================
            // TwoCrypto and Tricrypto share TokenExchange, RampAgamma, NewParameters,
            // and RemoveLiquidityOne signatures. Disambiguate by pool protocol.
            DecodedEvent::TwoCryptoSwap { pool } => {
                let is_tricrypto =
                    _pool_tracker.get_protocol(&pool) == Some(Protocol::CurveTricrypto);
                let protocol = if is_tricrypto {
                    Protocol::CurveTricrypto
                } else {
                    Protocol::CurveTwoCrypto
                };
                let update = if is_tricrypto {
                    let crypto_state = read_tricrypto_full_state(state, pool);
                    PoolUpdate::TricryptoState {
                        balances: crypto_state.balances,
                        packed_price_scale: crypto_state.packed_price_scale,
                        d: crypto_state.d,
                    }
                } else {
                    let version = _pool_tracker
                        .get_by_address(&pool)
                        .and_then(|meta| meta.twocrypto_version.as_deref());
                    let crypto_state = read_twocrypto_full_state(state, pool, version);
                    PoolUpdate::TwoCryptoState {
                        balances: crypto_state.balances,
                        price_scale: crypto_state.price_scale,
                        d: crypto_state.d,
                    }
                };
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol,
                    update_type: UpdateType::Swap,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update,
                })
            }

            DecodedEvent::TwoCryptoLiquidityChange { pool } => {
                let is_tricrypto =
                    _pool_tracker.get_protocol(&pool) == Some(Protocol::CurveTricrypto);
                let protocol = if is_tricrypto {
                    Protocol::CurveTricrypto
                } else {
                    Protocol::CurveTwoCrypto
                };
                let update = if is_tricrypto {
                    let crypto_state = read_tricrypto_full_state(state, pool);
                    PoolUpdate::TricryptoState {
                        balances: crypto_state.balances,
                        packed_price_scale: crypto_state.packed_price_scale,
                        d: crypto_state.d,
                    }
                } else {
                    let version = _pool_tracker
                        .get_by_address(&pool)
                        .and_then(|meta| meta.twocrypto_version.as_deref());
                    let crypto_state = read_twocrypto_full_state(state, pool, version);
                    PoolUpdate::TwoCryptoState {
                        balances: crypto_state.balances,
                        price_scale: crypto_state.price_scale,
                        d: crypto_state.d,
                    }
                };
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol,
                    update_type: UpdateType::Mint,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update,
                })
            }

            DecodedEvent::TwoCryptoRampAgamma {
                pool,
                initial_a,
                future_a,
                initial_gamma,
                future_gamma,
                initial_time,
                future_time,
            } => {
                let is_tricrypto =
                    _pool_tracker.get_protocol(&pool) == Some(Protocol::CurveTricrypto);
                let protocol = if is_tricrypto {
                    Protocol::CurveTricrypto
                } else {
                    Protocol::CurveTwoCrypto
                };
                let update = if is_tricrypto {
                    PoolUpdate::TricryptoRampAgamma {
                        initial_a,
                        future_a,
                        initial_gamma,
                        future_gamma,
                        initial_time,
                        future_time,
                    }
                } else {
                    PoolUpdate::TwoCryptoRampAgamma {
                        initial_a,
                        future_a,
                        initial_gamma,
                        future_gamma,
                        initial_time,
                        future_time,
                    }
                };
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol,
                    update_type: UpdateType::Swap,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update,
                })
            }

            DecodedEvent::TwoCryptoNewParameters {
                pool,
                mid_fee,
                out_fee,
                fee_gamma,
            } => {
                let is_tricrypto =
                    _pool_tracker.get_protocol(&pool) == Some(Protocol::CurveTricrypto);
                let protocol = if is_tricrypto {
                    Protocol::CurveTricrypto
                } else {
                    Protocol::CurveTwoCrypto
                };
                let update = if is_tricrypto {
                    PoolUpdate::TricryptoNewParameters {
                        mid_fee,
                        out_fee,
                        fee_gamma,
                    }
                } else {
                    PoolUpdate::TwoCryptoNewParameters {
                        mid_fee,
                        out_fee,
                        fee_gamma,
                    }
                };
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol,
                    update_type: UpdateType::Swap,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update,
                })
            }

            // ============================================================================
            // CURVE TRICRYPTO EVENTS (unique signatures)
            // ============================================================================
            DecodedEvent::TricryptoLiquidityChange { pool } => {
                let crypto_state = read_tricrypto_full_state(state, pool);
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::CurveTricrypto,
                    update_type: UpdateType::Mint,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::TricryptoState {
                        balances: crypto_state.balances,
                        packed_price_scale: crypto_state.packed_price_scale,
                        d: crypto_state.d,
                    },
                })
            }

            // ============================================================================
            // BALANCER V2 EVENTS
            // ============================================================================
            DecodedEvent::BalancerSwap {
                pool_id,
                token_in,
                token_out,
                amount_in,
                amount_out,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::PoolId(pool_id),
                protocol: Protocol::BalancerV2Weighted,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::BalancerSwap {
                    token_in,
                    token_out,
                    amount_in,
                    amount_out,
                },
            }),

            DecodedEvent::BalancerPoolBalanceChanged { pool_id, deltas } => {
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::PoolId(pool_id),
                    protocol: Protocol::BalancerV2Weighted,
                    update_type: UpdateType::Mint,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::BalancerLiquidity { deltas },
                })
            }

            // ============================================================================
            // FLUID DEX EVENTS
            // ============================================================================
            // FluidOperate is handled separately — the caller collects touched
            // pools and batch-decodes reserves from storage after the log loop.
            DecodedEvent::FluidOperate { .. } => None,
        }
    }

    fn send_begin_block(
        &self,
        stream_seq: &mut u64,
        block_number: u64,
        block_timestamp: u64,
        base_fee_per_gas: u64,
        is_revert: bool,
    ) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::BeginBlock {
            stream_seq: seq,
            block_number,
            block_timestamp,
            base_fee_per_gas,
            is_revert,
        }) {
            warn!("Failed to send BeginBlock: {}", e);
        }
    }

    fn send_pool_update(&self, stream_seq: &mut u64, update_msg: PoolUpdateMessage) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::PoolUpdate {
            stream_seq: seq,
            event: update_msg,
        }) {
            warn!("Failed to send PoolUpdate: {}", e);
        }
    }

    fn send_end_block(&self, stream_seq: &mut u64, block_number: u64, num_updates: u64) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::EndBlock {
            stream_seq: seq,
            block_number,
            num_updates,
        }) {
            warn!("Failed to send EndBlock: {}", e);
        }
    }

    fn send_reorg_start(&self, stream_seq: &mut u64, old_range: ReorgRange, new_range: ReorgRange) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::ReorgStart {
            stream_seq: seq,
            old_range,
            new_range,
        }) {
            warn!("Failed to send ReorgStart: {}", e);
        }
    }

    fn send_reorg_epilogue(
        &self,
        stream_seq: &mut u64,
        final_tip_block: u64,
        final_tip_timestamp: u64,
        update: ReorgEpilogueUpdate,
    ) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::ReorgEpilogue {
            stream_seq: seq,
            final_tip_block,
            final_tip_timestamp,
            update,
        }) {
            warn!("Failed to send ReorgEpilogue: {}", e);
        }
    }

    fn send_reorg_complete(&self, stream_seq: &mut u64, final_tip_block: u64) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::ReorgComplete {
            stream_seq: seq,
            final_tip_block,
        }) {
            warn!("Failed to send ReorgComplete: {}", e);
        }
    }

    /// Check if we should process this decoded event
    /// For V2/V3: checks if pool address is tracked
    /// For V4: checks if pool_id is tracked (NOT the PoolManager address)
    fn should_process_event(&self, event: &DecodedEvent, pool_tracker: &PoolTracker) -> bool {
        let should_process = match event {
            // V2/V3 events: check pool address
            DecodedEvent::V2Swap { pool, .. }
            | DecodedEvent::V2Mint { pool, .. }
            | DecodedEvent::V2Burn { pool, .. }
            | DecodedEvent::V3Swap { pool, .. }
            | DecodedEvent::V3Mint { pool, .. }
            | DecodedEvent::V3Burn { pool, .. } => pool_tracker.is_tracked_address(pool),

            // V4 events: check pool_id (NOT address!)
            DecodedEvent::V4Swap { pool_id, .. }
            | DecodedEvent::V4ModifyLiquidity { pool_id, .. } => {
                pool_tracker.is_tracked_pool_id(pool_id)
            }

            // Ekubo events: check pool_id
            DecodedEvent::EkuboSwap { pool_id, .. }
            | DecodedEvent::EkuboPositionUpdated { pool_id, .. } => {
                pool_tracker.is_tracked_pool_id(pool_id)
            }

            // Curve StableSwap events: check pool address
            DecodedEvent::CurveSwap { pool }
            | DecodedEvent::CurveLiquidityChange { pool, .. }
            | DecodedEvent::CurveRampA { pool, .. }
            | DecodedEvent::CurveApplyNewFee { pool, .. } => pool_tracker.is_tracked_address(pool),

            // Curve TwoCrypto events: check pool address
            // NOTE: Tricrypto pools share TokenExchange/RampAgamma/NewParameters
            // signatures with TwoCrypto — they are decoded as TwoCrypto variants
            // and disambiguated in create_pool_update.
            DecodedEvent::TwoCryptoSwap { pool }
            | DecodedEvent::TwoCryptoLiquidityChange { pool, .. }
            | DecodedEvent::TwoCryptoRampAgamma { pool, .. }
            | DecodedEvent::TwoCryptoNewParameters { pool, .. } => {
                pool_tracker.is_tracked_address(pool)
            }

            // Curve Tricrypto-specific events (unique signatures)
            DecodedEvent::TricryptoLiquidityChange { pool, .. } => {
                pool_tracker.is_tracked_address(pool)
            }

            // Balancer V2 events: check pool_id (emitted by Vault singleton)
            DecodedEvent::BalancerSwap { pool_id, .. }
            | DecodedEvent::BalancerPoolBalanceChanged { pool_id, .. } => {
                pool_tracker.is_tracked_pool_id(pool_id)
            }

            // Fluid LogOperate: emitted by Liquidity Layer, `pool` is the
            // DEX pool address extracted from the indexed `user` topic.
            DecodedEvent::FluidOperate { pool, .. } => pool_tracker.is_tracked_fluid_pool(pool),
        };

        // Log when events are filtered out to help with debugging
        if !should_process {
            match event {
                DecodedEvent::V2Swap { pool, .. }
                | DecodedEvent::V2Mint { pool, .. }
                | DecodedEvent::V2Burn { pool, .. } => {
                    debug!("Filtered V2 event from untracked pool: {:?}", pool);
                }
                DecodedEvent::V3Swap { pool, .. }
                | DecodedEvent::V3Mint { pool, .. }
                | DecodedEvent::V3Burn { pool, .. } => {
                    debug!("Filtered V3 event from untracked pool: {:?}", pool);
                }
                DecodedEvent::V4Swap { pool_id, .. }
                | DecodedEvent::V4ModifyLiquidity { pool_id, .. } => {
                    debug!(
                        "Filtered V4 event from untracked pool_id: {:?}",
                        hex::encode(pool_id)
                    );
                }
                DecodedEvent::EkuboSwap { pool_id, .. }
                | DecodedEvent::EkuboPositionUpdated { pool_id, .. } => {
                    debug!(
                        "Filtered Ekubo event from untracked pool_id: {:?}",
                        hex::encode(pool_id)
                    );
                }
                DecodedEvent::CurveSwap { pool }
                | DecodedEvent::CurveLiquidityChange { pool, .. }
                | DecodedEvent::CurveRampA { pool, .. }
                | DecodedEvent::CurveApplyNewFee { pool, .. } => {
                    debug!("Filtered CurveStable event from untracked pool: {:?}", pool);
                }
                DecodedEvent::TwoCryptoSwap { pool }
                | DecodedEvent::TwoCryptoLiquidityChange { pool, .. }
                | DecodedEvent::TwoCryptoRampAgamma { pool, .. }
                | DecodedEvent::TwoCryptoNewParameters { pool, .. } => {
                    debug!(
                        "Filtered CurveTwoCrypto/Tricrypto event from untracked pool: {:?}",
                        pool
                    );
                }
                DecodedEvent::TricryptoLiquidityChange { pool, .. } => {
                    debug!(
                        "Filtered CurveTricrypto event from untracked pool: {:?}",
                        pool
                    );
                }
                DecodedEvent::BalancerSwap { pool_id, .. }
                | DecodedEvent::BalancerPoolBalanceChanged { pool_id, .. } => {
                    debug!(
                        "Filtered Balancer V2 event from untracked pool_id: {:?}",
                        hex::encode(pool_id)
                    );
                }
                DecodedEvent::FluidOperate { pool, .. } => {
                    debug!("Filtered Fluid LogOperate from untracked pool: {:?}", pool);
                }
            }
        }

        should_process
    }
}

/// TricryptoNG D slot (Vyper 0.3.10 layout — different from TwoCrypto).
///   slot 11 = balances[0]   ← NOT D
///   slot 12 = balances[1]
///   slot 13 = balances[2]
///   slot 14 = D              ← correct
///   slot 17 = virtual_price
/// Matches scrape_reth/src/tricrypto_storage.rs slots::D = 14.
const TRICRYPTO_D_SLOT: U256 = U256::from_limbs([14, 0, 0, 0]);

/// Read a single storage slot from a held state snapshot.
///
/// Returns `U256::ZERO` if the slot is empty or the read fails. Callers choose
/// the snapshot once (startup anchor, block post-state, or final reorg tip) and
/// then thread it through all per-protocol readers; no reader re-fetches
/// `latest()` internally.
fn read_storage_slot(state: &dyn StateProvider, address: Address, slot: U256) -> U256 {
    use alloy_primitives::B256;
    let slot_key: B256 = B256::from(slot);
    match state.storage(address, slot_key) {
        Ok(Some(value)) => value,
        Ok(None) => U256::ZERO,
        Err(e) => {
            warn!(
                "Failed to read storage slot {} for {:?}: {}",
                slot, address, e
            );
            U256::ZERO
        }
    }
}

/// Read a UniswapV2Pair's `(reserve0, reserve1)` from storage slot 8 of a held
/// state snapshot. Slot 8 packs `reserve0 (112) | reserve1 (112) | ts (32)`.
fn read_v2_reserves(state: &dyn StateProvider, address: Address) -> (u128, u128) {
    let packed = read_storage_slot(state, address, U256::from(8u64));
    let mask112: U256 = (U256::from(1u64) << 112usize) - U256::from(1u64);
    let reserve0 = (packed & mask112).to::<u128>();
    let reserve1 = ((packed >> 112usize) & mask112).to::<u128>();
    (reserve0, reserve1)
}

fn pool_address(pool: &PoolMetadata) -> Option<Address> {
    pool.pool_id.as_address()
}

fn pool_id_32(pool: &PoolMetadata) -> Option<[u8; 32]> {
    pool.pool_id.as_pool_id()
}

fn v3_factory(pool: &PoolMetadata) -> Option<Address> {
    (pool.factory != Address::ZERO).then_some(pool.factory)
}

fn singleton_contract_or(pool: &PoolMetadata, fallback: Address) -> Address {
    if pool.factory == Address::ZERO {
        fallback
    } else {
        pool.factory
    }
}

fn pool_tokens(pool: &PoolMetadata) -> Option<Vec<TokenMetadata>> {
    let mut tokens = Vec::with_capacity(2 + pool.extra_tokens.len());
    tokens.push(TokenMetadata {
        address: pool.token0,
        decimals: pool.token0_decimals?,
    });
    tokens.push(TokenMetadata {
        address: pool.token1,
        decimals: pool.token1_decimals?,
    });
    tokens.extend(pool.extra_tokens.iter().cloned());
    Some(tokens)
}

fn pow10_u128(exp: u32) -> Option<u128> {
    10u128.checked_pow(exp)
}

fn stable_rate_multiplier(decimals: u8) -> Option<u128> {
    (decimals <= 36).then_some(())?;
    pow10_u128(u32::from(36 - decimals))
}

fn crypto_precision(decimals: u8) -> Option<u128> {
    (decimals <= 18).then_some(())?;
    pow10_u128(u32::from(18 - decimals))
}

fn u256_to_u128_checked(value: U256) -> Option<u128> {
    (value <= U256::from(u128::MAX)).then(|| value.to::<u128>())
}

fn unpack_packed_a_gamma(packed: U256) -> Option<(u64, u128)> {
    let mask128 = U256::from(u128::MAX);
    let gamma = (packed & mask128).to::<u128>();
    let a: U256 = packed >> 128usize;
    (a <= U256::from(u64::MAX)).then(|| (a.to::<u64>(), gamma))
}

fn unpack_packed_fee_params(packed: U256) -> (u64, u64, u128) {
    let mask64 = U256::from(u64::MAX);
    let fee_gamma = (packed & mask64).to::<u128>();
    let out_fee = ((packed >> 64usize) & mask64).to::<u64>();
    let mid_fee = ((packed >> 128usize) & mask64).to::<u64>();
    (mid_fee, out_fee, fee_gamma)
}

fn v2_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<V2Hydration> {
    let addr = pool_address(pool)?;
    let (reserve0, reserve1) = read_v2_reserves(state, addr);
    Some(V2Hydration {
        address: addr.into_array(),
        token0: pool.token0.into_array(),
        token1: pool.token1.into_array(),
        reserve0,
        reserve1,
        token0_decimals: pool.token0_decimals?,
        token1_decimals: pool.token1_decimals?,
    })
}

fn v3_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<UniswapV3Hydration> {
    let addr = pool_address(pool)?;
    if pool.tick_spacing.is_none()
        || pool.fee.is_none()
        || pool.token0_decimals.is_none()
        || pool.token1_decimals.is_none()
    {
        warn!(pool = %addr, "Skipping V3 hydration: missing fee/tick/decimal metadata");
        return None;
    }
    let snapshot = read_v3_full_state(state, addr, pool.tick_spacing?, v3_factory(pool))?;
    let arena_pool = build_v3_pool(addr.into_array(), pool, &snapshot)?;
    Some(UniswapV3Hydration {
        address: addr.into_array(),
        pool: arena_pool,
    })
}

fn v4_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<UniswapV4Hydration> {
    use pool_tracker::UNISWAP_V4_POOL_MANAGER;

    let pool_id = pool_id_32(pool)?;
    if pool.tick_spacing.is_none()
        || pool.fee.is_none()
        || pool.token0_decimals.is_none()
        || pool.token1_decimals.is_none()
    {
        warn!(pool_id = ?pool_id, "Skipping V4 hydration: missing fee/tick/decimal metadata");
        return None;
    }
    let pool_manager = singleton_contract_or(pool, UNISWAP_V4_POOL_MANAGER);
    let snapshot = read_v4_full_state(state, pool_manager, &pool_id, pool.tick_spacing?)?;
    let arena_pool = build_v4_pool(pool_id, pool, &snapshot)?;
    Some(UniswapV4Hydration {
        pool_id,
        pool: arena_pool,
    })
}

fn ekubo_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<EkuboHydration> {
    use events::EKUBO_CORE;

    let pool_id = pool_id_32(pool)?;
    if pool.tick_spacing.is_none()
        || pool.ekubo_fee.is_none()
        || pool.ekubo_type_config.is_none()
        || pool.token0_decimals.is_none()
        || pool.token1_decimals.is_none()
    {
        warn!(pool_id = ?pool_id, "Skipping Ekubo hydration: missing fee/type_config/tick/decimal metadata");
        return None;
    }
    let ekubo_core = singleton_contract_or(pool, EKUBO_CORE);
    let snapshot = read_ekubo_full_state(state, ekubo_core, &pool_id, pool.tick_spacing?)?;
    let arena_pool = build_ekubo_pool(pool_id, pool, &snapshot)?;
    Some(EkuboHydration {
        pool_id,
        pool: arena_pool,
    })
}

fn curve_stable_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<CurveStableHydration> {
    let addr = pool_address(pool)?;
    let tokens = pool_tokens(pool)?;
    let scraped = read_curve_stable_liquidity_state(state, addr);
    let n = scraped.effective_balances.len();
    if n == 0 || tokens.len() < n {
        warn!(pool = %addr, n_coins = n, tokens = tokens.len(), "Skipping CurveStable hydration: incomplete token metadata");
        return None;
    }

    let mut data = CurveStablePoolData::default();
    data.n_coins = n as u8;
    data.fee = scraped.fee;
    data.offpeg_fee_multiplier = scraped.offpeg_fee_multiplier;
    data.initial_a = scraped.initial_a;
    data.future_a = scraped.future_a;
    data.initial_a_time = scraped.initial_a_time;
    data.future_a_time = scraped.future_a_time;
    for (i, balance) in scraped.effective_balances.iter().enumerate() {
        data.balances[i] = *balance;
        data.coins[i] = tokens[i].address.into_array();
        data.rate_multipliers[i] = match stable_rate_multiplier(tokens[i].decimals) {
            Some(v) => v,
            None => {
                warn!(pool = %addr, decimals = tokens[i].decimals, "Skipping CurveStable hydration: invalid token decimals");
                return None;
            }
        };
    }

    Some(CurveStableHydration {
        address: addr.into_array(),
        pool: data,
    })
}

fn curve_twocrypto_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<CurveTwoCryptoHydration> {
    let addr = pool_address(pool)?;
    let tokens = pool_tokens(pool)?;
    if tokens.len() < 2 {
        return None;
    }
    let scraped = read_twocrypto_full_state(state, addr, pool.twocrypto_version.as_deref());
    let (initial_a, initial_gamma) = unpack_packed_a_gamma(scraped.initial_a_gamma)?;
    let (future_a, future_gamma) = unpack_packed_a_gamma(scraped.future_a_gamma)?;
    let (mid_fee, out_fee, fee_gamma) = unpack_packed_fee_params(scraped.packed_fee_params);

    let mut data = CurveTwoCryptoPoolData::default();
    data.balances = scraped.balances;
    data.price_scale = u256_to_u128_checked(scraped.price_scale)?;
    data.d = u256_to_u128_checked(scraped.d)?;
    data.initial_a = initial_a;
    data.initial_gamma = initial_gamma;
    data.future_a = future_a;
    data.future_gamma = future_gamma;
    data.initial_a_gamma_time = scraped.initial_a_gamma_time;
    data.future_a_gamma_time = scraped.future_a_gamma_time;
    data.mid_fee = mid_fee;
    data.out_fee = out_fee;
    data.fee_gamma = fee_gamma;
    data.coins = [
        tokens[0].address.into_array(),
        tokens[1].address.into_array(),
    ];
    data.precisions = [
        crypto_precision(tokens[0].decimals)?,
        crypto_precision(tokens[1].decimals)?,
    ];

    Some(CurveTwoCryptoHydration {
        address: addr.into_array(),
        pool: data,
    })
}

fn curve_tricrypto_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
) -> Option<CurveTricryptoHydration> {
    let addr = pool_address(pool)?;
    let tokens = pool_tokens(pool)?;
    if tokens.len() < 3 {
        warn!(pool = %addr, tokens = tokens.len(), "Skipping CurveTricrypto hydration: missing extra token metadata");
        return None;
    }
    let scraped = read_tricrypto_full_state(state, addr);
    let (initial_a, initial_gamma) = unpack_packed_a_gamma(scraped.initial_a_gamma)?;
    let (future_a, future_gamma) = unpack_packed_a_gamma(scraped.future_a_gamma)?;
    let (mid_fee, out_fee, fee_gamma) = unpack_packed_fee_params(scraped.packed_fee_params);
    let mask128 = U256::from(u128::MAX);

    let mut data = CurveTricryptoPoolData::default();
    data.balances = scraped.balances;
    data.price_scale = [
        u256_to_u128_checked(scraped.packed_price_scale & mask128)?,
        u256_to_u128_checked(scraped.packed_price_scale >> 128usize)?,
    ];
    data.d = u256_to_u128_checked(scraped.d)?;
    data.initial_a = initial_a;
    data.initial_gamma = initial_gamma;
    data.future_a = future_a;
    data.future_gamma = future_gamma;
    data.initial_a_gamma_time = scraped.initial_a_gamma_time;
    data.future_a_gamma_time = scraped.future_a_gamma_time;
    data.mid_fee = mid_fee;
    data.out_fee = out_fee;
    data.fee_gamma = fee_gamma;
    data.coins = [
        tokens[0].address.into_array(),
        tokens[1].address.into_array(),
        tokens[2].address.into_array(),
    ];
    data.precisions = [
        crypto_precision(tokens[0].decimals)?,
        crypto_precision(tokens[1].decimals)?,
        crypto_precision(tokens[2].decimals)?,
    ];

    Some(CurveTricryptoHydration {
        address: addr.into_array(),
        pool: data,
    })
}

fn fluid_hydration_from_snapshot(
    state: &dyn StateProvider,
    pool: &PoolMetadata,
    configs: &HashMap<Address, FluidPoolConfig>,
    block_timestamp: u64,
) -> Option<FluidHydration> {
    let addr = pool_address(pool)?;
    let config = configs.get(&addr)?;
    let reserves = decode_fluid_pool(state, config, block_timestamp)?;
    let fee = u32::try_from(reserves.fee).ok()?;
    Some(FluidHydration {
        address: addr.into_array(),
        token0: pool.token0.into_array(),
        token1: pool.token1.into_array(),
        token0_decimals: pool.token0_decimals?,
        token1_decimals: pool.token1_decimals?,
        col_token0_real: reserves.col_token0_real_reserves,
        col_token1_real: reserves.col_token1_real_reserves,
        col_token0_imaginary: reserves.col_token0_imaginary_reserves,
        col_token1_imaginary: reserves.col_token1_imaginary_reserves,
        debt_token0_real: reserves.debt_token0_real_reserves,
        debt_token1_real: reserves.debt_token1_real_reserves,
        debt_token0_imaginary: reserves.debt_token0_imaginary_reserves,
        debt_token1_imaginary: reserves.debt_token1_imaginary_reserves,
        center_price: reserves.center_price,
        fee,
    })
}

/// 3b startup hydration: hydrate shadow arena slots from the rich startup
/// snapshot, frozen at one anchor. Decimals/token metadata come from the
/// whitelist; reserves and dynamic Curve/Fluid state come from a single held
/// `history_by_block_number(anchor)` snapshot. No-op when the shadow writer is
/// disabled.
fn hydrate_shadow_from_snapshot<Node: FullNodeComponents>(
    ctx: &ExExContext<Node>,
    pools: &[PoolMetadata],
    fluid_configs: &HashMap<Address, FluidPoolConfig>,
    shadow: Option<&mut ShadowArena>,
) {
    use reth_provider::{BlockNumReader, HeaderProvider};
    let Some(shadow) = shadow else {
        return;
    };
    // Pin the anchor block first, then read state pinned to that exact block, so
    // all hydrated protocols and the recorded `scraped_at_block` come from one
    // snapshot — the frozen-anchor invariant the 3c replay guard relies on.
    let anchor = match ctx.provider().best_block_number() {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "shadow hydration: no best block number");
            return;
        }
    };
    let anchor_timestamp = match ctx.provider().header_by_number(anchor) {
        Ok(Some(header)) => header.timestamp(),
        Ok(None) => {
            warn!(anchor, "shadow hydration: no header at anchor block");
            return;
        }
        Err(e) => {
            warn!(error = %e, anchor, "shadow hydration: failed to read anchor header");
            return;
        }
    };
    let state = match ctx.provider().history_by_block_number(anchor) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, anchor, "shadow hydration: no state at anchor block");
            return;
        }
    };

    let v2: Vec<V2Hydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::UniswapV2)
        .filter_map(|p| v2_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let v3: Vec<UniswapV3Hydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::UniswapV3)
        .filter_map(|p| v3_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let v4: Vec<UniswapV4Hydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::UniswapV4)
        .filter_map(|p| v4_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let ekubo: Vec<EkuboHydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::Ekubo)
        .filter_map(|p| ekubo_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let curve_stable: Vec<CurveStableHydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::CurveStable)
        .filter_map(|p| curve_stable_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let curve_twocrypto: Vec<CurveTwoCryptoHydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::CurveTwoCrypto)
        .filter_map(|p| curve_twocrypto_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let curve_tricrypto: Vec<CurveTricryptoHydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::CurveTricrypto)
        .filter_map(|p| curve_tricrypto_hydration_from_snapshot(state.as_ref(), p))
        .collect();
    let fluid: Vec<FluidHydration> = pools
        .iter()
        .filter(|p| p.protocol == Protocol::Fluid)
        .filter_map(|p| {
            fluid_hydration_from_snapshot(state.as_ref(), p, fluid_configs, anchor_timestamp)
        })
        .collect();

    let counts = shadow.hydrate_startup(
        anchor,
        &v2,
        &v3,
        &v4,
        &ekubo,
        &curve_stable,
        &curve_twocrypto,
        &curve_tricrypto,
        &fluid,
    );
    info!(?counts, anchor, "shadow arena: hydrated startup slots");
}

#[derive(Debug, Clone)]
struct TwoCryptoSnapshot {
    balances: [u128; 2],
    price_scale: U256,
    d: U256,
    initial_a_gamma: U256,
    initial_a_gamma_time: u64,
    future_a_gamma: U256,
    future_a_gamma_time: u64,
    packed_fee_params: U256,
}

#[derive(Debug, Clone)]
struct TricryptoSnapshot {
    balances: [u128; 3],
    packed_price_scale: U256,
    d: U256,
    initial_a_gamma: U256,
    initial_a_gamma_time: u64,
    future_a_gamma: U256,
    future_a_gamma_time: u64,
    packed_fee_params: U256,
}

#[derive(Debug, Clone)]
struct CurveStableSnapshot {
    effective_balances: Vec<u128>,
    fee: u64,
    offpeg_fee_multiplier: u64,
    initial_a: u64,
    future_a: u64,
    initial_a_time: u64,
    future_a_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TwoCryptoStorageSlots {
    initial_a_gamma: u64,
    initial_a_gamma_time: u64,
    future_a_gamma: u64,
    future_a_gamma_time: u64,
    balance_0: u64,
    balance_1: u64,
    d: u64,
    packed_fee_params: u64,
}

fn twocrypto_storage_slots(version: Option<&str>) -> TwoCryptoStorageSlots {
    // Mirrors scrape_reth::twocrypto_storage: v2.0.0 uses a legacy layout;
    // None and all newer versions default to the v2.1.x Vyper 0.4.1 layout.
    if version == Some("v2.0.0") {
        TwoCryptoStorageSlots {
            initial_a_gamma: 8,
            initial_a_gamma_time: 20,
            future_a_gamma: 10,
            future_a_gamma_time: 20,
            balance_0: 12,
            balance_1: 13,
            d: 14,
            packed_fee_params: 16,
        }
    } else {
        TwoCryptoStorageSlots {
            initial_a_gamma: 5,
            initial_a_gamma_time: 6,
            future_a_gamma: 7,
            future_a_gamma_time: 8,
            balance_0: 9,
            balance_1: 10,
            d: 11,
            packed_fee_params: 16,
        }
    }
}

fn read_twocrypto_full_state(
    state: &dyn StateProvider,
    address: Address,
    version: Option<&str>,
) -> TwoCryptoSnapshot {
    let slots = twocrypto_storage_slots(version);
    let balances = [
        read_storage_slot(state, address, U256::from(slots.balance_0)).to::<u128>(),
        read_storage_slot(state, address, U256::from(slots.balance_1)).to::<u128>(),
    ];
    let price_scale = read_storage_slot(state, address, U256::from(1u64));
    let d = read_storage_slot(state, address, U256::from(slots.d));
    let initial_a_gamma = read_storage_slot(state, address, U256::from(slots.initial_a_gamma));
    let initial_a_gamma_time =
        read_storage_slot(state, address, U256::from(slots.initial_a_gamma_time)).to::<u64>();
    let future_a_gamma = read_storage_slot(state, address, U256::from(slots.future_a_gamma));
    let future_a_gamma_time =
        read_storage_slot(state, address, U256::from(slots.future_a_gamma_time)).to::<u64>();
    let packed_fee_params = read_storage_slot(state, address, U256::from(slots.packed_fee_params));
    TwoCryptoSnapshot {
        balances,
        price_scale,
        d,
        initial_a_gamma,
        initial_a_gamma_time,
        future_a_gamma,
        future_a_gamma_time,
        packed_fee_params,
    }
}

fn read_tricrypto_full_state(state: &dyn StateProvider, address: Address) -> TricryptoSnapshot {
    let balances = [
        read_storage_slot(state, address, U256::from(11u64)).to::<u128>(),
        read_storage_slot(state, address, U256::from(12u64)).to::<u128>(),
        read_storage_slot(state, address, U256::from(13u64)).to::<u128>(),
    ];
    let packed_price_scale = read_storage_slot(state, address, U256::from(3u64));
    let d = read_storage_slot(state, address, TRICRYPTO_D_SLOT);
    let initial_a_gamma = read_storage_slot(state, address, U256::from(7u64));
    let initial_a_gamma_time = read_storage_slot(state, address, U256::from(8u64)).to::<u64>();
    let future_a_gamma = read_storage_slot(state, address, U256::from(9u64));
    let future_a_gamma_time = read_storage_slot(state, address, U256::from(10u64)).to::<u64>();
    let packed_fee_params = read_storage_slot(state, address, U256::from(20u64));
    TricryptoSnapshot {
        balances,
        packed_price_scale,
        d,
        initial_a_gamma,
        initial_a_gamma_time,
        future_a_gamma,
        future_a_gamma_time,
        packed_fee_params,
    }
}

fn read_curve_stable_liquidity_state(
    state: &dyn StateProvider,
    address: Address,
) -> CurveStableSnapshot {
    let n_coins = read_storage_slot(state, address, U256::from(1u64)).to::<usize>();
    let n_coins = n_coins.min(8);

    let mut effective_balances = Vec::with_capacity(n_coins);
    for i in 0..n_coins {
        let stored = read_storage_slot(state, address, U256::from((2 + i) as u64)).to::<u128>();
        let admin = read_storage_slot(state, address, U256::from((17 + i) as u64)).to::<u128>();
        effective_balances.push(stored.saturating_sub(admin));
    }

    CurveStableSnapshot {
        effective_balances,
        fee: read_storage_slot(state, address, U256::from(10u64)).to::<u64>(),
        offpeg_fee_multiplier: read_storage_slot(state, address, U256::from(11u64)).to::<u64>(),
        initial_a: read_storage_slot(state, address, U256::from(12u64)).to::<u64>(),
        future_a: read_storage_slot(state, address, U256::from(13u64)).to::<u64>(),
        initial_a_time: read_storage_slot(state, address, U256::from(14u64)).to::<u64>(),
        future_a_time: read_storage_slot(state, address, U256::from(15u64)).to::<u64>(),
    }
}

/// V3 storage slots.
const V3_SLOT0: U256 = U256::from_limbs([0, 0, 0, 0]);
const V3_LIQUIDITY_VANILLA: U256 = U256::from_limbs([4, 0, 0, 0]);
const V3_LIQUIDITY_PANCAKE: U256 = U256::from_limbs([5, 0, 0, 0]);
const PANCAKE_V3_FACTORY_ETHEREUM: Address =
    alloy_primitives::address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct V3StorageSlots {
    slot0: u64,
    liquidity: u64,
    ticks: u64,
    tick_bitmap: u64,
}

fn v3_slots_for_factory(factory: Option<Address>) -> V3StorageSlots {
    if factory == Some(PANCAKE_V3_FACTORY_ETHEREUM) {
        V3StorageSlots {
            slot0: 0,
            liquidity: 5,
            ticks: 6,
            tick_bitmap: 7,
        }
    } else {
        V3StorageSlots {
            slot0: 0,
            liquidity: 4,
            ticks: 5,
            tick_bitmap: 6,
        }
    }
}

/// V4 PoolManager mapping slot (pools mapping at slot 6).
const V4_POOLS_SLOT: U256 = U256::from_limbs([6, 0, 0, 0]);

/// Ekubo Core additive storage offsets.
const EKUBO_TICKS_OFFSET: U256 = U256::from_limbs([
    0x8e7acd6efb28c568,
    0xfca683928d56726d,
    0x174331cf5a3902d9,
    0x435a5eb89a296820,
]);
const EKUBO_BITMAPS_OFFSET: U256 = U256::from_limbs([
    0x737975db05a2e3a5,
    0x5a0f42fdd4c53e1c,
    0xf515ce5eba4b363b,
    0x3def450d0010a2fe,
]);

const MIN_TICK: i32 = -887_272;
const MAX_TICK: i32 = 887_272;
const EKUBO_MIN_TICK: i32 = -88_722_835;
const EKUBO_MAX_TICK: i32 = 88_722_835;
const EKUBO_BITMAP_OFFSET: u32 = 89_421_695;

/// Decode V3/V4 packed slot0: sqrtPriceX96 (bits 0-159), tick (bits 160-183, signed int24).
fn decode_slot0_packed(value: U256) -> (U256, i32) {
    let sqrt_price_mask = (U256::from(1u128) << 160) - U256::from(1u128);
    let sqrt_price_x96 = value & sqrt_price_mask;

    let tick_u256: U256 = (value >> 160) & U256::from(0xFFFFFFu32);
    let tick_raw: u32 = tick_u256.to();
    let tick = if tick_raw & 0x800000 != 0 {
        (tick_raw | 0xFF000000) as i32
    } else {
        tick_raw as i32
    };

    (sqrt_price_x96, tick)
}

/// Decode Ekubo packed state: sqrtRatio (bits 0-95), tick (bits 96-127), liquidity (bits 128-255).
fn decode_ekubo_state_packed(value: U256) -> (U256, i32, u128) {
    let bytes = value.to_be_bytes::<32>();

    // [0..12] = sqrtRatio (96 bits), [12..16] = tick (int32), [16..32] = liquidity (128 bits)
    let sqrt_ratio = {
        let mut buf = [0u8; 16];
        buf[4..16].copy_from_slice(&bytes[0..12]);
        U256::from(u128::from_be_bytes(buf))
    };
    let tick = i32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let liquidity = u128::from_be_bytes({
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&bytes[16..32]);
        buf
    });

    (sqrt_ratio, tick, liquidity)
}

/// Compute V4 pool base slot: keccak256(abi.encode(poolId, 6)).
fn v4_pool_base_slot(pool_id: &[u8; 32]) -> U256 {
    use alloy_primitives::{keccak256, B256};
    use alloy_sol_types::SolValue;
    let encoded = (B256::from_slice(pool_id), V4_POOLS_SLOT).abi_encode();
    U256::from_be_bytes(*keccak256(&encoded))
}

fn read_v3_liquidity(state: &dyn StateProvider, address: Address) -> u128 {
    let liquidity_raw = read_storage_slot(state, address, V3_LIQUIDITY_VANILLA);

    // Vanilla Uniswap V3-compatible pools store a plain uint128 at slot 4.
    if (liquidity_raw >> 128usize).is_zero() {
        return liquidity_raw.to::<u128>();
    }

    // PancakeSwap V3 on Ethereum packs protocolFees into slot 4 and stores
    // liquidity at slot 5 instead. Minimal whitelist updates do not preserve
    // factory metadata, so reorg overrides need this runtime fallback.
    let pancake_liquidity_raw = read_storage_slot(state, address, V3_LIQUIDITY_PANCAKE);
    pancake_liquidity_raw.to::<u128>()
}

/// Read slot0 override for a V3 pool from a held state snapshot.
fn read_v3_slot0(state: &dyn StateProvider, address: Address) -> Option<(U256, i32, u128)> {
    let slot0_raw = read_storage_slot(state, address, V3_SLOT0);
    if slot0_raw.is_zero() {
        return None;
    }
    let (sqrt_price_x96, tick) = decode_slot0_packed(slot0_raw);
    let liquidity = read_v3_liquidity(state, address);
    Some((sqrt_price_x96, tick, liquidity))
}

/// Read slot0 override for a V4 pool from a held state snapshot.
fn read_v4_slot0(
    state: &dyn StateProvider,
    pool_manager: Address,
    pool_id: &[u8; 32],
) -> Option<(U256, i32, u128)> {
    let base = v4_pool_base_slot(pool_id);
    // slot0 at base + 0, liquidity at base + 3
    let slot0_key = U256::from_be_bytes(base.to_be_bytes::<32>());
    let liquidity_key = slot0_key + U256::from(3);

    let slot0_raw = read_storage_slot(state, pool_manager, slot0_key);
    if slot0_raw.is_zero() {
        return None;
    }
    let (sqrt_price_x96, tick) = decode_slot0_packed(slot0_raw);
    let liquidity_raw = read_storage_slot(state, pool_manager, liquidity_key);
    let liquidity = liquidity_raw.to::<u128>();
    Some((sqrt_price_x96, tick, liquidity))
}

/// Read state for an Ekubo pool from a held state snapshot.
fn read_ekubo_state(
    state: &dyn StateProvider,
    ekubo_core: Address,
    pool_id: &[u8; 32],
) -> Option<(U256, i32, u128)> {
    use alloy_primitives::B256;
    let state_slot = U256::from_be_bytes(*B256::from_slice(pool_id));
    let state_raw = read_storage_slot(state, ekubo_core, state_slot);
    if state_raw.is_zero() {
        return None;
    }
    let (sqrt_ratio, tick, liquidity) = decode_ekubo_state_packed(state_raw);
    Some((sqrt_ratio, tick, liquidity))
}

#[derive(Debug, Clone)]
struct TickBitmapSnapshot {
    sqrt_price_x96: U256,
    tick: i32,
    liquidity: u128,
    ticks: Vec<(i32, u128, i128)>,
    tick_bitmaps: Vec<(i16, [u8; 32])>,
}

#[derive(Debug, Clone)]
struct EkuboTickBitmapSnapshot {
    sqrt_ratio: U256,
    tick: i32,
    liquidity: u128,
    ticks: Vec<(i32, u128, i128)>,
    tick_bitmaps: Vec<(u32, [u8; 32])>,
}

fn keccak_slot(encoded: Vec<u8>) -> U256 {
    U256::from_be_bytes(*alloy_primitives::keccak256(encoded))
}

fn v3_bitmap_slot(word_pos: i16, mapping_slot: u64) -> U256 {
    use alloy_sol_types::SolValue;
    keccak_slot((word_pos, U256::from(mapping_slot)).abi_encode())
}

fn v3_tick_slot(tick: i32, mapping_slot: u64) -> U256 {
    use alloy_sol_types::SolValue;
    keccak_slot((tick, U256::from(mapping_slot)).abi_encode())
}

fn v4_mapping_slot(pool_id: &[u8; 32], offset: u64) -> U256 {
    v4_pool_base_slot(pool_id) + U256::from(offset)
}

fn v4_bitmap_slot(pool_id: &[u8; 32], word_pos: i16) -> U256 {
    use alloy_sol_types::SolValue;
    let mapping_slot = v4_mapping_slot(pool_id, 5);
    keccak_slot((word_pos, mapping_slot).abi_encode())
}

fn v4_tick_slot(pool_id: &[u8; 32], tick: i32) -> U256 {
    use alloy_sol_types::SolValue;
    let mapping_slot = v4_mapping_slot(pool_id, 4);
    keccak_slot((tick, mapping_slot).abi_encode())
}

fn signed_i32_to_u256(value: i32) -> U256 {
    if value >= 0 {
        U256::from(value as u64)
    } else {
        U256::MAX - U256::from((-(i64::from(value)) - 1) as u64)
    }
}

fn ekubo_tick_slot(pool_id: &[u8; 32], tick: i32) -> U256 {
    U256::from_be_bytes(*pool_id)
        .wrapping_add(EKUBO_TICKS_OFFSET)
        .wrapping_add(signed_i32_to_u256(tick))
}

fn ekubo_bitmap_slot(pool_id: &[u8; 32], word: u32) -> U256 {
    U256::from_be_bytes(*pool_id)
        .wrapping_add(EKUBO_BITMAPS_OFFSET)
        .wrapping_add(U256::from(word))
}

fn tick_to_word_pos(tick: i32, tick_spacing: i32) -> i16 {
    let compressed = tick / tick_spacing;
    (compressed >> 8) as i16
}

fn generate_word_positions(tick_spacing: i32) -> Option<Vec<i16>> {
    (tick_spacing > 0).then(|| {
        let min_word = tick_to_word_pos(MIN_TICK, tick_spacing);
        let max_word = tick_to_word_pos(MAX_TICK, tick_spacing);
        (min_word..=max_word).collect()
    })
}

fn extract_ticks_from_bitmap_u256(
    word_pos: i16,
    bitmap_bytes: &[u8; 32],
    tick_spacing: i32,
) -> Vec<i32> {
    let mut ticks = Vec::new();
    for byte_idx in 0..32 {
        let byte = bitmap_bytes[31 - byte_idx];
        if byte == 0 {
            continue;
        }
        for bit_in_byte in 0..8u8 {
            if byte & (1 << bit_in_byte) != 0 {
                let bit_pos = (byte_idx as u16 * 8) + u16::from(bit_in_byte);
                let compressed = (i32::from(word_pos) << 8) | i32::from(bit_pos);
                let tick = compressed * tick_spacing;
                if (MIN_TICK..=MAX_TICK).contains(&tick) {
                    ticks.push(tick);
                }
            }
        }
    }
    ticks
}

fn ekubo_tick_to_word_and_index(tick: i32, tick_spacing: i32) -> (u32, u8) {
    let quotient = if tick < 0 && tick % tick_spacing != 0 {
        tick / tick_spacing - 1
    } else {
        tick / tick_spacing
    };
    let raw_index = (i64::from(quotient) + i64::from(EKUBO_BITMAP_OFFSET)) as u32;
    (raw_index >> 8, (raw_index & 0xff) as u8)
}

fn ekubo_word_and_index_to_tick(word: u32, index: u8, tick_spacing: i32) -> i32 {
    let raw_index = i64::from(word) * 256 + i64::from(index);
    ((raw_index - i64::from(EKUBO_BITMAP_OFFSET)) * i64::from(tick_spacing)) as i32
}

fn generate_ekubo_word_positions(tick_spacing: i32) -> Option<(u32, u32)> {
    (tick_spacing > 0).then(|| {
        let (min_word, _) = ekubo_tick_to_word_and_index(EKUBO_MIN_TICK, tick_spacing);
        let (max_word, _) = ekubo_tick_to_word_and_index(EKUBO_MAX_TICK, tick_spacing);
        (min_word, max_word)
    })
}

fn extract_ekubo_ticks_from_bitmap(
    word: u32,
    bitmap_bytes: &[u8; 32],
    tick_spacing: i32,
) -> Vec<i32> {
    let mut ticks = Vec::new();
    for byte_idx in 0..32usize {
        let byte = bitmap_bytes[31 - byte_idx];
        if byte == 0 {
            continue;
        }
        for bit_in_byte in 0..8u8 {
            if byte & (1 << bit_in_byte) != 0 {
                let index = (byte_idx as u8 * 8) + bit_in_byte;
                let tick = ekubo_word_and_index_to_tick(word, index, tick_spacing);
                if (EKUBO_MIN_TICK..=EKUBO_MAX_TICK).contains(&tick) {
                    ticks.push(tick);
                }
            }
        }
    }
    ticks
}

fn decode_tick_liquidity(tick: i32, storage_value: U256) -> Option<(i32, u128, i128)> {
    let liquidity_gross = (storage_value & U256::from(u128::MAX)).to::<u128>();
    let liquidity_net_raw = (storage_value >> 128usize).to::<u128>();
    let liquidity_net = i128::from_be_bytes(liquidity_net_raw.to_be_bytes());
    Some((tick, liquidity_gross, liquidity_net))
}

fn decode_ekubo_tick_liquidity(tick: i32, storage_value: U256) -> (i32, u128, i128) {
    let bytes = storage_value.to_be_bytes::<32>();
    let liquidity_gross = u128::from_be_bytes({
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&bytes[0..16]);
        buf
    });
    let liquidity_net = i128::from_be_bytes({
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&bytes[16..32]);
        buf
    });
    (tick, liquidity_gross, liquidity_net)
}

fn read_v3_full_state(
    state: &dyn StateProvider,
    address: Address,
    tick_spacing: i32,
    factory: Option<Address>,
) -> Option<TickBitmapSnapshot> {
    let slots = v3_slots_for_factory(factory);
    let slot0_raw = read_storage_slot(state, address, U256::from(slots.slot0));
    if slot0_raw.is_zero() {
        return None;
    }
    let (sqrt_price_x96, tick) = decode_slot0_packed(slot0_raw);
    let liquidity = u256_to_u128_checked(read_storage_slot(
        state,
        address,
        U256::from(slots.liquidity),
    ))?;

    let mut tick_bitmaps = Vec::new();
    for word_pos in generate_word_positions(tick_spacing)? {
        let bitmap = read_storage_slot(state, address, v3_bitmap_slot(word_pos, slots.tick_bitmap));
        if !bitmap.is_zero() {
            tick_bitmaps.push((word_pos, bitmap.to_be_bytes::<32>()));
        }
    }

    let mut tick_values = Vec::new();
    for (word_pos, bitmap) in &tick_bitmaps {
        tick_values.extend(extract_ticks_from_bitmap_u256(
            *word_pos,
            bitmap,
            tick_spacing,
        ));
    }

    let mut ticks = Vec::new();
    for tick_value in tick_values {
        let value = read_storage_slot(state, address, v3_tick_slot(tick_value, slots.ticks));
        if !value.is_zero() {
            ticks.push(decode_tick_liquidity(tick_value, value)?);
        }
    }

    Some(TickBitmapSnapshot {
        sqrt_price_x96,
        tick,
        liquidity,
        ticks,
        tick_bitmaps,
    })
}

fn read_v4_full_state(
    state: &dyn StateProvider,
    pool_manager: Address,
    pool_id: &[u8; 32],
    tick_spacing: i32,
) -> Option<TickBitmapSnapshot> {
    let slot0_raw = read_storage_slot(state, pool_manager, v4_mapping_slot(pool_id, 0));
    if slot0_raw.is_zero() {
        return None;
    }
    let (sqrt_price_x96, tick) = decode_slot0_packed(slot0_raw);
    let liquidity = u256_to_u128_checked(read_storage_slot(
        state,
        pool_manager,
        v4_mapping_slot(pool_id, 3),
    ))?;

    let mut tick_bitmaps = Vec::new();
    for word_pos in generate_word_positions(tick_spacing)? {
        let bitmap = read_storage_slot(state, pool_manager, v4_bitmap_slot(pool_id, word_pos));
        if !bitmap.is_zero() {
            tick_bitmaps.push((word_pos, bitmap.to_be_bytes::<32>()));
        }
    }

    let mut tick_values = Vec::new();
    for (word_pos, bitmap) in &tick_bitmaps {
        tick_values.extend(extract_ticks_from_bitmap_u256(
            *word_pos,
            bitmap,
            tick_spacing,
        ));
    }

    let mut ticks = Vec::new();
    for tick_value in tick_values {
        let value = read_storage_slot(state, pool_manager, v4_tick_slot(pool_id, tick_value));
        if !value.is_zero() {
            ticks.push(decode_tick_liquidity(tick_value, value)?);
        }
    }

    Some(TickBitmapSnapshot {
        sqrt_price_x96,
        tick,
        liquidity,
        ticks,
        tick_bitmaps,
    })
}

fn read_ekubo_full_state(
    state: &dyn StateProvider,
    ekubo_core: Address,
    pool_id: &[u8; 32],
    tick_spacing: i32,
) -> Option<EkuboTickBitmapSnapshot> {
    let (sqrt_ratio, tick, liquidity) = read_ekubo_state(state, ekubo_core, pool_id)?;

    if tick_spacing == 0 {
        return Some(EkuboTickBitmapSnapshot {
            sqrt_ratio,
            tick,
            liquidity,
            ticks: Vec::new(),
            tick_bitmaps: Vec::new(),
        });
    }

    let (min_word, max_word) = generate_ekubo_word_positions(tick_spacing)?;
    let mut tick_bitmaps = Vec::new();
    for word in min_word..=max_word {
        let bitmap = read_storage_slot(state, ekubo_core, ekubo_bitmap_slot(pool_id, word));
        if !bitmap.is_zero() {
            tick_bitmaps.push((word, bitmap.to_be_bytes::<32>()));
        }
    }

    let mut tick_values = Vec::new();
    for (word, bitmap) in &tick_bitmaps {
        tick_values.extend(extract_ekubo_ticks_from_bitmap(*word, bitmap, tick_spacing));
    }

    let mut ticks = Vec::new();
    for tick_value in tick_values {
        let value = read_storage_slot(state, ekubo_core, ekubo_tick_slot(pool_id, tick_value));
        if !value.is_zero() {
            ticks.push(decode_ekubo_tick_liquidity(tick_value, value));
        }
    }

    Some(EkuboTickBitmapSnapshot {
        sqrt_ratio,
        tick,
        liquidity,
        ticks,
        tick_bitmaps,
    })
}

fn determine_tier(tick_count: usize, bitmap_count: usize) -> PoolTier {
    let tick_tier = if tick_count <= 50 {
        PoolTier::Low
    } else if tick_count <= 200 {
        PoolTier::Active
    } else if tick_count <= 500 {
        PoolTier::Popular
    } else {
        PoolTier::Major
    };
    let bitmap_tier = if bitmap_count <= 10 {
        PoolTier::Low
    } else if bitmap_count <= 25 {
        PoolTier::Active
    } else if bitmap_count <= 50 {
        PoolTier::Popular
    } else {
        PoolTier::Major
    };
    tick_tier.max(bitmap_tier)
}

fn fill_v3_pool<const TICK_CAP: usize, const BITMAP_CAP: usize>(
    mut data: UniswapV3PoolData<TICK_CAP, BITMAP_CAP>,
    address: [u8; 20],
    pool: &PoolMetadata,
    snapshot: &TickBitmapSnapshot,
) -> Option<UniswapV3PoolData<TICK_CAP, BITMAP_CAP>> {
    if snapshot.ticks.len() > TICK_CAP || snapshot.tick_bitmaps.len() > BITMAP_CAP {
        return None;
    }
    data.common.pool_id = address;
    data.common.is_active.store(true, Ordering::Release);
    data.token0 = pool.token0.into_array();
    data.token1 = pool.token1.into_array();
    data.token0_decimals = pool.token0_decimals?;
    data.token1_decimals = pool.token1_decimals?;
    data.fee = pool.fee?;
    data.tick_spacing = pool.tick_spacing?;
    data.sqrt_price_x96 = snapshot.sqrt_price_x96;
    data.tick = snapshot.tick;
    data.liquidity = snapshot.liquidity;
    for (i, tick) in snapshot.ticks.iter().enumerate() {
        data.ticks[i] = *tick;
    }
    data.tick_count = snapshot.ticks.len() as u16;
    for (i, bitmap) in snapshot.tick_bitmaps.iter().enumerate() {
        data.tick_bitmap[i] = *bitmap;
    }
    data.bitmap_count = snapshot.tick_bitmaps.len() as u16;
    Some(data)
}

fn build_v3_pool(
    address: [u8; 20],
    pool: &PoolMetadata,
    snapshot: &TickBitmapSnapshot,
) -> Option<AnyUniswapV3Pool> {
    match determine_tier(snapshot.ticks.len(), snapshot.tick_bitmaps.len()) {
        PoolTier::Low => {
            fill_v3_pool(Default::default(), address, pool, snapshot).map(AnyUniswapV3Pool::Low)
        }
        PoolTier::Active => {
            fill_v3_pool(Default::default(), address, pool, snapshot).map(AnyUniswapV3Pool::Active)
        }
        PoolTier::Popular => {
            fill_v3_pool(Default::default(), address, pool, snapshot).map(AnyUniswapV3Pool::Popular)
        }
        PoolTier::Major => {
            fill_v3_pool(Default::default(), address, pool, snapshot).map(AnyUniswapV3Pool::Major)
        }
    }
}

fn fill_v4_pool<const TICK_CAP: usize, const BITMAP_CAP: usize>(
    mut data: UniswapV4PoolData<TICK_CAP, BITMAP_CAP>,
    pool_id: [u8; 32],
    pool: &PoolMetadata,
    snapshot: &TickBitmapSnapshot,
) -> Option<UniswapV4PoolData<TICK_CAP, BITMAP_CAP>> {
    if snapshot.ticks.len() > TICK_CAP || snapshot.tick_bitmaps.len() > BITMAP_CAP {
        return None;
    }
    data.pool_id = pool_id;
    data.common.pool_id.copy_from_slice(&pool_id[..20]);
    data.common.is_active.store(true, Ordering::Release);
    data.token0 = pool.token0.into_array();
    data.token1 = pool.token1.into_array();
    data.token0_decimals = pool.token0_decimals?;
    data.token1_decimals = pool.token1_decimals?;
    data.fee = pool.fee?;
    data.tick_spacing = pool.tick_spacing?;
    data.sqrt_price_x96 = snapshot.sqrt_price_x96;
    data.tick = snapshot.tick;
    data.liquidity = snapshot.liquidity;
    for (i, tick) in snapshot.ticks.iter().enumerate() {
        data.ticks[i] = *tick;
    }
    data.tick_count = snapshot.ticks.len() as u16;
    for (i, bitmap) in snapshot.tick_bitmaps.iter().enumerate() {
        data.tick_bitmap[i] = *bitmap;
    }
    data.bitmap_count = snapshot.tick_bitmaps.len() as u16;
    Some(data)
}

fn build_v4_pool(
    pool_id: [u8; 32],
    pool: &PoolMetadata,
    snapshot: &TickBitmapSnapshot,
) -> Option<AnyUniswapV4Pool> {
    match determine_tier(snapshot.ticks.len(), snapshot.tick_bitmaps.len()) {
        PoolTier::Low => {
            fill_v4_pool(Default::default(), pool_id, pool, snapshot).map(AnyUniswapV4Pool::Low)
        }
        PoolTier::Active => {
            fill_v4_pool(Default::default(), pool_id, pool, snapshot).map(AnyUniswapV4Pool::Active)
        }
        PoolTier::Popular => {
            fill_v4_pool(Default::default(), pool_id, pool, snapshot).map(AnyUniswapV4Pool::Popular)
        }
        PoolTier::Major => {
            fill_v4_pool(Default::default(), pool_id, pool, snapshot).map(AnyUniswapV4Pool::Major)
        }
    }
}

fn fill_ekubo_pool<const TICK_CAP: usize, const BITMAP_CAP: usize>(
    mut data: EkuboPoolData<TICK_CAP, BITMAP_CAP>,
    pool_id: [u8; 32],
    pool: &PoolMetadata,
    snapshot: &EkuboTickBitmapSnapshot,
) -> Option<EkuboPoolData<TICK_CAP, BITMAP_CAP>> {
    if snapshot.ticks.len() > TICK_CAP || snapshot.tick_bitmaps.len() > BITMAP_CAP {
        return None;
    }
    data.pool_id = pool_id;
    data.common.pool_id.copy_from_slice(&pool_id[..20]);
    data.common.is_active.store(true, Ordering::Release);
    data.token0 = pool.token0.into_array();
    data.token1 = pool.token1.into_array();
    data.token0_decimals = pool.token0_decimals?;
    data.token1_decimals = pool.token1_decimals?;
    data.fee = pool.ekubo_fee?;
    data.tick_spacing = pool.tick_spacing?;
    data.type_config = pool.ekubo_type_config?;
    data.sqrt_price_x96 = snapshot.sqrt_ratio;
    data.tick = snapshot.tick;
    data.liquidity = snapshot.liquidity;
    for (i, tick) in snapshot.ticks.iter().enumerate() {
        data.ticks[i] = *tick;
    }
    data.tick_count = snapshot.ticks.len() as u16;
    for (i, bitmap) in snapshot.tick_bitmaps.iter().enumerate() {
        data.tick_bitmap[i] = *bitmap;
    }
    data.bitmap_count = snapshot.tick_bitmaps.len() as u16;
    Some(data)
}

fn build_ekubo_pool(
    pool_id: [u8; 32],
    pool: &PoolMetadata,
    snapshot: &EkuboTickBitmapSnapshot,
) -> Option<AnyEkuboPool> {
    match determine_tier(snapshot.ticks.len(), snapshot.tick_bitmaps.len()) {
        PoolTier::Low => {
            fill_ekubo_pool(Default::default(), pool_id, pool, snapshot).map(AnyEkuboPool::Low)
        }
        PoolTier::Active => {
            fill_ekubo_pool(Default::default(), pool_id, pool, snapshot).map(AnyEkuboPool::Active)
        }
        PoolTier::Popular => {
            fill_ekubo_pool(Default::default(), pool_id, pool, snapshot).map(AnyEkuboPool::Popular)
        }
        PoolTier::Major => {
            fill_ekubo_pool(Default::default(), pool_id, pool, snapshot).map(AnyEkuboPool::Major)
        }
    }
}

/// Send final slot0 epilogue messages for all affected pools after a reorg.
///
/// Reads definitive post-reorg state from one held final-tip snapshot and sends
/// epilogue messages as the sole slot0 recovery mechanism.
fn send_slot0_finals(
    state: &dyn StateProvider,
    affected_pools: &HashSet<(PoolIdentifier, Protocol)>,
    exex: &LiquidityExEx,
    stream_seq: &mut u64,
    block_number: u64,
    block_timestamp: u64,
) {
    use events::EKUBO_CORE;
    use pool_tracker::UNISWAP_V4_POOL_MANAGER;

    let mut overrides_sent = 0u32;

    for (pool_id, protocol) in affected_pools {
        let slot0 = match (pool_id, protocol) {
            (PoolIdentifier::Address(addr), Protocol::UniswapV3) => read_v3_slot0(state, *addr),
            (PoolIdentifier::PoolId(id), Protocol::UniswapV4) => {
                read_v4_slot0(state, UNISWAP_V4_POOL_MANAGER, id)
            }
            (PoolIdentifier::PoolId(id), Protocol::Ekubo) => {
                read_ekubo_state(state, EKUBO_CORE, id)
            }
            _ => continue,
        };

        let Some((sqrt_price_x96, tick, liquidity)) = slot0 else {
            warn!(
                "Failed to read slot0 for {:?} during reorg override",
                pool_id
            );
            continue;
        };

        exex.send_reorg_epilogue(
            stream_seq,
            block_number,
            block_timestamp,
            ReorgEpilogueUpdate::Slot0Final {
                pool_id: pool_id.clone(),
                protocol: *protocol,
                state: Slot0State {
                    sqrt_price_x96,
                    liquidity,
                    tick,
                },
            },
        );
        overrides_sent += 1;
    }

    if overrides_sent > 0 {
        info!(
            "Sent {} slot0 final epilogue updates after reorg",
            overrides_sent
        );
    }
}

/// Record a V3/V4/Ekubo swap pool affected by a reorg (for slot0 override).
fn record_affected_slot0_pool(
    event: &PoolUpdateMessage,
    affected: &mut HashSet<(PoolIdentifier, Protocol)>,
) {
    let dominated_by_slot0 = matches!(
        event.update,
        PoolUpdate::V3Swap { .. } | PoolUpdate::V4Swap { .. } | PoolUpdate::EkuboSwap { .. }
    );
    if dominated_by_slot0 {
        affected.insert((event.pool_id.clone(), event.protocol));
    }
}

fn state_at_block<P: StateProviderFactory>(
    provider: &P,
    block_number: u64,
    context: &str,
) -> eyre::Result<reth_provider::StateProviderBox> {
    provider
        .history_by_block_number(block_number)
        .map_err(|e| eyre::eyre!("{context}: failed to open state at block {block_number}: {e}"))
}

/// Main ExEx entry point
async fn liquidity_exex<Node: FullNodeComponents>(mut ctx: ExExContext<Node>) -> eyre::Result<()> {
    info!("🚀 Liquidity ExEx starting");

    // Start Unix socket server
    let socket_server = PoolUpdateSocketServer::new()?;
    let socket_tx = socket_server.get_sender();

    // Spawn socket server task
    tokio::spawn(async move {
        if let Err(e) = socket_server.run().await {
            warn!("Socket server error: {}", e);
        }
    });

    // Open the shadow arena writer if SHADOW_ARENA_PATH is set (ITE-16). Disabled
    // by default — when unset the ExEx behaves exactly as before.
    let shadow = ShadowArena::from_env()?;

    // Initialize ExEx state
    let mut exex = LiquidityExEx::new(socket_tx, shadow);

    info!("Socket protocol configured: v2 (cutover, legacy v1 removed)");

    // Monotonic stream sequence for socket protocol messages.
    let mut stream_seq: u64 = 0;

    // Subscribe to NATS for whitelist updates
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
    let chain = std::env::var("CHAIN").unwrap_or_else(|_| "ethereum".to_string());

    info!("Connecting to NATS at {} for chain {}", nats_url, chain);
    info!("Enforcing whitelist startup barrier before block processing");

    // Hard startup barrier:
    // 1) connect NATS
    // 2) subscribe whitelist deltas
    // 3) request + apply full snapshot
    // Only then continue into block processing.
    let nats_client = loop {
        match WhitelistNatsClient::connect(&nats_url).await {
            Ok(client) => {
                info!("✅ NATS connected successfully");
                break client;
            }
            Err(e) => {
                warn!(error = %e, "Failed to connect to NATS, retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };

    let subscriber = loop {
        match nats_client.subscribe_whitelist(&chain).await {
            Ok(subscriber) => {
                info!(
                    "✅ Subscribed to canonical whitelist updates (.full/.add/.remove) for {}",
                    chain
                );
                break subscriber;
            }
            Err(e) => {
                warn!(error = %e, "Failed to subscribe to canonical whitelist updates, retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };

    let mut full_subscriber = loop {
        match nats_client.subscribe_full_whitelist(&chain).await {
            Ok(subscriber) => {
                info!(
                    "✅ Subscribed to rich full whitelist snapshots for {}",
                    chain
                );
                break subscriber;
            }
            Err(e) => {
                warn!(error = %e, "Failed to subscribe to rich full whitelist, retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    };

    // ── Startup: request canonical rich full whitelist snapshot ──────────
    loop {
        if let Err(e) = nats_client.request_reseed().await {
            warn!(error = %e, "Failed to request whitelist reseed, retrying in 2s");
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }

        match nats_client
            .next_full_snapshot(&mut full_subscriber, Duration::from_secs(10))
            .await
        {
            Ok(pools) => {
                let pool_count = pools.len();

                if pool_count == 0 {
                    warn!("Startup rich full snapshot contained zero pools, retrying in 2s");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }

                let fluid_addrs: Vec<Address> = pools
                    .iter()
                    .filter(|p| p.protocol == Protocol::Fluid)
                    .filter_map(|p| p.pool_id.as_address())
                    .collect();
                let rpc_url = std::env::var("RPC_URL")
                    .unwrap_or_else(|_| "http://localhost:8545".to_string());
                let startup_fluid_configs = if exex.shadow.is_some() && !fluid_addrs.is_empty() {
                    resolve_fluid_config_batch(fluid_addrs.clone(), &rpc_url).await
                } else {
                    Vec::new()
                };
                let fluid_config_map: HashMap<Address, FluidPoolConfig> = startup_fluid_configs
                    .iter()
                    .cloned()
                    .map(|config| (config.pool_address, config))
                    .collect();

                // 3b: hydrate shadow arena slots from one frozen startup anchor.
                hydrate_shadow_from_snapshot(&ctx, &pools, &fluid_config_map, exex.shadow.as_mut());

                let update = crate::pool_tracker::WhitelistUpdate::Replace(pools);
                {
                    let mut tracker = exex.pool_tracker.write().await;
                    tracker.queue_update(update);
                    for config in startup_fluid_configs.iter().cloned() {
                        tracker.register_fluid_config(config);
                    }
                }
                info!(
                    pools = pool_count,
                    "✅ Applied rich startup whitelist snapshot"
                );

                // Resolve any Fluid configs not already needed/resolved for shadow hydration.
                let resolved_fluid: HashSet<Address> = startup_fluid_configs
                    .iter()
                    .map(|config| config.pool_address)
                    .collect();
                let unresolved_fluid: Vec<Address> = fluid_addrs
                    .into_iter()
                    .filter(|addr| !resolved_fluid.contains(addr))
                    .collect();
                if !unresolved_fluid.is_empty() {
                    let pt = exex.pool_tracker.clone();
                    tokio::spawn(async move {
                        resolve_fluid_configs(unresolved_fluid, &rpc_url, pt).await;
                    });
                }

                break;
            }
            Err(e) => {
                warn!(error = %e, "Failed to receive rich startup whitelist snapshot, retrying in 2s");
            }
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Spawn task to handle whitelist updates with reconnect.
    let pool_tracker = exex.pool_tracker.clone();
    let chain_for_task = chain.clone();
    let rpc_url = std::env::var("RPC_URL").unwrap_or_else(|_| "http://localhost:8545".to_string());
    tokio::spawn(async move {
        let mut current_sub = subscriber;
        loop {
            while let Some(message) = current_sub.next().await {
                // Canonical subjects are `whitelist.pools.{chain}.{full,add,remove}`;
                // dispatch on the suffix. The legacy `.minimal` (also matched by the
                // wildcard subscription) returns None and is ignored.
                let suffix = message.subject.rsplit('.').next().unwrap_or("");
                match WhitelistNatsClient::canonical_update(suffix, &message.payload) {
                    Ok(Some(update)) => {
                        // Extract Fluid pool addresses before queueing
                        let fluid_addrs = extract_fluid_addresses(&update);
                        pool_tracker.write().await.queue_update(update);

                        // Resolve configs for new Fluid pools
                        if !fluid_addrs.is_empty() {
                            let pt = pool_tracker.clone();
                            let rpc = rpc_url.clone();
                            tokio::spawn(async move {
                                resolve_fluid_configs(fluid_addrs, &rpc, pt).await;
                            });
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!("Failed to handle whitelist message: {}", e);
                    }
                }
            }

            // Stream closed — attempt resubscribe with backoff
            warn!("Whitelist subscription closed, attempting resubscribe");
            let mut backoff = Duration::from_secs(1);
            loop {
                tokio::time::sleep(backoff).await;
                match nats_client.subscribe_whitelist(&chain_for_task).await {
                    Ok(new_sub) => {
                        info!("✅ Whitelist subscription restored");
                        current_sub = new_sub;
                        break;
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to resubscribe, retrying in {:?}", backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
        }
    });

    // Main event loop: receive notifications from Reth
    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                debug!(
                    "Processing committed chain with {} blocks",
                    new.blocks().len()
                );

                // Process each block with block boundaries.
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let base_fee_per_gas = block.base_fee_per_gas().unwrap_or(0);

                    // 🔒 Begin block - lock whitelist updates until block completes
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(
                        &mut stream_seq,
                        block_number,
                        block_timestamp,
                        base_fee_per_gas,
                        false,
                    );

                    let pool_tracker = exex.pool_tracker.read().await;
                    let state = state_at_block(ctx.provider(), block_number, "ChainCommitted")?;
                    let mut events_in_block = 0;
                    let mut logs_checked = 0;
                    let mut logs_matched_address = 0;
                    let mut logs_decoded = 0;
                    let mut fluid_touched: HashSet<Address> = HashSet::new();

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;
                            logs_checked += 1;

                            // Quick address filter (includes V2/V3 pools + PoolManager for V4 + Liquidity Layer for Fluid)
                            if !pool_tracker.is_tracked_address(&log_address) {
                                continue;
                            }
                            logs_matched_address += 1;

                            // For Fluid Liquidity Layer: pre-filter by indexed pool
                            // address in topics[1] before full ABI decode. The
                            // Liquidity Layer emits LogOperate for ALL protocols
                            // (fTokens, Vaults, etc.), not just our tracked DEX pools.
                            if log_address == pool_tracker::FLUID_LIQUIDITY_LAYER {
                                match fluid_log_operate_pool(log) {
                                    Some(pool) if pool_tracker.is_tracked_fluid_pool(&pool) => {
                                        // Collect touched pool — decode reserves after log loop
                                        fluid_touched.insert(pool);
                                        continue;
                                    }
                                    _ => continue,
                                }
                            }

                            // Decode event
                            let decoded_event = match decode_log(log) {
                                Some(event) => {
                                    logs_decoded += 1;
                                    event
                                }
                                None => continue,
                            };

                            // Check if we should process this specific event
                            // For V2/V3: checks pool address
                            // For V4: checks pool_id from event data (NOT PoolManager address)
                            if !exex.should_process_event(&decoded_event, &pool_tracker) {
                                continue;
                            }

                            // Create and send update
                            if let Some(update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                false,
                                state.as_ref(),
                                &pool_tracker,
                            ) {
                                exex.send_pool_update(&mut stream_seq, update_msg);

                                events_in_block += 1;
                                exex.events_processed += 1;
                            }
                        }
                    }

                    // ── Fluid batch decode ───────────────────────────────────
                    // For each Fluid pool touched in this block, read 8 storage
                    // slots from the state provider and decode reserves.
                    for pool_addr in &fluid_touched {
                        if let Some(config) = pool_tracker.fluid_config(pool_addr) {
                            match decode_fluid_pool(state.as_ref(), config, block_timestamp) {
                                Some(reserves) => {
                                    exex.send_pool_update(
                                        &mut stream_seq,
                                        fluid_update_msg(
                                            *pool_addr,
                                            &reserves,
                                            block_number,
                                            block_timestamp,
                                        ),
                                    );
                                    events_in_block += 1;
                                    exex.events_processed += 1;
                                    debug!(pool = %pool_addr, "Decoded Fluid reserves from storage");
                                }
                                None => {
                                    warn!(pool = %pool_addr, "Failed to decode Fluid reserves from storage");
                                }
                            }
                        } else {
                            debug!(pool = %pool_addr, "Fluid pool touched but no config cached — skipping");
                        }
                    }

                    // Release state/read lock before sending EndBlock and awaiting tracker writes.
                    drop(state);
                    drop(pool_tracker);

                    exex.send_end_block(&mut stream_seq, block_number, events_in_block);
                    exex.shadow_end_block(block_number);

                    // 🔓 End block - apply any pending whitelist updates atomically
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.end_block();
                    }

                    if events_in_block > 0 {
                        info!(
                            "Block {}: processed {} liquidity events",
                            block_number, events_in_block
                        );
                    }

                    // Debug logging every block for now
                    if logs_checked > 0 || events_in_block > 0 {
                        info!(
                            "🔍 Block {}: checked {} logs, {} matched address, {} decoded, {} events",
                            block_number, logs_checked, logs_matched_address, logs_decoded, events_in_block
                        );
                    }

                    exex.blocks_processed += 1;

                    // Log stats every 100 blocks
                    if exex.blocks_processed % 100 == 0 {
                        info!(
                            "Stats: {} blocks, {} events processed",
                            exex.blocks_processed, exex.events_processed
                        );

                        let pool_tracker = exex.pool_tracker.read().await;
                        let stats = pool_tracker.stats();
                        info!(
                            "Tracking: {} pools ({} V2, {} V3, {} V4)",
                            stats.total_pools, stats.v2_pools, stats.v3_pools, stats.v4_pools
                        );

                        if stats.total_pools == 0 {
                            warn!("⚠️  No pools in whitelist! Events will be filtered out.");
                            warn!("   Check that NATS whitelist updates are being received.");
                        }
                    }
                }
            }

            ExExNotification::ChainReorged { old, new } => {
                warn!(
                    "⚠️  Chain reorg detected: reverting {} old blocks, applying {} new blocks",
                    old.blocks().len(),
                    new.blocks().len()
                );

                let old_range = block_range_summary_from_numbers(old.blocks().keys().copied());
                let new_range = block_range_summary_from_numbers(new.blocks().keys().copied());
                let final_tip_block = new_range
                    .last_block
                    .or(old_range.last_block)
                    .unwrap_or_default();

                exex.send_reorg_start(&mut stream_seq, old_range.clone(), new_range.clone());

                let mut affected_slot0_pools: HashSet<(PoolIdentifier, Protocol)> = HashSet::new();
                let mut reorg_fluid_touched = HashSet::<Address>::new();

                // Step 1: Revert old blocks
                info!("Step 1: Reverting {} old blocks", old.blocks().len());
                for (block, receipts) in old.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let base_fee_per_gas = block.base_fee_per_gas().unwrap_or(0);

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(
                        &mut stream_seq,
                        block_number,
                        block_timestamp,
                        base_fee_per_gas,
                        true,
                    );

                    let pool_tracker = exex.pool_tracker.read().await;
                    // Reth exposes canonical post-reorg state here, not old-fork state.
                    // Absolute full-state revert messages therefore use this final-tip
                    // snapshot; reorg epilogues below remain the definitive recovery path.
                    let state =
                        state_at_block(ctx.provider(), final_tip_block, "ChainReorged revert")?;
                    let mut events_reverted = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

                            // Fluid: collect touched pools — will decode from
                            // post-reorg state after Step 2 (or after new-block
                            // processing removes them from the set).
                            if log_address == pool_tracker::FLUID_LIQUIDITY_LAYER {
                                if let Some(pool) = fluid_log_operate_pool(log) {
                                    if pool_tracker.is_tracked_fluid_pool(&pool) {
                                        reorg_fluid_touched.insert(pool);
                                    }
                                }
                                continue;
                            }

                            // Quick address filter (includes V2/V3 pools + PoolManager for V4)
                            if !pool_tracker.is_tracked_address(&log_address) {
                                continue;
                            }

                            // Decode event first
                            let decoded_event = match decode_log(log) {
                                Some(event) => event,
                                None => continue,
                            };

                            // Check if we should process this specific event
                            // For V2/V3: checks pool address
                            // For V4: checks pool_id from event data (NOT PoolManager address)
                            if !exex.should_process_event(&decoded_event, &pool_tracker) {
                                continue;
                            }

                            // Create and send revert update
                            if let Some(update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                true,
                                state.as_ref(),
                                &pool_tracker,
                            ) {
                                record_affected_slot0_pool(&update_msg, &mut affected_slot0_pools);
                                exex.send_pool_update(&mut stream_seq, update_msg);

                                events_reverted += 1;
                            }
                        }
                    }

                    drop(state);
                    drop(pool_tracker);

                    exex.send_end_block(&mut stream_seq, block_number, events_reverted);

                    // 🔓 End block - apply pending updates
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.end_block();
                    }

                    if events_reverted > 0 {
                        debug!(
                            "Block {}: reverted {} liquidity events",
                            block_number, events_reverted
                        );
                    }
                }

                // Step 2: Process new blocks (same as ChainCommitted, with Fluid batch decode).
                info!("Step 2: Processing {} new blocks", new.blocks().len());
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let base_fee_per_gas = block.base_fee_per_gas().unwrap_or(0);

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(
                        &mut stream_seq,
                        block_number,
                        block_timestamp,
                        base_fee_per_gas,
                        false,
                    );

                    let pool_tracker = exex.pool_tracker.read().await;
                    let state = state_at_block(ctx.provider(), block_number, "ChainReorged apply")?;
                    let mut events_in_block = 0;
                    let mut fluid_touched = HashSet::<Address>::new();

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

                            // Fluid Liquidity Layer: pre-filter + collect touched pools
                            if log_address == pool_tracker::FLUID_LIQUIDITY_LAYER {
                                if let Some(pool) = fluid_log_operate_pool(log) {
                                    if pool_tracker.is_tracked_fluid_pool(&pool) {
                                        fluid_touched.insert(pool);
                                    }
                                }
                                continue;
                            }

                            // Quick address filter (includes V2/V3 pools + PoolManager for V4)
                            if !pool_tracker.is_tracked_address(&log_address) {
                                continue;
                            }

                            // Decode event first
                            let decoded_event = match decode_log(log) {
                                Some(event) => event,
                                None => continue,
                            };

                            // Check if we should process this specific event
                            if !exex.should_process_event(&decoded_event, &pool_tracker) {
                                continue;
                            }

                            // Create and send update
                            if let Some(update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                false,
                                state.as_ref(),
                                &pool_tracker,
                            ) {
                                exex.send_pool_update(&mut stream_seq, update_msg);

                                events_in_block += 1;
                                exex.events_processed += 1;
                            }
                        }
                    }

                    // ── Fluid batch decode (same as ChainCommitted) ──────────
                    for pool_addr in &fluid_touched {
                        if let Some(config) = pool_tracker.fluid_config(pool_addr) {
                            match decode_fluid_pool(state.as_ref(), config, block_timestamp) {
                                Some(reserves) => {
                                    exex.send_pool_update(
                                        &mut stream_seq,
                                        fluid_update_msg(
                                            *pool_addr,
                                            &reserves,
                                            block_number,
                                            block_timestamp,
                                        ),
                                    );
                                    events_in_block += 1;
                                    exex.events_processed += 1;
                                }
                                None => {
                                    warn!(pool = %pool_addr, "Failed to decode Fluid reserves during reorg reapply");
                                }
                            }
                        }
                        // Pool handled in new chain — don't re-decode after Step 2
                        reorg_fluid_touched.remove(pool_addr);
                    }

                    drop(state);
                    drop(pool_tracker);

                    exex.send_end_block(&mut stream_seq, block_number, events_in_block);

                    // 🔓 End block - apply pending updates
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.end_block();
                    }

                    if events_in_block > 0 {
                        debug!(
                            "Block {}: processed {} liquidity events",
                            block_number, events_in_block
                        );
                    }

                    exex.blocks_processed += 1;
                }

                let final_state =
                    state_at_block(ctx.provider(), final_tip_block, "ChainReorged final")?;

                // ── Fluid: decode pools touched in old blocks but not new ──
                if !reorg_fluid_touched.is_empty() {
                    let pool_tracker = exex.pool_tracker.read().await;
                    let tip_timestamp = new
                        .blocks()
                        .values()
                        .last()
                        .map(|b| b.timestamp())
                        .unwrap_or_default();
                    for pool_addr in &reorg_fluid_touched {
                        if let Some(config) = pool_tracker.fluid_config(pool_addr) {
                            match decode_fluid_pool(final_state.as_ref(), config, tip_timestamp) {
                                Some(reserves) => {
                                    exex.send_reorg_epilogue(
                                        &mut stream_seq,
                                        final_tip_block,
                                        tip_timestamp,
                                        ReorgEpilogueUpdate::FluidStateFinal {
                                            pool_id: PoolIdentifier::Address(*pool_addr),
                                            state: fluid_state_from_reserves(&reserves),
                                        },
                                    );
                                    debug!(pool = %pool_addr, "Decoded Fluid reserves post-reorg epilogue (not in new chain)");
                                }
                                None => {
                                    warn!(pool = %pool_addr, "Failed to decode Fluid reserves post-reorg");
                                }
                            }
                        }
                    }
                    drop(pool_tracker);
                }

                // Send definitive slot0 overrides from the final-tip state snapshot.
                send_slot0_finals(
                    final_state.as_ref(),
                    &affected_slot0_pools,
                    &exex,
                    &mut stream_seq,
                    final_tip_block,
                    new.blocks()
                        .values()
                        .last()
                        .map(|b| b.timestamp())
                        .unwrap_or(0),
                );
                exex.send_reorg_complete(&mut stream_seq, final_tip_block);

                info!("✅ Reorg handled successfully");
            }

            ExExNotification::ChainReverted { old } => {
                warn!(
                    "⚠️  Chain reverted: reverting {} blocks",
                    old.blocks().len()
                );

                let old_range = block_range_summary_from_numbers(old.blocks().keys().copied());
                let final_tip_block = old_range
                    .first_block
                    .map(|n| n.saturating_sub(1))
                    .unwrap_or_default();

                exex.send_reorg_start(
                    &mut stream_seq,
                    old_range.clone(),
                    ReorgRange {
                        first_block: None,
                        last_block: None,
                        block_count: 0,
                    },
                );

                let mut affected_slot0_pools: HashSet<(PoolIdentifier, Protocol)> = HashSet::new();
                let mut revert_fluid_touched = HashSet::<Address>::new();
                // Reth exposes canonical post-revert state here, not the reverted-away
                // old blocks' state. Absolute full-state revert messages and final
                // epilogues both read this one final-tip snapshot.
                let final_state =
                    state_at_block(ctx.provider(), final_tip_block, "ChainReverted final")?;

                for (block, receipts) in old.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let base_fee_per_gas = block.base_fee_per_gas().unwrap_or(0);

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(
                        &mut stream_seq,
                        block_number,
                        block_timestamp,
                        base_fee_per_gas,
                        true,
                    );

                    let pool_tracker = exex.pool_tracker.read().await;
                    let mut events_reverted = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

                            // Fluid: collect touched pools — decode from
                            // post-revert state after the block loop.
                            if log_address == pool_tracker::FLUID_LIQUIDITY_LAYER {
                                if let Some(pool) = fluid_log_operate_pool(log) {
                                    if pool_tracker.is_tracked_fluid_pool(&pool) {
                                        revert_fluid_touched.insert(pool);
                                    }
                                }
                                continue;
                            }

                            if !pool_tracker.is_tracked_address(&log_address) {
                                continue;
                            }

                            let decoded_event = match decode_log(log) {
                                Some(event) => event,
                                None => continue,
                            };

                            // Filter by pool_id for V4 (same as Committed/Reorged paths)
                            if !exex.should_process_event(&decoded_event, &pool_tracker) {
                                continue;
                            }

                            if let Some(update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                true,
                                final_state.as_ref(),
                                &pool_tracker,
                            ) {
                                record_affected_slot0_pool(&update_msg, &mut affected_slot0_pools);
                                exex.send_pool_update(&mut stream_seq, update_msg);

                                events_reverted += 1;
                            }
                        }
                    }

                    drop(pool_tracker);

                    exex.send_end_block(&mut stream_seq, block_number, events_reverted);

                    // 🔓 End block - apply pending updates
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.end_block();
                    }

                    if events_reverted > 0 {
                        debug!(
                            "Block {}: reverted {} liquidity events",
                            block_number, events_reverted
                        );
                    }
                }

                // ── Fluid: decode touched pools from post-revert state ───
                if !revert_fluid_touched.is_empty() {
                    let pool_tracker = exex.pool_tracker.read().await;
                    // Provider reflects canonical state after revert
                    let tip_timestamp = old
                        .blocks()
                        .values()
                        .next()
                        .map(|b| b.timestamp())
                        .unwrap_or_default();
                    for pool_addr in &revert_fluid_touched {
                        if let Some(config) = pool_tracker.fluid_config(pool_addr) {
                            match decode_fluid_pool(final_state.as_ref(), config, tip_timestamp) {
                                Some(reserves) => {
                                    exex.send_reorg_epilogue(
                                        &mut stream_seq,
                                        final_tip_block,
                                        tip_timestamp,
                                        ReorgEpilogueUpdate::FluidStateFinal {
                                            pool_id: PoolIdentifier::Address(*pool_addr),
                                            state: fluid_state_from_reserves(&reserves),
                                        },
                                    );
                                    debug!(pool = %pool_addr, "Decoded Fluid reserves post-revert epilogue");
                                }
                                None => {
                                    warn!(pool = %pool_addr, "Failed to decode Fluid reserves post-revert");
                                }
                            }
                        }
                    }
                    drop(pool_tracker);
                }

                // Send definitive slot0 overrides from the final-tip state snapshot.
                send_slot0_finals(
                    final_state.as_ref(),
                    &affected_slot0_pools,
                    &exex,
                    &mut stream_seq,
                    final_tip_block,
                    0, // No new blocks in ChainReverted
                );
                exex.send_reorg_complete(&mut stream_seq, final_tip_block);

                info!("✅ Revert handled successfully");
            }
        }

        // Notify Reth that we've processed this notification
        if let Some(committed_chain) = notification.committed_chain() {
            ctx.events
                .send(ExExEvent::FinishedHeight(committed_chain.tip().num_hash()))?;
        }
    }

    Ok(())
}

#[inline]
/// Build a `PoolUpdateMessage` from decoded Fluid reserves.
fn fluid_state_from_reserves(reserves: &fluid_decoder::FluidReserves) -> FluidState {
    FluidState {
        col_token0_real: reserves.col_token0_real_reserves,
        col_token1_real: reserves.col_token1_real_reserves,
        col_token0_imaginary: reserves.col_token0_imaginary_reserves,
        col_token1_imaginary: reserves.col_token1_imaginary_reserves,
        debt_token0_real: reserves.debt_token0_real_reserves,
        debt_token1_real: reserves.debt_token1_real_reserves,
        debt_token0_imaginary: reserves.debt_token0_imaginary_reserves,
        debt_token1_imaginary: reserves.debt_token1_imaginary_reserves,
        center_price: reserves.center_price,
        fee: reserves.fee,
    }
}

fn fluid_update_msg(
    pool_addr: Address,
    reserves: &fluid_decoder::FluidReserves,
    block_number: u64,
    block_timestamp: u64,
) -> PoolUpdateMessage {
    PoolUpdateMessage {
        pool_id: PoolIdentifier::Address(pool_addr),
        protocol: Protocol::Fluid,
        update_type: UpdateType::Swap,
        block_number,
        block_timestamp,
        tx_index: 0,
        log_index: 0,
        is_revert: false,
        update: PoolUpdate::FluidState {
            state: fluid_state_from_reserves(reserves),
        },
    }
}

/// Extract Fluid pool addresses from a whitelist update.
fn extract_fluid_addresses(update: &pool_tracker::WhitelistUpdate) -> Vec<Address> {
    let pools = match update {
        pool_tracker::WhitelistUpdate::Add(p) | pool_tracker::WhitelistUpdate::Replace(p) => p,
        pool_tracker::WhitelistUpdate::Remove(_) => return vec![],
    };
    pools
        .iter()
        .filter(|p| p.protocol == Protocol::Fluid)
        .filter_map(|p| p.pool_id.as_address())
        .collect()
}

/// Resolve `FluidPoolConfig` for a batch of pool addresses via RPC.
async fn resolve_fluid_config_batch(addrs: Vec<Address>, rpc_url: &str) -> Vec<FluidPoolConfig> {
    info!("Resolving Fluid configs for {} pools via RPC", addrs.len());
    let mut configs = Vec::new();
    for addr in addrs {
        match FluidPoolConfig::resolve(addr, rpc_url).await {
            Ok(config) => {
                info!(pool = %addr, liquidity = %config.liquidity_address, "✅ Fluid config resolved");
                configs.push(config);
            }
            Err(e) => {
                warn!(pool = %addr, error = %e, "❌ Failed to resolve Fluid config");
            }
        }
    }
    configs
}

/// Resolve `FluidPoolConfig` for a batch of pool addresses via RPC and register them.
async fn resolve_fluid_configs(
    addrs: Vec<Address>,
    rpc_url: &str,
    pool_tracker: Arc<RwLock<PoolTracker>>,
) {
    let configs = resolve_fluid_config_batch(addrs, rpc_url).await;
    let mut tracker = pool_tracker.write().await;
    for config in configs {
        tracker.register_fluid_config(config);
    }
}

/// Read 8 storage slots from a held state snapshot and decode Fluid reserves.
fn decode_fluid_pool(
    state: &dyn StateProvider,
    config: &FluidPoolConfig,
    block_timestamp: u64,
) -> Option<fluid_decoder::FluidReserves> {
    let reads = config.storage_reads();
    let mut values = [U256::ZERO; 8];
    for (i, (addr, slot)) in reads.iter().enumerate() {
        match state.storage(*addr, (*slot).into()) {
            Ok(Some(v)) => values[i] = v,
            Ok(None) => values[i] = U256::ZERO,
            Err(e) => {
                warn!(pool = %config.pool_address, slot = %slot, error = %e, "Failed to read Fluid storage slot");
                return None;
            }
        }
    }

    let slots = config.to_storage_slots(&values);
    fluid_decoder::decode_fluid_reserves(&slots, config, block_timestamp)
}

fn next_stream_seq(counter: &mut u64) -> u64 {
    *counter = counter.wrapping_add(1);
    *counter
}

fn block_range_summary_from_numbers<I>(block_numbers: I) -> ReorgRange
where
    I: IntoIterator<Item = u64>,
{
    let mut first: Option<u64> = None;
    let mut last: Option<u64> = None;
    let mut count: u64 = 0;

    for n in block_numbers {
        count += 1;
        first = Some(first.map_or(n, |cur| cur.min(n)));
        last = Some(last.map_or(n, |cur| cur.max(n)));
    }

    ReorgRange {
        first_block: first,
        last_block: last,
        block_count: count,
    }
}

fn main() -> eyre::Result<()> {
    reth::cli::Cli::parse_args().run(|builder, _| async move {
        let handle = builder
            .node(EthereumNode::default())
            .install_exex("Liquidity", async move |ctx| Ok(liquidity_exex(ctx)))
            // .install_exex("Transfers", async move |ctx| Ok(transfers::transfers_exex(ctx)))
            .install_exex("BalanceMonitor", async move |ctx| {
                Ok(balance_monitor::balance_monitor_exex(ctx))
            })
            .launch()
            .await?;

        handle.wait_for_node_exit().await
    })
}

#[cfg(test)]
mod tests {
    use super::{
        determine_tier, extract_ekubo_ticks_from_bitmap, extract_ticks_from_bitmap_u256,
        twocrypto_storage_slots, v2_swap_deltas, v3_slots_for_factory, TwoCryptoStorageSlots,
        V3StorageSlots, PANCAKE_V3_FACTORY_ETHEREUM,
    };
    use alloy_primitives::{I256, U256};
    use arena_layout::PoolTier;

    #[test]
    fn twocrypto_storage_slots_follow_versioned_layouts() {
        assert_eq!(
            twocrypto_storage_slots(None),
            TwoCryptoStorageSlots {
                initial_a_gamma: 5,
                initial_a_gamma_time: 6,
                future_a_gamma: 7,
                future_a_gamma_time: 8,
                balance_0: 9,
                balance_1: 10,
                d: 11,
                packed_fee_params: 16,
            }
        );
        assert_eq!(
            twocrypto_storage_slots(Some("v2.0.0")),
            TwoCryptoStorageSlots {
                initial_a_gamma: 8,
                initial_a_gamma_time: 20,
                future_a_gamma: 10,
                future_a_gamma_time: 20,
                balance_0: 12,
                balance_1: 13,
                d: 14,
                packed_fee_params: 16,
            }
        );
    }

    #[test]
    fn v3_storage_slots_follow_factory_layouts() {
        assert_eq!(
            v3_slots_for_factory(None),
            V3StorageSlots {
                slot0: 0,
                liquidity: 4,
                ticks: 5,
                tick_bitmap: 6,
            }
        );
        assert_eq!(
            v3_slots_for_factory(Some(PANCAKE_V3_FACTORY_ETHEREUM)),
            V3StorageSlots {
                slot0: 0,
                liquidity: 5,
                ticks: 6,
                tick_bitmap: 7,
            }
        );
    }

    #[test]
    fn tick_bitmap_helpers_extract_initialized_ticks() {
        let mut bitmap = [0u8; 32];
        bitmap[31] = 0b0010_0001;
        assert_eq!(extract_ticks_from_bitmap_u256(0, &bitmap, 60), vec![0, 300]);

        let mut ekubo_bitmap = [0u8; 32];
        ekubo_bitmap[31] = 0b0000_0001;
        let ticks = extract_ekubo_ticks_from_bitmap(349_303, &ekubo_bitmap, 10);
        assert_eq!(ticks, vec![-1_270]);
    }

    #[test]
    fn determine_tier_uses_tick_and_bitmap_capacity() {
        assert_eq!(determine_tier(50, 10), PoolTier::Low);
        assert_eq!(determine_tier(51, 10), PoolTier::Active);
        assert_eq!(determine_tier(200, 26), PoolTier::Popular);
        assert_eq!(determine_tier(501, 51), PoolTier::Major);
    }

    #[test]
    fn v2_swap_deltas_handle_standard_one_sided_swap() {
        let (delta0, delta1) = v2_swap_deltas(
            U256::from(1_000u64),
            U256::ZERO,
            U256::ZERO,
            U256::from(500u64),
        );

        assert_eq!(delta0, I256::try_from(1_000u64).unwrap());
        assert_eq!(delta1, -I256::try_from(500u64).unwrap());
    }

    #[test]
    fn v2_swap_deltas_keep_rare_nonzero_amount1_in_on_token0_to_token1_swap() {
        let (delta0, delta1) = v2_swap_deltas(
            U256::from(4_630_623_146_782_210_569u128),
            U256::from(100u64),
            U256::ZERO,
            U256::from(8_101_991_724u64),
        );

        assert_eq!(
            delta0,
            I256::from_raw(U256::from(4_630_623_146_782_210_569u128))
        );
        assert_eq!(
            delta1,
            I256::try_from(100u64).unwrap() - I256::try_from(8_101_991_724u64).unwrap()
        );
    }
}
