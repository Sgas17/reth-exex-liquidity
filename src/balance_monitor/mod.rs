//! Balance Monitor ExEx.
//!
//! Monitors ERC20 Transfer events to/from a configured executor address,
//! maintains running token balances, and publishes updates to NATS.
//!
//! Token tracking set is append-only (persisted to JSON) and populated from
//! whitelist NATS subscription. Initial balances are seeded from Reth DB.

pub mod slots;
pub mod token_tracker;

use alloy_consensus::{BlockHeader, TxReceipt};
use alloy_primitives::{Address, Log, U256};
use futures::{StreamExt, TryStreamExt};
use reth_exex::{ExExContext, ExExEvent, ExExNotification};
use reth_node_api::{FullNodeComponents, NodePrimitives, NodeTypes};
use reth::providers::StateProviderFactory;
use std::collections::HashMap;
use std::path::PathBuf;
use token_tracker::TokenTracker;
use tracing::{debug, info, warn};

use crate::transfers::events::decode_transfer;

/// NATS message: per-token balance update (published on change).
#[derive(Debug, Clone, serde::Serialize)]
pub struct BalanceUpdate {
    pub chain: String,
    pub token: String,
    pub balance: String,
    pub decimals: u8,
    pub block_number: u64,
    pub ts: u64,
}

/// Run the balance monitor ExEx.
pub async fn balance_monitor_exex<Node>(mut ctx: ExExContext<Node>) -> eyre::Result<()>
where
    Node: FullNodeComponents,
    Node::Provider: StateProviderFactory,
{
    info!("Balance Monitor ExEx starting");

    // ── Config ──────────────────────────────────────────────────────────

    let executor_address: Address = std::env::var("BALANCE_MONITOR_ADDRESS")
        .map_err(|_| eyre::eyre!("BALANCE_MONITOR_ADDRESS env var required"))?
        .parse()
        .map_err(|e| eyre::eyre!("invalid BALANCE_MONITOR_ADDRESS: {e}"))?;

    let chain_id =
        std::env::var("BALANCE_MONITOR_CHAIN_ID").unwrap_or_else(|_| "1".to_string());

    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());

    let chain = std::env::var("CHAIN").unwrap_or_else(|_| "ethereum".to_string());

    // Derive persist path from reth datadir.
    let persist_path = std::env::var("BALANCE_MONITOR_PERSIST_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = ctx.config.datadir().data_dir().to_path_buf();
            p.push("exex");
            p.push("balance_monitor_tokens.json");
            p
        });

    let nats_subject = format!("balances.chain.{chain_id}");

    info!(
        executor = %executor_address,
        chain_id = %chain_id,
        persist_path = %persist_path.display(),
        nats_subject = %nats_subject,
        "balance monitor config"
    );

    // ── NATS ────────────────────────────────────────────────────────────

    let nats_client = async_nats::connect(&nats_url).await?;
    info!("NATS connected for balance monitor");

    // ── Token tracker ───────────────────────────────────────────────────

    let mut tracker = TokenTracker::new(persist_path);

    // ── Whitelist subscription (for token discovery) ────────────────────

    let whitelist_subject = format!("whitelist.pools.{chain}.full");
    let mut whitelist_sub = nats_client.subscribe(whitelist_subject.clone()).await?;
    info!(subject = %whitelist_subject, "subscribed to whitelist for token discovery");

    // ── In-memory balance map ───────────────────────────────────────────

    let mut balances: HashMap<Address, U256> = HashMap::new();

    // Seed existing tracked tokens from Reth DB.
    seed_balances_from_db(ctx.provider(), executor_address, &tracker, &mut balances)?;
    info!(
        tokens = tracker.len(),
        "seeded initial balances from Reth DB"
    );

    // ── Stats ───────────────────────────────────────────────────────────

    let mut blocks_processed: u64 = 0;
    let mut updates_published: u64 = 0;

    // ── Main loop ───────────────────────────────────────────────────────

    loop {
        tokio::select! {
            // ExEx block notifications
            notification = ctx.notifications.try_next() => {
                let notification = match notification? {
                    Some(n) => n,
                    None => break, // stream ended
                };

                let changed = process_notification(
                    &notification,
                    executor_address,
                    &tracker,
                    &mut balances,
                );

                // Publish updates for changed tokens.
                if !changed.is_empty() {
                    for token in &changed {
                        let balance = balances.get(token).copied().unwrap_or(U256::ZERO);
                        let decimals = tracker.decimals(token).unwrap_or(18);
                        let block_number = notification_tip_block(&notification);

                        let update = BalanceUpdate {
                            chain: chain_id.clone(),
                            token: format!("{token:#x}"),
                            balance: balance.to_string(),
                            decimals,
                            block_number,
                            ts: now_ms(),
                        };

                        let payload = serde_json::to_vec(&update)
                            .expect("BalanceUpdate serializes");
                        if let Err(e) = nats_client
                            .publish(nats_subject.clone(), payload.into())
                            .await
                        {
                            warn!(error = %e, token = %token, "failed to publish balance update");
                        } else {
                            updates_published += 1;
                        }
                    }

                    debug!(
                        changed = changed.len(),
                        block = notification_tip_block(&notification),
                        "published balance updates"
                    );
                }

                // Acknowledge processed height.
                if let Some(committed_chain) = notification.committed_chain() {
                    ctx.events
                        .send(ExExEvent::FinishedHeight(committed_chain.tip().num_hash()))?;
                }

                blocks_processed += 1;
                if blocks_processed % 100 == 0 {
                    info!(
                        blocks = blocks_processed,
                        updates = updates_published,
                        tokens = tracker.len(),
                        "balance monitor stats"
                    );
                }
            }

            // Whitelist updates (token discovery)
            msg = whitelist_sub.next() => {
                if let Some(msg) = msg {
                    let new_tokens = process_whitelist_message(
                        &msg.payload,
                        &mut tracker,
                    );

                    // Seed balances for newly discovered tokens.
                    if !new_tokens.is_empty() {
                        for &token in &new_tokens {
                            if let Err(e) = seed_token_balance(
                                ctx.provider(),
                                executor_address,
                                token,
                                &mut balances,
                            ) {
                                warn!(error = %e, token = %token, "failed to seed balance for new token");
                            }
                        }
                        info!(
                            new_tokens = new_tokens.len(),
                            total = tracker.len(),
                            "discovered tokens from whitelist"
                        );
                    }
                }
            }
        }
    }

    info!("Balance Monitor ExEx exiting");
    Ok(())
}

