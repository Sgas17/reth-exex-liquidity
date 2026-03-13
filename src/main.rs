// Reth ExEx: Liquidity Pool Event Decoder with Unix Socket Output
//
// This ExEx:
// 1. Subscribes to pool whitelist updates from dynamicWhitelist (via NATS or file)
// 2. Decodes Uniswap V2/V3/V4 Swap/Mint/Burn events from tracked pools
// 3. Sends pool state updates via Unix Domain Socket to orderbook engine
//
// Architecture:
//   Reth ExEx → Event Decoder → Pool State Extractor → Unix Socket → Orderbook Engine

mod balance_monitor;
mod events;
mod nats_client;
mod pool_creations;
mod pool_tracker;
mod socket;
mod swap_monitor;
mod transfers;
mod types;

use alloy_consensus::{BlockHeader, TxReceipt};
use alloy_primitives::{Address, I256, U256};
use events::{decode_log, DecodedEvent};
use nats_client::WhitelistNatsClient;
// Removed: use eyre::Result; (unused import)
use futures::{StreamExt, TryStreamExt};
use pool_tracker::PoolTracker;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::FullNodeComponents;
use reth_node_ethereum::EthereumNode;
use reth_provider::StateProviderFactory;
use socket::PoolUpdateSocketServer;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use types::{
    ControlMessage, PoolIdentifier, PoolUpdate, PoolUpdateMessage, Protocol, ReorgRange, UpdateType,
};

/// Main ExEx state
struct LiquidityExEx {
    /// Pool tracker (shared, can be updated from whitelist subscription)
    pool_tracker: Arc<RwLock<PoolTracker>>,

    /// Socket sender for outgoing messages
    socket_tx: tokio::sync::mpsc::Sender<ControlMessage>,

    /// Statistics
    events_processed: u64,
    blocks_processed: u64,
}

