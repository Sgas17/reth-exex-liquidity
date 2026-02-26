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
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::PathBuf;
use token_tracker::TokenTracker;
use tracing::{debug, info, warn};

use crate::transfers::events::decode_transfer;

/// NATS message matching `ChainBalanceSnapshot` schema in defi_arb_rust/common.
///
/// The hedger deserializes this as `ChainBalanceSnapshot`, so field names and
/// serde formats must match exactly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChainBalanceSnapshot {
    pub chain: String,
    pub balances: Vec<ChainTokenBalance>,
    pub ts: u64,
}

/// Per-token balance entry matching `ChainTokenBalance` in common/messages.rs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChainTokenBalance {
    pub token: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub available: Decimal,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "rust_decimal::serde::str_option")]
    pub total: Option<Decimal>,
}

/// Convert a raw U256 balance to a human-readable Decimal given token decimals.
///
/// E.g. U256(1_000_000) with 6 decimals → Decimal(1.000000)
pub fn u256_to_decimal(raw: U256, decimals: u8) -> Decimal {
    // U256::to_string gives a base-10 integer string. Parse into Decimal, then
    // shift by `decimals` places. Decimal can hold up to 28-29 significant digits
    // which covers all realistic ERC20 balances (U256 max is 78 digits, but real
    // balances are much smaller).
    let s = raw.to_string();
    let d = match Decimal::from_str_exact(&s) {
        Ok(d) => d,
        Err(_) => {
            // Overflow: balance exceeds Decimal range (~7.9e28). Extremely
            // unlikely for real tokens. Clamp to MAX.
            warn!(raw = %s, decimals, "U256 exceeds Decimal range, clamping");
            return Decimal::MAX;
        }
    };
    // Decimal::new(1, scale) gives 10^(-scale). Multiply to shift decimal point.
    // E.g. 1_000_000 * 10^(-6) = 1.000000
    let scale = Decimal::new(1, decimals as u32);
    d.checked_mul(scale).unwrap_or(Decimal::MAX)
}

/// How often to publish a full snapshot of all balances (in blocks).
/// Acts as a resync mechanism if individual publishes are lost.
const FULL_SNAPSHOT_INTERVAL: u64 = 50;