// ─── Block processing ────────────────────────────────────────────────────────

/// Process a notification and return the set of tokens whose balances changed.
fn process_notification<N: NodePrimitives<Receipt: TxReceipt<Log = Log>>>(
    notification: &ExExNotification<N>,
    executor: Address,
    tracker: &TokenTracker,
    balances: &mut HashMap<Address, U256>,
) -> Vec<Address> {
    let mut changed = Vec::new();

    match notification {
        ExExNotification::ChainCommitted { new } => {
            for (_block, receipts) in new.blocks_and_receipts() {
                process_receipts(receipts, executor, tracker, balances, &mut changed, false);
            }
        }
        ExExNotification::ChainReorged { old, new } => {
            // Revert old blocks.
            for (_block, receipts) in old.blocks_and_receipts() {
                process_receipts(receipts, executor, tracker, balances, &mut changed, true);
            }
            // Apply new blocks.
            for (_block, receipts) in new.blocks_and_receipts() {
                process_receipts(receipts, executor, tracker, balances, &mut changed, false);
            }
        }
        ExExNotification::ChainReverted { old } => {
            for (_block, receipts) in old.blocks_and_receipts() {
                process_receipts(receipts, executor, tracker, balances, &mut changed, true);
            }
        }
    }

    changed.sort_unstable();
    changed.dedup();
    changed
}