impl LiquidityExEx {
    fn new(socket_tx: tokio::sync::mpsc::Sender<ControlMessage>) -> Self {
        Self {
            pool_tracker: Arc::new(RwLock::new(PoolTracker::new())),
            socket_tx,
            events_processed: 0,
            blocks_processed: 0,
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
                // Calculate signed reserve deltas for V2 swaps
                // Match Python logic: if amount0In == 0, use (negative out, positive in), else (positive in, negative out)
                let (amount0, amount1) = if amount0_in == U256::ZERO {
                    // Token1 -> Token0 swap: token0 going OUT (negative), token1 coming IN (positive)
                    let delta0 = -I256::try_from(amount0_out).unwrap_or(I256::ZERO);
                    let delta1 = I256::try_from(amount1_in).unwrap_or(I256::ZERO);
                    (delta0, delta1)
                } else {
                    // Token0 -> Token1 swap: token0 coming IN (positive), token1 going OUT (negative)
                    let delta0 = I256::try_from(amount0_in).unwrap_or(I256::ZERO);
                    let delta1 = -I256::try_from(amount1_out).unwrap_or(I256::ZERO);
                    (delta0, delta1)
                };

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
                    liquidity_delta: i128::try_from(amount)
                        .map(|v| -v)
                        .unwrap_or_else(|_| {
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
            DecodedEvent::CurveSwap {
                pool,
                sold_id,
                tokens_sold,
                bought_id,
                tokens_bought,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::CurveStable,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::CurveSwap {
                    sold_id,
                    tokens_sold,
                    bought_id,
                    tokens_bought,
                },
            }),

            DecodedEvent::CurveLiquidityChange { pool } => {
                // Liquidity events don't carry enough info for delta tracking.
                // Signal the arena to re-scrape this pool's balances from storage.
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::CurveStable,
                    update_type: UpdateType::Mint, // Generic — arena re-scrapes regardless
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::CurveLiquidity {
                        effective_balances: vec![], // Empty — arena will re-scrape
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
            DecodedEvent::TwoCryptoSwap {
                pool,
                sold_id,
                tokens_sold,
                bought_id,
                tokens_bought,
                packed_price_scale,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::CurveTwoCrypto,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::TwoCryptoSwap {
                    sold_id,
                    tokens_sold,
                    bought_id,
                    tokens_bought,
                    packed_price_scale,
                    d: U256::ZERO, // Enriched from storage after creation
                },
            }),

            DecodedEvent::TwoCryptoLiquidityChange { pool } => {
                Some(PoolUpdateMessage {
                    pool_id: PoolIdentifier::Address(pool),
                    protocol: Protocol::CurveTwoCrypto,
                    update_type: UpdateType::Mint,
                    block_number,
                    block_timestamp,
                    tx_index,
                    log_index,
                    is_revert,
                    update: PoolUpdate::TwoCryptoLiquidity {
                        balances: [0; 2], // Empty — arena will re-scrape
                    },
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
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::CurveTwoCrypto,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::TwoCryptoRampAgamma {
                    initial_a,
                    future_a,
                    initial_gamma,
                    future_gamma,
                    initial_time,
                    future_time,
                },
            }),

            DecodedEvent::TwoCryptoNewParameters {
                pool,
                mid_fee,
                out_fee,
                fee_gamma,
            } => Some(PoolUpdateMessage {
                pool_id: PoolIdentifier::Address(pool),
                protocol: Protocol::CurveTwoCrypto,
                update_type: UpdateType::Swap,
                block_number,
                block_timestamp,
                tx_index,
                log_index,
                is_revert,
                update: PoolUpdate::TwoCryptoNewParameters {
                    mid_fee,
                    out_fee,
                    fee_gamma,
                },
            }),
        }
    }

    fn send_begin_block(
        &self,
        stream_seq: &mut u64,
        block_number: u64,
        block_timestamp: u64,
        is_revert: bool,
    ) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::BeginBlock {
            stream_seq: seq,
            block_number,
            block_timestamp,
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

    fn send_reorg_complete(
        &self,
        stream_seq: &mut u64,
        final_tip_block: u64,
    ) {
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
            DecodedEvent::CurveSwap { pool, .. }
            | DecodedEvent::CurveLiquidityChange { pool, .. }
            | DecodedEvent::CurveRampA { pool, .. }
            | DecodedEvent::CurveApplyNewFee { pool, .. } => {
                pool_tracker.is_tracked_address(pool)
            }

            // Curve TwoCrypto events: check pool address
            DecodedEvent::TwoCryptoSwap { pool, .. }
            | DecodedEvent::TwoCryptoLiquidityChange { pool, .. }
            | DecodedEvent::TwoCryptoRampAgamma { pool, .. }
            | DecodedEvent::TwoCryptoNewParameters { pool, .. } => {
                pool_tracker.is_tracked_address(pool)
            }
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
                DecodedEvent::CurveSwap { pool, .. }
                | DecodedEvent::CurveLiquidityChange { pool, .. }
                | DecodedEvent::CurveRampA { pool, .. }
                | DecodedEvent::CurveApplyNewFee { pool, .. } => {
                    debug!("Filtered CurveStable event from untracked pool: {:?}", pool);
                }
                DecodedEvent::TwoCryptoSwap { pool, .. }
                | DecodedEvent::TwoCryptoLiquidityChange { pool, .. }
                | DecodedEvent::TwoCryptoRampAgamma { pool, .. }
                | DecodedEvent::TwoCryptoNewParameters { pool, .. } => {
                    debug!("Filtered CurveTwoCrypto event from untracked pool: {:?}", pool);
                }
            }
        }

        should_process
    }
}

/// Curve pool storage slot for D (TwoCrypto slot 14, Tricrypto slot 14).
const CURVE_D_SLOT: U256 = U256::from_limbs([14, 0, 0, 0]);

/// Read a single storage slot from the state at a given block.
///
/// Returns `U256::ZERO` if the slot is empty or the read fails.
fn read_storage_slot<P: StateProviderFactory>(
    provider: &P,
    address: Address,
    slot: U256,
) -> U256 {
    use alloy_primitives::B256;
    use reth_provider::StateProvider;
    let slot_key: B256 = B256::from(slot);
    match provider.latest() {
        Ok(state) => match state.storage(address, slot_key.into()) {
            Ok(Some(value)) => value,
            Ok(None) => U256::ZERO,
            Err(e) => {
                warn!("Failed to read storage slot {} for {:?}: {}", slot, address, e);
                U256::ZERO
            }
        },
        Err(e) => {
            warn!("Failed to get latest state provider: {}", e);
            U256::ZERO
        }
    }
}

/// Enrich a pool update message with storage-derived fields.
///
/// For Curve TwoCrypto/Tricrypto swap events, reads D from storage (slot 14)
/// to avoid newton_D recomputation on the arena side.
fn enrich_with_storage<P: StateProviderFactory>(
    msg: &mut PoolUpdateMessage,
    provider: &P,
) {
    match &mut msg.update {
        PoolUpdate::TwoCryptoSwap { d, .. } => {
            if let Some(address) = msg.pool_id.as_address() {
                *d = read_storage_slot(provider, address, CURVE_D_SLOT);
            }
        }
        // Future: PoolUpdate::TricryptoSwap { d, .. } => { ... }
        _ => {}
    }
}

/// V3 storage slots.
const V3_SLOT0: U256 = U256::from_limbs([0, 0, 0, 0]);
const V3_LIQUIDITY: U256 = U256::from_limbs([4, 0, 0, 0]);

/// V4 PoolManager mapping slot (pools mapping at slot 6).
const V4_POOLS_SLOT: U256 = U256::from_limbs([6, 0, 0, 0]);

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
    use alloy_primitives::{B256, keccak256};
    use alloy_sol_types::SolValue;
    let encoded = (B256::from_slice(pool_id), V4_POOLS_SLOT).abi_encode();
    U256::from_be_bytes(*keccak256(&encoded))
}

/// Read slot0 override for a V3 pool from latest state.
fn read_v3_slot0<P: StateProviderFactory>(
    provider: &P,
    address: Address,
) -> Option<(U256, i32, u128)> {
    let slot0_raw = read_storage_slot(provider, address, V3_SLOT0);
    if slot0_raw.is_zero() {
        return None;
    }
    let (sqrt_price_x96, tick) = decode_slot0_packed(slot0_raw);
    let liquidity_raw = read_storage_slot(provider, address, V3_LIQUIDITY);
    let liquidity = liquidity_raw.to::<u128>();
    Some((sqrt_price_x96, tick, liquidity))
}

/// Read slot0 override for a V4 pool from latest state.
fn read_v4_slot0<P: StateProviderFactory>(
    provider: &P,
    pool_manager: Address,
    pool_id: &[u8; 32],
) -> Option<(U256, i32, u128)> {
    let base = v4_pool_base_slot(pool_id);
    // slot0 at base + 0, liquidity at base + 3
    let slot0_key = U256::from_be_bytes(base.to_be_bytes::<32>());
    let liquidity_key = slot0_key + U256::from(3);

    let slot0_raw = read_storage_slot(provider, pool_manager, slot0_key);
    if slot0_raw.is_zero() {
        return None;
    }
    let (sqrt_price_x96, tick) = decode_slot0_packed(slot0_raw);
    let liquidity_raw = read_storage_slot(provider, pool_manager, liquidity_key);
    let liquidity = liquidity_raw.to::<u128>();
    Some((sqrt_price_x96, tick, liquidity))
}

/// Read state for an Ekubo pool from latest state.
fn read_ekubo_state<P: StateProviderFactory>(
    provider: &P,
    ekubo_core: Address,
    pool_id: &[u8; 32],
) -> Option<(U256, i32, u128)> {
    use alloy_primitives::B256;
    let state_slot = U256::from_be_bytes(*B256::from_slice(pool_id));
    let state_raw = read_storage_slot(provider, ekubo_core, state_slot);
    if state_raw.is_zero() {
        return None;
    }
    let (sqrt_ratio, tick, liquidity) = decode_ekubo_state_packed(state_raw);
    Some((sqrt_ratio, tick, liquidity))
}

/// Send Slot0Override messages for all affected pools after a reorg.
///
/// Reads definitive post-reorg state from `latest()` storage and sends
/// override messages, replacing the old `slot0_resync_required` mechanism.
fn send_slot0_overrides<P: StateProviderFactory>(
    provider: &P,
    affected_pools: &HashSet<(PoolIdentifier, Protocol)>,
    exex: &LiquidityExEx,
    stream_seq: &mut u64,
    block_number: u64,
    block_timestamp: u64,
) {
    use pool_tracker::UNISWAP_V4_POOL_MANAGER;
    use events::EKUBO_CORE;

    let mut overrides_sent = 0u32;

    for (pool_id, protocol) in affected_pools {
        let slot0 = match (pool_id, protocol) {
            (PoolIdentifier::Address(addr), Protocol::UniswapV3) => {
                read_v3_slot0(provider, *addr)
            }
            (PoolIdentifier::PoolId(id), Protocol::UniswapV4) => {
                read_v4_slot0(provider, UNISWAP_V4_POOL_MANAGER, id)
            }
            (PoolIdentifier::PoolId(id), Protocol::Ekubo) => {
                read_ekubo_state(provider, EKUBO_CORE, id)
            }
            _ => continue,
        };

        let Some((sqrt_price_x96, tick, liquidity)) = slot0 else {
            warn!("Failed to read slot0 for {:?} during reorg override", pool_id);
            continue;
        };

        let update_msg = PoolUpdateMessage {
            pool_id: pool_id.clone(),
            protocol: *protocol,
            update_type: UpdateType::Swap, // Reuses swap path in arena
            block_number,
            block_timestamp,
            tx_index: u64::MAX, // Sentinel: synthetic override, not from a real tx
            log_index: u64::MAX,
            is_revert: false,
            update: PoolUpdate::Slot0Override {
                sqrt_price_x96,
                liquidity,
                tick,
            },
        };

        exex.send_pool_update(stream_seq, update_msg);
        overrides_sent += 1;
    }

    if overrides_sent > 0 {
        info!("Sent {} slot0 overrides after reorg", overrides_sent);
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

    // Initialize ExEx state
    let mut exex = LiquidityExEx::new(socket_tx);

    info!("Socket protocol configured: v2 (cutover, legacy v1 removed)");

    // Monotonic stream sequence for socket protocol messages.
    let mut stream_seq: u64 = 0;

    // Subscribe to NATS for whitelist updates
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
    let chain = std::env::var("CHAIN").unwrap_or_else(|_| "ethereum".to_string());

    info!("Connecting to NATS at {} for chain {}", nats_url, chain);

    match WhitelistNatsClient::connect(&nats_url).await {
        Ok(nats_client) => {
            info!("✅ NATS connected successfully");

            // Subscribe to whitelist updates
            match nats_client.subscribe_whitelist(&chain).await {
                Ok(subscriber) => {
                    info!("✅ Subscribed to whitelist updates for {}", chain);

                    // Spawn task to handle whitelist updates with reconnect
                    let pool_tracker = exex.pool_tracker.clone();
                    let chain_for_task = chain.clone();
                    tokio::spawn(async move {
                        let mut current_sub = subscriber;
                        loop {
                            while let Some(message) = current_sub.next().await {
                                match nats_client.parse_message(&message.payload) {
                                    Ok(whitelist_msg) => {
                                        match nats_client.convert_to_pool_update(whitelist_msg) {
                                            Ok(update) => {
                                                pool_tracker.write().await.queue_update(update);
                                            }
                                            Err(e) => {
                                                warn!("Failed to convert whitelist message: {}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to parse whitelist message: {}", e);
                                    }
                                }
                            }

                            // Stream closed — attempt resubscribe with backoff
                            warn!("Whitelist subscription closed, attempting resubscribe");
                            let mut backoff = std::time::Duration::from_secs(1);
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
                                        backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                                    }
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    warn!("Failed to subscribe to NATS: {}", e);
                    info!("⚠️  Starting with empty pool whitelist");
                }
            }
        }
        Err(e) => {
            warn!("Failed to connect to NATS: {}", e);
            info!("⚠️  Starting with empty pool whitelist");
            info!("   Set NATS_URL environment variable to enable whitelist updates");
        }
    }

    // Main event loop: receive notifications from Reth
    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                debug!(
                    "Processing committed chain with {} blocks",
                    new.blocks().len()
                );

                // Process each block with block boundaries.
                // Storage enrichment (`D` for Curve crypto pools) is only applied on
                // the final block in the notification batch. Consumers only read after
                // arena processing completes, so per-intermediate-block `D` accuracy
                // is intentionally skipped to reduce state reads.
                let total_new_blocks = new.blocks().len();
                for (block_idx, (block, receipts)) in new.blocks_and_receipts().enumerate() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let enrich_storage_for_block = block_idx + 1 == total_new_blocks;

                    // 🔒 Begin block - lock whitelist updates until block completes
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(&mut stream_seq, block_number, block_timestamp, false);

                    let pool_tracker = exex.pool_tracker.read().await;
                    let mut events_in_block = 0;
                    let mut logs_checked = 0;
                    let mut logs_matched_address = 0;
                    let mut logs_decoded = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;
                            logs_checked += 1;

                            // Quick address filter (includes V2/V3 pools + PoolManager for V4)
                            if !pool_tracker.is_tracked_address(&log_address) {
                                continue;
                            }
                            logs_matched_address += 1;

                            // Decode event first
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
                            if let Some(mut update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                false,
                                &pool_tracker,
                            ) {
                                if enrich_storage_for_block {
                                    enrich_with_storage(&mut update_msg, ctx.provider());
                                }
                                exex.send_pool_update(&mut stream_seq, update_msg);

                                events_in_block += 1;
                                exex.events_processed += 1;
                            }
                        }
                    }

                    // Release read lock before sending EndBlock
                    drop(pool_tracker);

                    exex.send_end_block(&mut stream_seq, block_number, events_in_block);

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

                // Step 1: Revert old blocks
                info!("Step 1: Reverting {} old blocks", old.blocks().len());
                for (block, receipts) in old.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(&mut stream_seq, block_number, block_timestamp, true);

                    let pool_tracker = exex.pool_tracker.read().await;
                    let mut events_reverted = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

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
                                &pool_tracker,
                            ) {
                                record_affected_slot0_pool(
                                    &update_msg,
                                    &mut affected_slot0_pools,
                                );
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

                // Step 2: Process new blocks.
                // Same policy as ChainCommitted: only enrich storage-derived fields
                // on the final block in this batch.
                info!("Step 2: Processing {} new blocks", new.blocks().len());
                let total_new_blocks = new.blocks().len();
                for (block_idx, (block, receipts)) in new.blocks_and_receipts().enumerate() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let enrich_storage_for_block = block_idx + 1 == total_new_blocks;

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(&mut stream_seq, block_number, block_timestamp, false);

                    let pool_tracker = exex.pool_tracker.read().await;
                    let mut events_in_block = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

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

                            // Create and send update
                            if let Some(mut update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                false,
                                &pool_tracker,
                            ) {
                                if enrich_storage_for_block {
                                    enrich_with_storage(&mut update_msg, ctx.provider());
                                }
                                exex.send_pool_update(&mut stream_seq, update_msg);

                                events_in_block += 1;
                                exex.events_processed += 1;
                            }
                        }
                    }

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

                // Send definitive slot0 overrides from latest() state
                send_slot0_overrides(
                    ctx.provider(),
                    &affected_slot0_pools,
                    &exex,
                    &mut stream_seq,
                    final_tip_block,
                    new.blocks().values().last().map(|b| b.timestamp()).unwrap_or(0),
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

                for (block, receipts) in old.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    exex.send_begin_block(&mut stream_seq, block_number, block_timestamp, true);

                    let pool_tracker = exex.pool_tracker.read().await;
                    let mut events_reverted = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

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
                                &pool_tracker,
                            ) {
                                record_affected_slot0_pool(
                                    &update_msg,
                                    &mut affected_slot0_pools,
                                );
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

                // Send definitive slot0 overrides from latest() state
                send_slot0_overrides(
                    ctx.provider(),
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
            .install_exex("PoolCreations", async move |ctx| {
                Ok(pool_creations::pool_creations_exex(ctx))
            })
            .launch()
            .await?;

        handle.wait_for_node_exit().await
    })
}
