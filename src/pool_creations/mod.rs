mod db;
mod events;

use alloy_consensus::{BlockHeader, TxReceipt};
use db::{PoolDb, PoolRow};
use events::decode_pool_creation;
use futures::TryStreamExt;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::FullNodeComponents;
use std::sync::Arc;
use tracing::{debug, info, warn};

fn build_pool_rows(
    log: &alloy_primitives::Log,
    block_number: u64,
) -> Option<PoolRow> {
    let decoded = decode_pool_creation(log)?;

    Some(PoolRow {
        address: decoded.pool_address,
        factory: format!("{:#x}", decoded.factory),
        asset0: format!("{:#x}", decoded.token0),
        asset1: format!("{:#x}", decoded.token1),
        creation_block: block_number,
        fee: decoded.fee,
        tick_spacing: decoded.tick_spacing,
        additional_data: decoded.additional_data,
    })
}

pub async fn pool_creations_exex<Node: FullNodeComponents>(
    mut ctx: ExExContext<Node>,
) -> eyre::Result<()> {
    info!("Pool Creations ExEx starting");

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://transfers_user:transfers_pass@localhost:5433/transfers".to_string()
    });
    let db = Arc::new(PoolDb::new(&database_url).await?);

    let mut blocks_processed: u64 = 0;
    let mut total_pools: u64 = 0;

    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number();
                    let mut rows: Vec<PoolRow> = Vec::new();

                    for receipt in receipts.iter() {
                        for log in receipt.logs() {
                            if let Some(row) = build_pool_rows(log, block_number) {
                                rows.push(row);
                            }
                        }
                    }

                    if !rows.is_empty() {
                        let count = rows.len();
                        match db.insert_pools(&rows).await {
                            Ok(()) => {
                                total_pools += count as u64;
                                debug!("Block {}: inserted {} pool creations", block_number, count);
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to insert {} pools for block {}: {}",
                                    count, block_number, e
                                );
                            }
                        }
                    }

                    blocks_processed += 1;
                    if blocks_processed % 1000 == 0 {
                        info!(
                            "Stats: {} blocks processed, {} total pool creations inserted",
                            blocks_processed, total_pools
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
                                "Reverted block {}: deleted {} pool creations",
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
                    let mut rows: Vec<PoolRow> = Vec::new();

                    for receipt in receipts.iter() {
                        for log in receipt.logs() {
                            if let Some(row) = build_pool_rows(log, block_number) {
                                rows.push(row);
                            }
                        }
                    }

                    if !rows.is_empty() {
                        if let Err(e) = db.insert_pools(&rows).await {
                            warn!(
                                "Failed to insert pools for reorged block {}: {}",
                                block_number, e
                            );
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
                                "Reverted block {}: deleted {} pool creations",
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
