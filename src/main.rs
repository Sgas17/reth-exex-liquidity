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
mod transfers;
mod types;

use alloy_consensus::{BlockHeader, TxReceipt};
use alloy_primitives::{I256, U256};
use events::{decode_log, DecodedEvent};
use nats_client::WhitelistNatsClient;
// Removed: use eyre::Result; (unused import)
use futures::{StreamExt, TryStreamExt};
use pool_tracker::PoolTracker;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::FullNodeComponents;
use reth_node_ethereum::EthereumNode;
use socket::PoolUpdateSocketServer;
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
        slot0_resync_required: Vec<PoolIdentifier>,
    ) {
        let seq = next_stream_seq(stream_seq);
        if let Err(e) = self.socket_tx.try_send(ControlMessage::ReorgComplete {
            stream_seq: seq,
            final_tip_block,
            slot0_resync_required,
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
            }
        }

        should_process
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

                // Process each block with block boundaries
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();

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
                            if let Some(update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                false,
                                &pool_tracker,
                            ) {
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

                let mut slot0_resync_required: Vec<PoolIdentifier> = Vec::new();

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
                                maybe_record_slot0_resync_pool(
                                    &update_msg,
                                    &mut slot0_resync_required,
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

                // Step 2: Process new blocks
                info!("Step 2: Processing {} new blocks", new.blocks().len());
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();

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
                            if let Some(update_msg) = exex.create_pool_update(
                                decoded_event,
                                block_number,
                                block_timestamp,
                                tx_index as u64,
                                log_index as u64,
                                false,
                                &pool_tracker,
                            ) {
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

                sort_pool_identifiers_deterministically(&mut slot0_resync_required);
                exex.send_reorg_complete(&mut stream_seq, final_tip_block, slot0_resync_required);

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

                let mut slot0_resync_required: Vec<PoolIdentifier> = Vec::new();

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
                                maybe_record_slot0_resync_pool(
                                    &update_msg,
                                    &mut slot0_resync_required,
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

                sort_pool_identifiers_deterministically(&mut slot0_resync_required);
                exex.send_reorg_complete(&mut stream_seq, final_tip_block, slot0_resync_required);

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

fn maybe_record_slot0_resync_pool(
    update: &PoolUpdateMessage,
    slot0_resync_required: &mut Vec<PoolIdentifier>,
) {
    if !update.is_revert {
        return;
    }

    let needs_resync = matches!(
        update.update,
        PoolUpdate::V3Swap { .. } | PoolUpdate::V4Swap { .. }
    );

    if !needs_resync {
        return;
    }

    if slot0_resync_required
        .iter()
        .any(|existing| pool_identifier_eq(existing, &update.pool_id))
    {
        return;
    }

    slot0_resync_required.push(update.pool_id.clone());
}

fn pool_identifier_eq(lhs: &PoolIdentifier, rhs: &PoolIdentifier) -> bool {
    match (lhs, rhs) {
        (PoolIdentifier::Address(a), PoolIdentifier::Address(b)) => a == b,
        (PoolIdentifier::PoolId(a), PoolIdentifier::PoolId(b)) => a == b,
        _ => false,
    }
}

fn sort_pool_identifiers_deterministically(pool_ids: &mut [PoolIdentifier]) {
    pool_ids.sort_by_key(pool_identifier_sort_key);
}

fn pool_identifier_sort_key(pool_id: &PoolIdentifier) -> String {
    match pool_id {
        PoolIdentifier::Address(addr) => format!("a:{}", hex::encode(addr.0)),
        PoolIdentifier::PoolId(id) => format!("p:{}", hex::encode(id)),
    }
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
