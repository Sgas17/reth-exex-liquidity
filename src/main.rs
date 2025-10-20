// Minimal Uniswap V3 Liquidity Event ExEx
//
// This ExEx subscribes to Reth notifications and decodes Mint/Burn events
// from tracked Uniswap V3 pools, printing them to console.
//
// Phase 1 Goal: Verify we can decode liquidity events in real-time

use alloy_sol_types::{sol, SolEvent};
use futures::TryStreamExt;
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::FullNodeComponents;
use reth_node_ethereum::EthereumNode;
use reth_tracing::tracing::info;
use std::collections::HashSet;

// Define Uniswap V3 liquidity events using Alloy's sol! macro
sol! {
    /// Emitted when liquidity is added to a position
    event Mint(
        address indexed sender,
        address indexed owner,
        int24 indexed tickLower,
        int24 tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );

    /// Emitted when liquidity is removed from a position
    event Burn(
        address indexed owner,
        int24 indexed tickLower,
        int24 tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );
}

/// High-volume Uniswap V3 pools to track (hardcoded for Phase 1)
const TRACKED_POOLS: &[&str] = &[
    "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640", // USDC/WETH 0.05%
    "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8", // USDC/WETH 0.3%
    "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed", // WBTC/WETH 0.3%
    "0x4e68ccd3e89f51c3074ca5072bbac773960dfa36", // WETH/USDT 0.3%
];

/// Convert pool addresses to a HashSet for fast lookups
fn get_tracked_pool_set() -> HashSet<String> {
    TRACKED_POOLS
        .iter()
        .map(|addr| addr.to_lowercase())
        .collect()
}

/// Main ExEx logic
async fn liquidity_exex<Node: FullNodeComponents>(
    mut ctx: ExExContext<Node>,
) -> eyre::Result<()> {
    let tracked_pools = get_tracked_pool_set();

    info!("Liquidity ExEx started");
    info!("Tracking {} pools", tracked_pools.len());
    for pool in TRACKED_POOLS {
        info!("  - {}", pool);
    }

    // Main event loop: receive notifications from Reth
    while let Some(notification) = ctx.notifications.try_next().await? {
        match &notification {
            ExExNotification::ChainCommitted { new } => {
                info!("Processing committed chain with {} blocks", new.blocks().len());

                // Process each block in the committed chain
                for (block, receipts) in new.blocks_and_receipts() {
                    let block_number = block.number;
                    let block_hash = block.hash();
                    let block_timestamp = block.timestamp;

                    // Track events found in this block
                    let mut mint_count = 0;
                    let mut burn_count = 0;

                    // Process each transaction's receipts
                    for receipt in receipts.iter() {
                        // Process each log in the receipt
                        for log in receipt.logs.iter() {
                            let log_address = format!("{:#x}", log.address);

                            // Filter: only process logs from tracked pools
                            if !tracked_pools.contains(&log_address.to_lowercase()) {
                                continue;
                            }

                            // Try to decode as Mint event
                            if let Ok(mint_event) = Mint::decode_log_data(log, true) {
                                mint_count += 1;
                                info!(
                                    "🟢 MINT | Block {} | Pool {} | Owner {} | Ticks [{}, {}] | Amount {} | Amount0 {} | Amount1 {}",
                                    block_number,
                                    log_address,
                                    mint_event.owner,
                                    mint_event.tickLower,
                                    mint_event.tickUpper,
                                    mint_event.amount,
                                    mint_event.amount0,
                                    mint_event.amount1
                                );
                                continue;
                            }

                            // Try to decode as Burn event
                            if let Ok(burn_event) = Burn::decode_log_data(log, true) {
                                burn_count += 1;
                                info!(
                                    "🔴 BURN | Block {} | Pool {} | Owner {} | Ticks [{}, {}] | Amount {} | Amount0 {} | Amount1 {}",
                                    block_number,
                                    log_address,
                                    burn_event.owner,
                                    burn_event.tickLower,
                                    burn_event.tickUpper,
                                    burn_event.amount,
                                    burn_event.amount0,
                                    burn_event.amount1
                                );
                                continue;
                            }
                        }
                    }

                    // Log block summary if we found events
                    if mint_count > 0 || burn_count > 0 {
                        info!(
                            "📊 Block {} summary: {} Mints, {} Burns (timestamp: {})",
                            block_number, mint_count, burn_count, block_timestamp
                        );
                    }
                }
            }
            ExExNotification::ChainReorged { old, new } => {
                info!(
                    "⚠️  Chain reorg detected: old chain {} blocks, new chain {} blocks",
                    old.blocks().len(),
                    new.blocks().len()
                );
                // Phase 1: Just log reorgs, don't handle them yet
            }
            ExExNotification::ChainReverted { old } => {
                info!(
                    "⚠️  Chain reverted: {} blocks",
                    old.blocks().len()
                );
                // Phase 1: Just log reverts, don't handle them yet
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
            .install_exex("Liquidity", liquidity_exex)
            .launch()
            .await?;

        handle.wait_for_node_exit().await
    })
}
