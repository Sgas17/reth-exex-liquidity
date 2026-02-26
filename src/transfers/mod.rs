mod aggregator;
mod db;
pub mod events;

use alloy_consensus::{transaction::TxHashRef, BlockHeader, TxReceipt};
use db::{TransferDb, TransferRow};
use events::decode_transfer;
use futures::TryStreamExt;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::{BlockBody, FullNodeComponents};
use std::sync::Arc;
use tracing::{debug, info, warn};

pub async fn transfers_exex<Node: FullNodeComponents>(
    mut ctx: ExExContext<Node>,
) -> eyre::Result<()> {
    info!("Transfers ExEx starting");

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://transfers_user:transfers_pass@localhost:5433/transfers".to_string()
    });
    let db = Arc::new(TransferDb::new(&database_url).await?);
    info!("Connected to PostgreSQL");

    // Temporarily disable expensive transfer aggregation while node catches up.
    // Keep daily cleanup enabled so table size remains bounded.
    // aggregator::spawn_aggregator(db.clone());
    aggregator::spawn_cleanup(db.clone());
    info!("Transfers aggregation task is disabled");

    let mut blocks_processed: u64 = 0;
    let mut total_transfers: u64 = 0;

    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let mut rows: Vec<TransferRow> = Vec::new();

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        let tx_hash: [u8; 32] = block
                            .body()
                            .transactions()
                            .get(tx_index)
                            .map(|tx| tx.tx_hash().0)
                            .unwrap_or_default();

                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            if let Some(t) = decode_transfer(log) {
                                rows.push(TransferRow {
                                    block_number,
                                    tx_hash: format!("0x{}", hex::encode(tx_hash)),
                                    log_index: log_index as u32,
                                    token_address: format!("0x{}", hex::encode(t.token.0 .0)),
                                    from_address: format!("0x{}", hex::encode(t.from.0 .0)),
                                    to_address: format!("0x{}", hex::encode(t.to.0 .0)),
                                    amount_str: t.value.to_string(),
                                    block_timestamp,
                                });
                            }
                        }
                    }

                    if !rows.is_empty() {
                        let count = rows.len();
                        let mut inserted = false;
                        for attempt in 1..=3 {
                            match db.insert_transfers(&rows).await {
                                Ok(()) => {
                                    total_transfers += count as u64;
                                    debug!("Block {}: inserted {} transfers", block_number, count);
                                    inserted = true;
                                    break;
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to insert {} transfers for block {} (attempt {}/3): {}",
                                        count, block_number, attempt, e
                                    );
                                    if attempt < 3 {
                                        tokio::time::sleep(std::time::Duration::from_secs(
                                            attempt as u64 * 2,
                                        ))
                                        .await;
                                    }
                                }
                            }
                        }
                        if !inserted {
                            warn!("Giving up on block {} after 3 retries", block_number);
                        }
                    }

                    blocks_processed += 1;
                    if blocks_processed % 100 == 0 {
                        info!(
                            "Stats: {} blocks processed, {} total transfers inserted",
                            blocks_processed, total_transfers
                        );
                    }
                }
            }

            ExExNotification::ChainReorged { old, new } => {
                warn!(
                    "Chain reorg: reverting {} blocks, applying {} new",
                    old.blocks().len(),
                    new.blocks().len()
                );

                for (block, _) in old.blocks_and_receipts() {
                    match db.delete_block(block.number()).await {
                        Ok(deleted) if deleted > 0 => {
                            debug!(
                                "Reverted block {}: deleted {} transfers",
                                block.number(),
                                deleted
                            );
                        }
                        Err(e) => {
                            warn!("Failed to delete reverted block {}: {}", block.number(), e);
                        }
                        _ => {}
                    }
                }

                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let block_timestamp = block.timestamp();
                    let mut rows: Vec<TransferRow> = Vec::new();

                    for (tx_index, receipt) in receipts.iter().enumerate() {
                        let tx_hash: [u8; 32] = block
                            .body()
                            .transactions()
                            .get(tx_index)
                            .map(|tx| tx.tx_hash().0)
                            .unwrap_or_default();

                        for (log_index, log) in receipt.logs().iter().enumerate() {
                            if let Some(t) = decode_transfer(log) {
                                rows.push(TransferRow {
                                    block_number,
                                    tx_hash: format!("0x{}", hex::encode(tx_hash)),
                                    log_index: log_index as u32,
                                    token_address: format!("0x{}", hex::encode(t.token.0 .0)),
                                    from_address: format!("0x{}", hex::encode(t.from.0 .0)),
                                    to_address: format!("0x{}", hex::encode(t.to.0 .0)),
                                    amount_str: t.value.to_string(),
                                    block_timestamp,
                                });
                            }
                        }
                    }

                    if !rows.is_empty() {
                        for attempt in 1..=3 {
                            match db.insert_transfers(&rows).await {
                                Ok(()) => break,
                                Err(e) => {
                                    warn!(
                                        "Failed to insert transfers for reorged block {} (attempt {}/3): {}",
                                        block_number, attempt, e
                                    );
                                    if attempt < 3 {
                                        tokio::time::sleep(std::time::Duration::from_secs(
                                            attempt as u64 * 2,
                                        ))
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                    blocks_processed += 1;
                }
            }

            ExExNotification::ChainReverted { old } => {
                warn!("Chain reverted: {} blocks", old.blocks().len());
                for (block, _) in old.blocks_and_receipts() {
                    match db.delete_block(block.number()).await {
                        Ok(deleted) if deleted > 0 => {
                            debug!(
                                "Reverted block {}: deleted {} transfers",
                                block.number(),
                                deleted
                            );
                        }
                        Err(e) => {
                            warn!("Failed to delete reverted block {}: {}", block.number(), e);
                        }
                        _ => {}
                    }
                }
            }
        }

        if let Some(committed_chain) = notification.committed_chain() {
            ctx.events
                .send(ExExEvent::FinishedHeight(committed_chain.tip().num_hash()))?;
        }
    }

    Ok(())
}