fn process_receipts<R: TxReceipt<Log = alloy_primitives::Log>>(
    receipts: &[R],
    executor: Address,
    tracker: &TokenTracker,
    balances: &mut HashMap<Address, U256>,
    changed: &mut Vec<Address>,
    is_revert: bool,
) {
    for receipt in receipts {
        for log in receipt.logs() {
            let transfer = match decode_transfer(log) {
                Some(t) => t,
                None => continue,
            };

            // Only care about transfers involving our executor.
            let is_incoming = transfer.to == executor;
            let is_outgoing = transfer.from == executor;
            if !is_incoming && !is_outgoing {
                continue;
            }

            // Only care about tracked tokens.
            if !tracker.contains(&transfer.token) {
                continue;
            }

            let entry = balances.entry(transfer.token).or_insert(U256::ZERO);

            // Apply or revert the delta.
            if is_revert {
                // Undo: incoming was an add, so subtract; outgoing was a subtract, so add.
                if is_incoming {
                    *entry = entry.saturating_sub(transfer.value);
                } else {
                    *entry = entry.saturating_add(transfer.value);
                }
            } else {
                if is_incoming {
                    *entry = entry.saturating_add(transfer.value);
                } else {
                    *entry = entry.saturating_sub(transfer.value);
                }
            }

            changed.push(transfer.token);
        }
    }
}

// ─── Balance seeding ─────────────────────────────────────────────────────────

fn seed_balances_from_db<P: StateProviderFactory>(
    provider: &P,
    executor: Address,
    tracker: &TokenTracker,
    balances: &mut HashMap<Address, U256>,
) -> eyre::Result<()> {
    let state = provider.latest()?;
    for (&token, _decimals) in tracker.iter() {
        let slot = slots::balance_storage_slot(token, executor);
        let value = state.storage(token, slot.into())?.unwrap_or(U256::ZERO);
        balances.insert(token, value);
        debug!(token = %token, balance = %value, "seeded balance from DB");
    }
    Ok(())
}

fn seed_token_balance<P: StateProviderFactory>(
    provider: &P,
    executor: Address,
    token: Address,
    balances: &mut HashMap<Address, U256>,
) -> eyre::Result<()> {
    let state = provider.latest()?;
    let slot = slots::balance_storage_slot(token, executor);
    let value = state.storage(token, slot.into())?.unwrap_or(U256::ZERO);
    balances.insert(token, value);
    debug!(token = %token, balance = %value, "seeded balance for new token");
    Ok(())
}

// ─── Whitelist processing ────────────────────────────────────────────────────

/// Minimal whitelist pool entry — only need token addresses and decimals.
#[derive(Debug, serde::Deserialize)]
struct WhitelistFullMessage {
    #[serde(default)]
    pools: Vec<WhitelistPoolEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct WhitelistPoolEntry {
    #[serde(default)]
    token0: Option<TokenEntry>,
    #[serde(default)]
    token1: Option<TokenEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct TokenEntry {
    address: String,
    #[serde(default = "default_decimals")]
    decimals: u8,
}

fn default_decimals() -> u8 {
    18
}

/// Extract new tokens from a whitelist message. Returns addresses of newly added tokens.
fn process_whitelist_message(payload: &[u8], tracker: &mut TokenTracker) -> Vec<Address> {
    let msg: WhitelistFullMessage = match serde_json::from_slice(payload) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "failed to parse whitelist message");
            return Vec::new();
        }
    };

    let mut new_tokens = Vec::new();

    for pool in &msg.pools {
        if let Some(ref t) = pool.token0 {
            if let Ok(addr) = t.address.parse::<Address>() {
                if tracker.add(addr, t.decimals) {
                    new_tokens.push(addr);
                }
            }
        }
        if let Some(ref t) = pool.token1 {
            if let Ok(addr) = t.address.parse::<Address>() {
                if tracker.add(addr, t.decimals) {
                    new_tokens.push(addr);
                }
            }
        }
    }

    new_tokens
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn notification_tip_block<N: NodePrimitives>(notification: &ExExNotification<N>) -> u64 {
    match notification {
        ExExNotification::ChainCommitted { new } => {
            new.tip().number()
        }
        ExExNotification::ChainReorged { new, .. } => {
            new.tip().number()
        }
        ExExNotification::ChainReverted { old } => {
            old.tip().number()
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