/// Build a full snapshot of all tracked token balances.
fn build_full_snapshot(
    chain_id: &str,
    tracker: &TokenTracker,
    balances: &HashMap<Address, U256>,
) -> ChainBalanceSnapshot {
    let entries: Vec<ChainTokenBalance> = tracker
        .iter()
        .map(|(&token, &decimals)| {
            let raw = balances.get(&token).copied().unwrap_or(U256::ZERO);
            ChainTokenBalance {
                token: format!("{token:#x}"),
                available: u256_to_decimal(raw, decimals),
                total: None,
            }
        })
        .collect();

    ChainBalanceSnapshot {
        chain: chain_id.to_string(),
        balances: entries,
        ts: now_ms(),
    }
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
    let mut whitelist_sub = Some(nats_client.subscribe(whitelist_subject.clone()).await?);
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

                // Publish snapshot for changed tokens.
                if !changed.is_empty() {
                    let entries: Vec<ChainTokenBalance> = changed
                        .iter()
                        .map(|token| {
                            let raw = balances.get(token).copied().unwrap_or(U256::ZERO);
                            let decimals = tracker.decimals(token).unwrap_or(18);
                            ChainTokenBalance {
                                token: format!("{token:#x}"),
                                available: u256_to_decimal(raw, decimals),
                                total: None,
                            }
                        })
                        .collect();

                    let snapshot = ChainBalanceSnapshot {
                        chain: chain_id.clone(),
                        balances: entries,
                        ts: now_ms(),
                    };

                    let payload = serde_json::to_vec(&snapshot)
                        .expect("ChainBalanceSnapshot serializes");
                    if let Err(e) = nats_client
                        .publish(nats_subject.clone(), payload.into())
                        .await
                    {
                        warn!(error = %e, "failed to publish balance snapshot");
                    } else {
                        updates_published += changed.len() as u64;
                    }

                    debug!(
                        changed = changed.len(),
                        block = notification_tip_block(&notification),
                        "published balance snapshot"
                    );
                }

                // Acknowledge processed height.
                if let Some(committed_chain) = notification.committed_chain() {
                    ctx.events
                        .send(ExExEvent::FinishedHeight(committed_chain.tip().num_hash()))?;
                }

                blocks_processed += 1;

                // Periodic full snapshot: resync mechanism for missed publishes.
                if blocks_processed % FULL_SNAPSHOT_INTERVAL == 0 && tracker.len() > 0 {
                    let snapshot = build_full_snapshot(&chain_id, &tracker, &balances);
                    let payload = serde_json::to_vec(&snapshot)
                        .expect("ChainBalanceSnapshot serializes");
                    if let Err(e) = nats_client
                        .publish(nats_subject.clone(), payload.into())
                        .await
                    {
                        warn!(error = %e, "failed to publish periodic full snapshot");
                    } else {
                        debug!(
                            tokens = tracker.len(),
                            block = notification_tip_block(&notification),
                            "published periodic full balance snapshot"
                        );
                    }
                }

                if blocks_processed % 100 == 0 {
                    info!(
                        blocks = blocks_processed,
                        updates = updates_published,
                        tokens = tracker.len(),
                        "balance monitor stats"
                    );
                }
            }

            // Whitelist updates (token discovery).
            // Guard: only poll if we have an active subscription.
            msg = async { whitelist_sub.as_mut().unwrap().next().await }, if whitelist_sub.is_some() => {
                match msg {
                    Some(msg) => {
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
                    None => {
                        // Subscription closed (NATS disconnect / server restart).
                        warn!("whitelist subscription closed, attempting resubscribe");
                        match nats_client.subscribe(whitelist_subject.clone()).await {
                            Ok(new_sub) => {
                                whitelist_sub = Some(new_sub);
                                info!("whitelist subscription restored");
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to resubscribe to whitelist, token discovery disabled");
                                // Drop the sub. The `if` guard disables this branch.
                                whitelist_sub = None;
                            }
                        }
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

            // Skip zero-value transfers — no balance change, no publish needed.
            if transfer.value == U256::ZERO {
                continue;
            }

            // Self-transfer (from == to == executor): net zero, skip.
            if is_incoming && is_outgoing {
                continue;
            }

            let entry = balances.entry(transfer.token).or_insert(U256::ZERO);

            if is_revert {
                // Undo: incoming was an add, so subtract; outgoing was a subtract, so add.
                if is_incoming {
                    *entry = entry.saturating_sub(transfer.value);
                } else {
                    *entry = entry.saturating_add(transfer.value);
                }
            } else if is_incoming {
                *entry = entry.saturating_add(transfer.value);
            } else {
                *entry = entry.saturating_sub(transfer.value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use rust_decimal_macros::dec;

    // ── Helper: build a mock receipt with Transfer logs ──────────────────

    /// Minimal receipt that implements TxReceipt<Log = Log>.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MockReceipt {
        logs: Vec<Log>,
    }

    impl alloy_consensus::TxReceipt for MockReceipt {
        type Log = Log;
        fn status_or_post_state(&self) -> alloy_consensus::Eip658Value {
            alloy_consensus::Eip658Value::Eip658(true)
        }
        fn status(&self) -> bool { true }
        fn bloom(&self) -> alloy_primitives::Bloom {
            alloy_primitives::Bloom::default()
        }
        fn cumulative_gas_used(&self) -> u64 { 0 }
        fn logs(&self) -> &[Log] { &self.logs }
    }

    fn transfer_log(token: Address, from: Address, to: Address, value: U256) -> Log {
        use alloy_sol_types::SolEvent;
        let event = crate::transfers::events::Transfer { from, to, value };
        let log_data = event.encode_log_data();
        Log::new(token, log_data.topics().to_vec(), log_data.data.clone()).unwrap()
    }

    fn make_tracker(tokens: &[(Address, u8)]) -> TokenTracker {
        let path = std::path::PathBuf::from(format!(
            "/tmp/bm_test_{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut t = TokenTracker::new(path);
        for &(addr, dec) in tokens {
            t.add(addr, dec);
        }
        t
    }

    // ── u256_to_decimal ──────────────────────────────────────────────────

    #[test]
    fn u256_to_decimal_usdc_1m() {
        // 1,000,000 USDC = 1_000_000 * 10^6 raw = 1_000_000_000_000
        let raw = U256::from(1_000_000_000_000u64);
        let d = u256_to_decimal(raw, 6);
        assert_eq!(d, dec!(1000000));
    }

    #[test]
    fn u256_to_decimal_weth_1() {
        // 1 WETH = 10^18 raw
        let raw = U256::from(1_000_000_000_000_000_000u64);
        let d = u256_to_decimal(raw, 18);
        assert_eq!(d, dec!(1));
    }

    #[test]
    fn u256_to_decimal_zero() {
        assert_eq!(u256_to_decimal(U256::ZERO, 18), Decimal::ZERO);
    }

    #[test]
    fn u256_to_decimal_fractional() {
        // 500_000 raw with 6 decimals = 0.5
        let raw = U256::from(500_000u64);
        let d = u256_to_decimal(raw, 6);
        assert_eq!(d, dec!(0.5));
    }

    #[test]
    fn u256_to_decimal_zero_decimals() {
        // Token with 0 decimals: raw = human
        let raw = U256::from(42u64);
        let d = u256_to_decimal(raw, 0);
        assert_eq!(d, dec!(42));
    }

    // ── Schema compatibility ─────────────────────────────────────────────

    /// Verify the JSON shape matches what the hedger deserializes as
    /// `ChainBalanceSnapshot` from common/messages.rs.
    #[test]
    fn snapshot_json_matches_hedger_schema() {
        let snapshot = ChainBalanceSnapshot {
            chain: "1".to_string(),
            balances: vec![ChainTokenBalance {
                token: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".to_string(),
                available: dec!(1000.5),
                total: None,
            }],
            ts: 1234567890,
        };

        let json = serde_json::to_value(&snapshot).unwrap();

        // Required fields
        assert_eq!(json["chain"], "1");
        assert_eq!(json["ts"], 1234567890u64);
        assert!(json["balances"].is_array());

        let entry = &json["balances"][0];
        assert_eq!(entry["token"], "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48");
        // `available` must be a string (rust_decimal::serde::str format)
        assert_eq!(entry["available"], "1000.5");
        // `total` should be absent (skip_serializing_if = None)
        assert!(entry.get("total").is_none());
    }

    /// Verify the hedger can round-trip our JSON through its expected types.
    /// We replicate the hedger's deserialization structs here to prove compat.
    #[test]
    fn snapshot_json_deserializes_as_hedger_types() {
        // Hedger-side types (mirrored from common/messages.rs)
        #[derive(serde::Deserialize)]
        struct HedgerSnapshot {
            chain: String,
            balances: Vec<HedgerTokenBalance>,
            ts: u64,
        }
        #[derive(serde::Deserialize)]
        struct HedgerTokenBalance {
            token: String,
            #[serde(with = "rust_decimal::serde::str")]
            available: Decimal,
            #[serde(default, with = "rust_decimal::serde::str_option")]
            total: Option<Decimal>,
        }

        let snapshot = ChainBalanceSnapshot {
            chain: "1".to_string(),
            balances: vec![ChainTokenBalance {
                token: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".to_string(),
                available: dec!(2.5),
                total: None,
            }],
            ts: 999,
        };

        let json = serde_json::to_vec(&snapshot).unwrap();
        let parsed: HedgerSnapshot = serde_json::from_slice(&json).unwrap();

        assert_eq!(parsed.chain, "1");
        assert_eq!(parsed.ts, 999);
        assert_eq!(parsed.balances.len(), 1);
        assert_eq!(parsed.balances[0].token, "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
        assert_eq!(parsed.balances[0].available, dec!(2.5));
        assert!(parsed.balances[0].total.is_none());
    }

    // ── process_receipts: delta logic ────────────────────────────────────

    const EXECUTOR: Address = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
    const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    const OTHER: Address = address!("dEAD000000000000000000000000000000000000");

    #[test]
    fn incoming_transfer_adds_balance() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::new();
        let mut changed = Vec::new();

        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, OTHER, EXECUTOR, U256::from(1_000_000u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        assert_eq!(balances[&USDC], U256::from(1_000_000u64));
        assert_eq!(changed, vec![USDC]);
    }

    #[test]
    fn outgoing_transfer_subtracts_balance() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::from([(USDC, U256::from(5_000_000u64))]);
        let mut changed = Vec::new();

        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, EXECUTOR, OTHER, U256::from(2_000_000u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        assert_eq!(balances[&USDC], U256::from(3_000_000u64));
    }

    #[test]
    fn revert_undoes_incoming() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::from([(USDC, U256::from(10_000_000u64))]);
        let mut changed = Vec::new();

        // Revert an incoming transfer of 3M
        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, OTHER, EXECUTOR, U256::from(3_000_000u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, true);

        assert_eq!(balances[&USDC], U256::from(7_000_000u64));
    }

    #[test]
    fn revert_undoes_outgoing() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::from([(USDC, U256::from(10_000_000u64))]);
        let mut changed = Vec::new();

        // Revert an outgoing transfer of 2M (should add back)
        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, EXECUTOR, OTHER, U256::from(2_000_000u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, true);

        assert_eq!(balances[&USDC], U256::from(12_000_000u64));
    }

    #[test]
    fn self_transfer_is_noop() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::from([(USDC, U256::from(5_000_000u64))]);
        let mut changed = Vec::new();

        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, EXECUTOR, EXECUTOR, U256::from(1_000_000u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        // Balance unchanged, no token in changed list.
        assert_eq!(balances[&USDC], U256::from(5_000_000u64));
        assert!(changed.is_empty());
    }

    #[test]
    fn zero_value_transfer_is_skipped() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::from([(USDC, U256::from(5_000_000u64))]);
        let mut changed = Vec::new();

        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, OTHER, EXECUTOR, U256::ZERO)],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        assert_eq!(balances[&USDC], U256::from(5_000_000u64));
        assert!(changed.is_empty());
    }

    #[test]
    fn untracked_token_is_ignored() {
        let tracker = make_tracker(&[(USDC, 6)]); // only USDC tracked
        let mut balances = HashMap::new();
        let mut changed = Vec::new();

        let receipt = MockReceipt {
            logs: vec![transfer_log(WETH, OTHER, EXECUTOR, U256::from(1_000u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        assert!(!balances.contains_key(&WETH));
        assert!(changed.is_empty());
    }

    #[test]
    fn uninvolved_transfer_is_ignored() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::new();
        let mut changed = Vec::new();

        // Transfer between two other addresses
        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, OTHER, address!("BEEF000000000000000000000000000000000000"), U256::from(999u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        assert!(changed.is_empty());
    }

    #[test]
    fn saturating_sub_floors_at_zero() {
        let tracker = make_tracker(&[(USDC, 6)]);
        let mut balances = HashMap::from([(USDC, U256::from(100u64))]);
        let mut changed = Vec::new();

        // Outgoing more than balance
        let receipt = MockReceipt {
            logs: vec![transfer_log(USDC, EXECUTOR, OTHER, U256::from(500u64))],
        };
        process_receipts(&[receipt], EXECUTOR, &tracker, &mut balances, &mut changed, false);

        assert_eq!(balances[&USDC], U256::ZERO);
    }

    // ── build_full_snapshot ──────────────────────────────────────────────

    #[test]
    fn full_snapshot_includes_all_tracked_tokens() {
        let tracker = make_tracker(&[(USDC, 6), (WETH, 18)]);
        let balances = HashMap::from([
            (USDC, U256::from(2_000_000u64)),     // 2.0 USDC
            (WETH, U256::from(500_000_000_000_000_000u64)), // 0.5 WETH
        ]);

        let snapshot = build_full_snapshot("1", &tracker, &balances);

        assert_eq!(snapshot.chain, "1");
        assert_eq!(snapshot.balances.len(), 2);

        // Find each token (order is non-deterministic from HashMap iter)
        let usdc_entry = snapshot.balances.iter().find(|e| e.token.contains("a0b8")).unwrap();
        let weth_entry = snapshot.balances.iter().find(|e| e.token.contains("c02a")).unwrap();

        assert_eq!(usdc_entry.available, dec!(2));
        assert_eq!(weth_entry.available, dec!(0.5));
    }

    // ── process_whitelist_message ────────────────────────────────────────

    #[test]
    fn whitelist_message_extracts_tokens() {
        let json = serde_json::json!({
            "pools": [{
                "token0": { "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "decimals": 6 },
                "token1": { "address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "decimals": 18 }
            }]
        });
        let payload = serde_json::to_vec(&json).unwrap();

        let mut tracker = make_tracker(&[]);
        let new = process_whitelist_message(&payload, &mut tracker);

        assert_eq!(new.len(), 2);
        assert_eq!(tracker.len(), 2);
        assert!(tracker.contains(&USDC));
        assert!(tracker.contains(&WETH));
    }

    #[test]
    fn whitelist_message_malformed_returns_empty() {
        let mut tracker = make_tracker(&[]);
        let new = process_whitelist_message(b"not json", &mut tracker);
        assert!(new.is_empty());
        assert_eq!(tracker.len(), 0);
    }

    #[test]
    fn whitelist_message_duplicate_tokens_not_readded() {
        let mut tracker = make_tracker(&[(USDC, 6)]);
        let json = serde_json::json!({
            "pools": [{
                "token0": { "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "decimals": 6 },
                "token1": { "address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "decimals": 18 }
            }]
        });
        let payload = serde_json::to_vec(&json).unwrap();
        let new = process_whitelist_message(&payload, &mut tracker);

        // Only WETH is new
        assert_eq!(new.len(), 1);
        assert_eq!(new[0], WETH);
    }
}
