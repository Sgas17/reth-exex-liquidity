// Reth ExEx: Liquidity Pool Event Decoder with Unix Socket Output
//
// This ExEx:
// 1. Subscribes to pool whitelist updates from dynamicWhitelist (via NATS or file)
// 2. Decodes Uniswap V2/V3/V4 Swap/Mint/Burn events from tracked pools
// 3. Sends pool state updates via Unix Domain Socket to orderbook engine
//
// Architecture:
//   Reth ExEx → Event Decoder → Pool State Extractor → Unix Socket → Orderbook Engine

mod events;
mod nats_client;
mod pool_tracker;
mod socket;
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
use types::{ControlMessage, PoolIdentifier, PoolUpdate, PoolUpdateMessage, Protocol, UpdateType};

/// Main ExEx state
struct LiquidityExEx {
    /// Pool tracker (shared, can be updated from whitelist subscription)
    pool_tracker: Arc<RwLock<PoolTracker>>,

    /// Socket sender for outgoing messages
    socket_tx: tokio::sync::mpsc::UnboundedSender<ControlMessage>,

    /// Statistics
    events_processed: u64,
    blocks_processed: u64,
}

impl LiquidityExEx {
    fn new(socket_tx: tokio::sync::mpsc::UnboundedSender<ControlMessage>) -> Self {
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
                    update: PoolUpdate::V2Swap {
                        amount0,
                        amount1,
                    },
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
                    liquidity_delta: amount as i128, // Mint is positive
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
                    liquidity_delta: -(amount as i128), // Burn is negative
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
                    debug!("Filtered V4 event from untracked pool_id: {:?}", hex::encode(pool_id));
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
                Ok(mut subscriber) => {
                    info!("✅ Subscribed to whitelist updates for {}", chain);

                    // Spawn task to handle whitelist updates
                    let pool_tracker = exex.pool_tracker.clone();
                    tokio::spawn(async move {
                        while let Some(message) = subscriber.next().await {
                            match nats_client.parse_message(&message.payload) {
                                Ok(whitelist_msg) => {
                                    match nats_client.convert_to_pool_update(whitelist_msg) {
                                        Ok(update) => {
                                            // Queue the differential update
                                            pool_tracker
                                                .write()
                                                .await
                                                .queue_update(update);
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

                    // Send BeginBlock marker
                    if let Err(e) = exex.socket_tx.send(ControlMessage::BeginBlock {
                        block_number,
                        block_timestamp,
                        is_revert: false,
                    }) {
                        warn!("Failed to send BeginBlock: {}", e);
                    }

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
                                },
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
                                if let Err(e) =
                                    exex.socket_tx.send(ControlMessage::PoolUpdate(update_msg))
                                {
                                    warn!("Failed to send pool update: {}", e);
                                }

                                events_in_block += 1;
                                exex.events_processed += 1;
                            }
                        }
                    }

                    // Release read lock before sending EndBlock
                    drop(pool_tracker);

                    // Send EndBlock marker
                    if let Err(e) = exex.socket_tx.send(ControlMessage::EndBlock {
                        block_number,
                        num_updates: events_in_block,
                    }) {
                        warn!("Failed to send EndBlock: {}", e);
                    }

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

                    if let Err(e) = exex.socket_tx.send(ControlMessage::BeginBlock {
                        block_number,
                        block_timestamp,
                        is_revert: true,
                    }) {
                        warn!("Failed to send BeginBlock: {}", e);
                    }

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
                                if let Err(e) =
                                    exex.socket_tx.send(ControlMessage::PoolUpdate(update_msg))
                                {
                                    warn!("Failed to send pool revert: {}", e);
                                }

                                events_reverted += 1;
                            }
                        }
                    }

                    drop(pool_tracker);

                    if let Err(e) = exex.socket_tx.send(ControlMessage::EndBlock {
                        block_number,
                        num_updates: events_reverted,
                    }) {
                        warn!("Failed to send EndBlock: {}", e);
                    }

                    // 🔓 End block - apply pending updates
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.end_block();
                    }

                    if events_reverted > 0 {
                        debug!("Block {}: reverted {} liquidity events", block_number, events_reverted);
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

                    if let Err(e) = exex.socket_tx.send(ControlMessage::BeginBlock {
                        block_number,
                        block_timestamp,
                        is_revert: false,
                    }) {
                        warn!("Failed to send BeginBlock: {}", e);
                    }

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
                                if let Err(e) =
                                    exex.socket_tx.send(ControlMessage::PoolUpdate(update_msg))
                                {
                                    warn!("Failed to send pool update: {}", e);
                                }

                                events_in_block += 1;
                                exex.events_processed += 1;
                            }
                        }
                    }

                    drop(pool_tracker);

                    if let Err(e) = exex.socket_tx.send(ControlMessage::EndBlock {
                        block_number,
                        num_updates: events_in_block,
                    }) {
                        warn!("Failed to send EndBlock: {}", e);
                    }

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

                info!("✅ Reorg handled successfully");
            }

            ExExNotification::ChainReverted { old } => {
                warn!("⚠️  Chain reverted: reverting {} blocks", old.blocks().len());

                for (block, receipts) in old.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();

                    // 🔒 Begin block
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.begin_block();
                    }

                    if let Err(e) = exex.socket_tx.send(ControlMessage::BeginBlock {
                        block_number,
                        block_timestamp,
                        is_revert: true,
                    }) {
                        warn!("Failed to send BeginBlock: {}", e);
                    }

                    let pool_tracker = exex.pool_tracker.read().await;
                    let mut events_reverted = 0;

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            let log_address = log.address;

                            if !pool_tracker.is_tracked_address(&log_address) {
                                continue;
                            }

                            if let Some(decoded_event) = decode_log(log) {
                                if let Some(update_msg) = exex.create_pool_update(
                                    decoded_event,
                                    block_number,
                                    block_timestamp,
                                    tx_index as u64,
                                    log_index as u64,
                                    true,
                                    &pool_tracker,
                                ) {
                                    if let Err(e) =
                                        exex.socket_tx.send(ControlMessage::PoolUpdate(update_msg))
                                    {
                                        warn!("Failed to send pool revert: {}", e);
                                    }

                                    events_reverted += 1;
                                }
                            }
                        }
                    }

                    drop(pool_tracker);

                    if let Err(e) = exex.socket_tx.send(ControlMessage::EndBlock {
                        block_number,
                        num_updates: events_reverted,
                    }) {
                        warn!("Failed to send EndBlock: {}", e);
                    }

                    // 🔓 End block - apply pending updates
                    {
                        let mut pool_tracker = exex.pool_tracker.write().await;
                        pool_tracker.end_block();
                    }

                    if events_reverted > 0 {
                        debug!("Block {}: reverted {} liquidity events", block_number, events_reverted);
                    }
                }

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

fn main() -> eyre::Result<()> {
    reth::cli::Cli::parse_args().run(|builder, _| async move {
        let handle = builder
            .node(EthereumNode::default())
            .install_exex("Liquidity", async move |ctx| Ok(liquidity_exex(ctx)))
            .launch()
            .await?;

        handle.wait_for_node_exit().await
    })
}
