// Pool Tracker V2 - Differential Whitelist Updates with Block Synchronization
//
// Key improvements:
// 1. Differential updates (add/remove) instead of full replacement
// 2. Block-synchronized updates - changes applied between blocks to prevent event loss
// 3. Pending update queue - whitelist changes queued and applied atomically

use crate::events::{BALANCER_V2_VAULT, EKUBO_CORE};
use crate::fluid_decoder::FluidPoolConfig;
use crate::types::{PoolIdentifier, PoolMetadata, Protocol};
use alloy_primitives::{address, Address};
use std::collections::{HashMap, HashSet, VecDeque};
use tracing::{info, warn};

// ============================================================================
// SINGLETON CONTRACT CONSTANTS
// ============================================================================

/// Uniswap V4 PoolManager singleton contract address (Ethereum Mainnet)
/// All V4 Swap and ModifyLiquidity events are emitted from this address
/// Deployed: https://etherscan.io/address/0x000000000004444c5dc75cb358380d2e3de08a90
pub const UNISWAP_V4_POOL_MANAGER: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

/// Fluid Liquidity Layer singleton address (Ethereum Mainnet).
/// All LogOperate events from Fluid DEX pools are emitted from this address.
/// Deployed: https://etherscan.io/address/0x52Aa899454998Be5b000Ad077a46Bbe360F4e497
pub const FLUID_LIQUIDITY_LAYER: Address = address!("52Aa899454998Be5b000Ad077a46Bbe360F4e497");

/// Differential whitelist update operations
#[derive(Debug, Clone)]
pub enum WhitelistUpdate {
    /// Add pools to whitelist
    Add(Vec<PoolMetadata>),
    /// Remove pools from whitelist
    Remove(Vec<PoolIdentifier>),
    /// Full replacement (for initial load only)
    Replace(Vec<PoolMetadata>),
}

/// Tracks which pools we should monitor for events
pub struct PoolTracker {
    /// Map of pool address -> metadata (for V2/V3)
    pools_by_address: HashMap<Address, PoolMetadata>,

    /// Map of pool_id (bytes32) -> metadata (for V4)
    pools_by_id: HashMap<[u8; 32], PoolMetadata>,

    /// Set of tracked addresses for fast lookup
    tracked_addresses: HashSet<Address>,

    /// Set of tracked pool IDs for fast lookup
    tracked_pool_ids: HashSet<[u8; 32]>,

    /// Fluid pool configs — cached immutable constants from `constantsView()`.
    /// Keyed by pool address. Populated at registration time via RPC.
    fluid_configs: HashMap<Address, FluidPoolConfig>,

    /// Balancer V2 pool CONTRACT address (`pool_id[..20]`) -> 32-byte poolId.
    /// SwapFeePercentageChanged is emitted by the pool contract, so we track the
    /// pool address and map it back to the poolId for the arena fee update.
    balancer_pools_by_addr: HashMap<Address, [u8; 32]>,

    /// Pending whitelist updates (applied between blocks)
    pending_updates: VecDeque<WhitelistUpdate>,

    /// Pools added since the last `take_newly_added` drain. The ExEx drains this
    /// at each committed block boundary and hydrates them into the shadow arena
    /// from current state, so live `.add` pools are written without a restart.
    newly_added: Vec<PoolMetadata>,

    /// Whether we're currently processing a block
    in_block: bool,

    /// Statistics
    v2_count: usize,
    v3_count: usize,
    v4_count: usize,
    ekubo_count: usize,
    curve_stable_count: usize,
    curve_twocrypto_count: usize,
    curve_tricrypto_count: usize,
    balancer_v2_count: usize,
    fluid_count: usize,
}

impl PoolTracker {
    pub fn new() -> Self {
        Self {
            pools_by_address: HashMap::new(),
            pools_by_id: HashMap::new(),
            tracked_addresses: HashSet::new(),
            tracked_pool_ids: HashSet::new(),
            fluid_configs: HashMap::new(),
            balancer_pools_by_addr: HashMap::new(),
            pending_updates: VecDeque::new(),
            newly_added: Vec::new(),
            in_block: false,
            v2_count: 0,
            v3_count: 0,
            v4_count: 0,
            ekubo_count: 0,
            curve_stable_count: 0,
            curve_twocrypto_count: 0,
            curve_tricrypto_count: 0,
            balancer_v2_count: 0,
            fluid_count: 0,
        }
    }

    /// Mark the start of block processing
    /// Whitelist updates will be queued until block ends
    pub fn begin_block(&mut self) {
        self.in_block = true;
    }

    /// Mark the end of block processing
    /// Apply any pending whitelist updates atomically
    pub fn end_block(&mut self) {
        self.in_block = false;
        self.apply_pending_updates();
    }

    /// Queue a whitelist update (will be applied at end of current block)
    pub fn queue_update(&mut self, update: WhitelistUpdate) {
        match &update {
            WhitelistUpdate::Add(pools) => {
                info!("Queuing add: {} pools", pools.len());
            }
            WhitelistUpdate::Remove(pools) => {
                info!("Queuing remove: {} pools", pools.len());
            }
            WhitelistUpdate::Replace(pools) => {
                info!("Queuing replace: {} pools", pools.len());
            }
        }

        self.pending_updates.push_back(update);

        // If not in block, apply immediately
        if !self.in_block {
            self.apply_pending_updates();
        }
    }

    /// Apply all pending whitelist updates
    fn apply_pending_updates(&mut self) {
        if self.pending_updates.is_empty() {
            return;
        }

        info!(
            "Applying {} pending whitelist updates",
            self.pending_updates.len()
        );

        while let Some(update) = self.pending_updates.pop_front() {
            match update {
                WhitelistUpdate::Add(pools) => self.add_pools(pools, true),
                WhitelistUpdate::Remove(pool_ids) => self.remove_pools(pool_ids),
                WhitelistUpdate::Replace(pools) => self.replace_all(pools),
            }
        }

        info!(
            "Whitelist now tracking: {} V2, {} V3, {} V4, {} Ekubo, {} CurveStable, {} CurveTwoCrypto, {} CurveTricrypto, {} BalancerV2, {} Fluid pools (total: {})",
            self.v2_count,
            self.v3_count,
            self.v4_count,
            self.ekubo_count,
            self.curve_stable_count,
            self.curve_twocrypto_count,
            self.curve_tricrypto_count,
            self.balancer_v2_count,
            self.fluid_count,
            self.pools_by_address.len() + self.pools_by_id.len()
        );
    }

    /// Add pools to the whitelist.
    ///
    /// `surface_newly_added` is true for live `.add` deltas so the ExEx can hydrate
    /// those pools into the shadow arena. It is false for `.full`/startup replace:
    /// startup hydration is already done from the frozen anchor, and treating the
    /// full snapshot as live additions would retry-hydrate the whole universe on the
    /// first committed block.
    fn add_pools(&mut self, pools: Vec<PoolMetadata>, surface_newly_added: bool) {
        let mut added = 0;

        for pool in pools {
            // Check if already tracked
            let already_tracked = match &pool.pool_id {
                PoolIdentifier::Address(addr) => self.tracked_addresses.contains(addr),
                PoolIdentifier::PoolId(id) => self.tracked_pool_ids.contains(id),
            };

            if already_tracked {
                continue; // Skip duplicates
            }

            // Add to tracking
            match &pool.pool_id {
                PoolIdentifier::Address(addr) => {
                    self.tracked_addresses.insert(*addr);
                    self.pools_by_address.insert(*addr, pool.clone());
                }
                PoolIdentifier::PoolId(id) => {
                    // For V4/Ekubo pools, track the poolId AND the singleton address
                    self.tracked_pool_ids.insert(*id);
                    self.pools_by_id.insert(*id, pool.clone());

                    // Track singleton contract addresses so we receive their events
                    match pool.protocol {
                        Protocol::UniswapV4 => {
                            if !self.tracked_addresses.contains(&UNISWAP_V4_POOL_MANAGER) {
                                self.tracked_addresses.insert(UNISWAP_V4_POOL_MANAGER);
                                info!(
                                    "🔧 Added PoolManager address for V4 events: {:?}",
                                    UNISWAP_V4_POOL_MANAGER
                                );
                            }
                        }
                        Protocol::Ekubo => {
                            if !self.tracked_addresses.contains(&EKUBO_CORE) {
                                self.tracked_addresses.insert(EKUBO_CORE);
                                info!(
                                    "🔧 Added Ekubo Core address for Ekubo events: {:?}",
                                    EKUBO_CORE
                                );
                            }
                        }
                        Protocol::BalancerV2Weighted => {
                            if !self.tracked_addresses.contains(&BALANCER_V2_VAULT) {
                                self.tracked_addresses.insert(BALANCER_V2_VAULT);
                                info!(
                                    "🔧 Added Balancer V2 Vault for Swap/PoolBalanceChanged events: {:?}",
                                    BALANCER_V2_VAULT
                                );
                            }
                            // Also track the POOL contract address: SwapFeePercentage-
                            // Changed is emitted by the pool itself, not the Vault. Map
                            // it back to the 32-byte poolId for the fee-update apply.
                            let pool_addr = Address::from_slice(&id[..20]);
                            self.tracked_addresses.insert(pool_addr);
                            self.balancer_pools_by_addr.insert(pool_addr, *id);
                        }
                        _ => {}
                    }
                }
            }

            // Update counts
            match pool.protocol {
                Protocol::UniswapV2 => self.v2_count += 1,
                Protocol::UniswapV3 => self.v3_count += 1,
                Protocol::UniswapV4 => self.v4_count += 1,
                Protocol::Ekubo => self.ekubo_count += 1,
                Protocol::CurveStable => self.curve_stable_count += 1,
                Protocol::CurveTwoCrypto => self.curve_twocrypto_count += 1,
                Protocol::CurveTricrypto => self.curve_tricrypto_count += 1,
                Protocol::BalancerV2Weighted => self.balancer_v2_count += 1,
                Protocol::Fluid => self.fluid_count += 1,
            }

            // Queue live `.add` pools for shadow-arena hydration (drained by the
            // ExEx at the next committed block boundary). Startup/full replace is
            // hydrated separately from the frozen anchor and must not surface here.
            if surface_newly_added {
                self.newly_added.push(pool);
            }
            added += 1;
        }

        // Ensure Liquidity Layer address is tracked when any Fluid pools exist
        if self.fluid_count > 0 && !self.tracked_addresses.contains(&FLUID_LIQUIDITY_LAYER) {
            self.tracked_addresses.insert(FLUID_LIQUIDITY_LAYER);
            info!(
                "🔧 Added Fluid Liquidity Layer to tracked addresses for LogOperate events: {:?}",
                FLUID_LIQUIDITY_LAYER
            );
        }

        info!("Added {} new pools to whitelist", added);
    }

    /// Remove pools from the whitelist
    fn remove_pools(&mut self, pool_ids: Vec<PoolIdentifier>) {
        let mut removed = 0;

        for pool_id in pool_ids {
            // Drop any not-yet-hydrated `.add` for this pool: a failed add followed
            // by a remove must not later hydrate a stale arena slot.
            self.newly_added.retain(|p| p.pool_id != pool_id);
            match pool_id {
                PoolIdentifier::Address(addr) => {
                    if let Some(pool) = self.pools_by_address.remove(&addr) {
                        self.tracked_addresses.remove(&addr);

                        // Clean up Fluid config if applicable
                        if pool.protocol == Protocol::Fluid {
                            self.fluid_configs.remove(&addr);
                        }

                        // Update counts
                        match pool.protocol {
                            Protocol::UniswapV2 => self.v2_count -= 1,
                            Protocol::UniswapV3 => self.v3_count -= 1,
                            Protocol::UniswapV4 => self.v4_count -= 1,
                            Protocol::Ekubo => self.ekubo_count -= 1,
                            Protocol::CurveStable => self.curve_stable_count -= 1,
                            Protocol::CurveTwoCrypto => self.curve_twocrypto_count -= 1,
                            Protocol::CurveTricrypto => self.curve_tricrypto_count -= 1,
                            Protocol::BalancerV2Weighted => self.balancer_v2_count -= 1,
                            Protocol::Fluid => self.fluid_count -= 1,
                        }

                        removed += 1;
                    }
                }
                PoolIdentifier::PoolId(id) => {
                    if let Some(pool) = self.pools_by_id.remove(&id) {
                        self.tracked_pool_ids.remove(&id);

                        // Balancer pools also track their pool contract address (for
                        // fee events) — untrack it and drop the reverse mapping.
                        if pool.protocol == Protocol::BalancerV2Weighted {
                            let pool_addr = Address::from_slice(&id[..20]);
                            self.tracked_addresses.remove(&pool_addr);
                            self.balancer_pools_by_addr.remove(&pool_addr);
                        }

                        // Update counts
                        match pool.protocol {
                            Protocol::UniswapV2 => self.v2_count -= 1,
                            Protocol::UniswapV3 => self.v3_count -= 1,
                            Protocol::UniswapV4 => self.v4_count -= 1,
                            Protocol::Ekubo => self.ekubo_count -= 1,
                            Protocol::CurveStable => self.curve_stable_count -= 1,
                            Protocol::CurveTwoCrypto => self.curve_twocrypto_count -= 1,
                            Protocol::CurveTricrypto => self.curve_tricrypto_count -= 1,
                            Protocol::BalancerV2Weighted => self.balancer_v2_count -= 1,
                            Protocol::Fluid => self.fluid_count -= 1,
                        }

                        removed += 1;
                    }
                }
            }
        }

        info!("Removed {} pools from whitelist", removed);
    }

    /// Full replacement of whitelist (used for initial load)
    fn replace_all(&mut self, pools: Vec<PoolMetadata>) {
        warn!("Full whitelist replacement with {} pools", pools.len());

        // Clear existing
        self.pools_by_address.clear();
        self.pools_by_id.clear();
        self.tracked_addresses.clear();
        self.tracked_pool_ids.clear();
        self.fluid_configs.clear();
        self.balancer_pools_by_addr.clear();
        self.newly_added.clear();
        self.v2_count = 0;
        self.v3_count = 0;
        self.v4_count = 0;
        self.ekubo_count = 0;
        self.curve_stable_count = 0;
        self.curve_twocrypto_count = 0;
        self.curve_tricrypto_count = 0;
        self.balancer_v2_count = 0;
        self.fluid_count = 0;

        // Add new pools. A full snapshot is not a live `.add` delta: startup
        // shadow hydration is driven explicitly from the same snapshot before the
        // tracker is replaced, so do not put every pool into `newly_added`.
        self.add_pools(pools, false);
    }

    /// Legacy method for backward compatibility - converts to Replace update
    #[allow(dead_code)]
    pub fn update_whitelist(&mut self, pools: Vec<PoolMetadata>) {
        self.queue_update(WhitelistUpdate::Replace(pools));
    }

    /// Check if an address is a tracked pool
    pub fn is_tracked_address(&self, address: &Address) -> bool {
        self.tracked_addresses.contains(address)
    }

    /// Check if a pool ID is tracked
    pub fn is_tracked_pool_id(&self, pool_id: &[u8; 32]) -> bool {
        self.tracked_pool_ids.contains(pool_id)
    }

    /// Full metadata for an address-keyed pool (V2/V3/Curve/Fluid), for re-scrape.
    pub fn pool_metadata(&self, address: &Address) -> Option<&PoolMetadata> {
        self.pools_by_address.get(address)
    }

    /// Full metadata for a pool-id-keyed pool (V4/Ekubo/Balancer/FluidV2).
    pub fn pool_metadata_by_id(&self, pool_id: &[u8; 32]) -> Option<&PoolMetadata> {
        self.pools_by_id.get(pool_id)
    }

    /// Get the protocol of a pool tracked by address.
    pub fn get_protocol(&self, address: &Address) -> Option<Protocol> {
        self.pools_by_address.get(address).map(|m| m.protocol)
    }

    /// Get pool metadata by address
    #[allow(dead_code)]
    pub fn get_by_address(&self, address: &Address) -> Option<&PoolMetadata> {
        self.pools_by_address.get(address)
    }

    /// Get pool metadata by pool ID
    #[allow(dead_code)]
    pub fn get_by_pool_id(&self, pool_id: &[u8; 32]) -> Option<&PoolMetadata> {
        self.pools_by_id.get(pool_id)
    }

    /// Get all tracked addresses
    #[allow(dead_code)]
    pub fn tracked_addresses(&self) -> &HashSet<Address> {
        &self.tracked_addresses
    }

    /// Get all tracked pool IDs
    #[allow(dead_code)]
    pub fn tracked_pool_ids(&self) -> &HashSet<[u8; 32]> {
        &self.tracked_pool_ids
    }

    /// Check if a pool address is a tracked Fluid pool.
    pub fn is_tracked_fluid_pool(&self, address: &Address) -> bool {
        self.pools_by_address
            .get(address)
            .map(|p| p.protocol == Protocol::Fluid)
            .unwrap_or(false)
    }

    /// Check if a Fluid pool has its config resolved (slot addresses cached).
    #[allow(dead_code)]
    pub fn has_fluid_config(&self, address: &Address) -> bool {
        self.fluid_configs.contains_key(address)
    }

    /// Register a Fluid pool's immutable config (slot addresses + precision).
    /// Called once per pool at registration time after RPC resolution.
    pub fn register_fluid_config(&mut self, config: FluidPoolConfig) {
        info!(
            pool = %config.pool_address,
            liquidity = %config.liquidity_address,
            "Registered Fluid pool config"
        );
        self.fluid_configs.insert(config.pool_address, config);
    }

    /// Get a Fluid pool's cached config for storage reads + decoding.
    pub fn fluid_config(&self, pool: &Address) -> Option<&FluidPoolConfig> {
        self.fluid_configs.get(pool)
    }

    /// The full resolved Fluid config map — used by live-add shadow hydration to
    /// build Fluid hydrations from the same source as startup.
    pub fn fluid_configs_map(&self) -> &HashMap<Address, FluidPoolConfig> {
        &self.fluid_configs
    }

    /// Map a Balancer pool CONTRACT address (`pool_id[..20]`) back to its 32-byte
    /// poolId, for the fee-update apply. `None` if not a tracked Balancer pool.
    pub fn balancer_pool_id_for_addr(&self, addr: &Address) -> Option<[u8; 32]> {
        self.balancer_pools_by_addr.get(addr).copied()
    }

    /// Whether a pool identifier is currently tracked. Used by live-add hydration
    /// to skip drained additions that were removed before they could hydrate.
    pub fn is_tracked(&self, pool_id: &PoolIdentifier) -> bool {
        match pool_id {
            PoolIdentifier::Address(addr) => self.pools_by_address.contains_key(addr),
            PoolIdentifier::PoolId(id) => self.pools_by_id.contains_key(id),
        }
    }

    /// Re-queue pools that could not be hydrated this round (e.g. a Fluid pool
    /// whose config has not finished resolving) so the next committed block
    /// retries them, rather than dropping them from the shadow topology.
    pub fn requeue_newly_added(&mut self, pools: Vec<PoolMetadata>) {
        self.newly_added.extend(pools);
    }

    /// Get statistics
    pub fn stats(&self) -> PoolTrackerStats {
        PoolTrackerStats {
            total_pools: self.pools_by_address.len() + self.pools_by_id.len(),
            v2_pools: self.v2_count,
            v3_pools: self.v3_count,
            v4_pools: self.v4_count,
            ekubo_pools: self.ekubo_count,
            curve_stable_pools: self.curve_stable_count,
            curve_twocrypto_pools: self.curve_twocrypto_count,
            curve_tricrypto_pools: self.curve_tricrypto_count,
            balancer_v2_pools: self.balancer_v2_count,
            fluid_pools: self.fluid_count,
        }
    }

    /// Check if there are pending updates
    #[allow(dead_code)]
    pub fn has_pending_updates(&self) -> bool {
        !self.pending_updates.is_empty()
    }

    /// Drain the pools added since the last call. The ExEx hydrates these into
    /// the shadow arena from current state at the committed block boundary so a
    /// live `.add` pool is written without waiting for a restart.
    pub fn take_newly_added(&mut self) -> Vec<PoolMetadata> {
        std::mem::take(&mut self.newly_added)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PoolTrackerStats {
    pub total_pools: usize,
    pub v2_pools: usize,
    pub v3_pools: usize,
    pub v4_pools: usize,
    pub ekubo_pools: usize,
    pub curve_stable_pools: usize,
    pub curve_twocrypto_pools: usize,
    pub curve_tricrypto_pools: usize,
    pub balancer_v2_pools: usize,
    pub fluid_pools: usize,
}

impl Default for PoolTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_pool(addr: Address, protocol: Protocol) -> PoolMetadata {
        PoolMetadata {
            pool_id: PoolIdentifier::Address(addr),
            token0: Address::ZERO,
            token1: Address::ZERO,
            protocol,
            factory: Address::ZERO,
            tick_spacing: None,
            fee: None,
            token0_decimals: None,
            token1_decimals: None,
            extra_tokens: vec![],
            twocrypto_version: None,
            ekubo_fee: None,
            ekubo_type_config: None,
            balancer_weights: None,
            balancer_swap_fee: None,
        }
    }

    #[test]
    fn test_add_pools() {
        let mut tracker = PoolTracker::new();

        let pool1 = create_test_pool(Address::from([1u8; 20]), Protocol::UniswapV2);
        let pool2 = create_test_pool(Address::from([2u8; 20]), Protocol::UniswapV3);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool1, pool2]));

        assert_eq!(tracker.stats().total_pools, 2);
        assert_eq!(tracker.stats().v2_pools, 1);
        assert_eq!(tracker.stats().v3_pools, 1);
    }

    /// ITE-16 round-18: added pools surface via `take_newly_added` (for live-add
    /// shadow hydration); full replace/startup does not surface the whole snapshot,
    /// the drain empties it, dedup of duplicate adds holds, and `requeue_newly_added`
    /// puts unhydratable pools back for a later retry.
    #[test]
    fn newly_added_drains_and_requeues() {
        let mut tracker = PoolTracker::new();
        let a = Address::from([1u8; 20]);
        let b = Address::from([2u8; 20]);
        tracker.queue_update(WhitelistUpdate::Add(vec![
            create_test_pool(a, Protocol::UniswapV2),
            create_test_pool(b, Protocol::UniswapV3),
        ]));

        let drained = tracker.take_newly_added();
        assert_eq!(drained.len(), 2, "both added pools surfaced for hydration");
        assert!(
            tracker.take_newly_added().is_empty(),
            "drain empties the set"
        );

        // A duplicate add is skipped (already tracked) — nothing new to hydrate.
        tracker.queue_update(WhitelistUpdate::Add(vec![create_test_pool(
            a,
            Protocol::UniswapV2,
        )]));
        assert!(
            tracker.take_newly_added().is_empty(),
            "duplicate add does not re-queue hydration"
        );

        // Re-queued (unhydratable) pools come back on the next drain.
        tracker.requeue_newly_added(drained);
        assert_eq!(
            tracker.take_newly_added().len(),
            2,
            "requeued pools retried"
        );
    }

    /// Round-19 Warning: an `.add` removed before it hydrates must not linger in
    /// `newly_added` (else it would later hydrate a stale/untracked slot).
    #[test]
    fn remove_purges_pending_newly_added() {
        let mut tracker = PoolTracker::new();
        let a = Address::from([9u8; 20]);
        tracker.queue_update(WhitelistUpdate::Add(vec![create_test_pool(
            a,
            Protocol::UniswapV2,
        )]));
        tracker.queue_update(WhitelistUpdate::Remove(vec![PoolIdentifier::Address(a)]));
        assert!(
            tracker.take_newly_added().is_empty(),
            "removed-before-hydrate add is purged from newly_added"
        );
    }

    /// Round-19 Critical: a Balancer pool tracks its CONTRACT address (`pool_id[..20]`)
    /// so pool-emitted SwapFeePercentageChanged logs pass the filter, and maps it
    /// back to the poolId. Removal untracks the address and clears the mapping.
    #[test]
    fn balancer_pool_contract_addr_tracked_and_mapped() {
        let mut tracker = PoolTracker::new();
        let mut pid = [0u8; 32];
        pid[..20].copy_from_slice(&[0x5c; 20]);
        pid[21] = 0x02; // TwoToken specialization bytes (not used here)
        let pool = PoolMetadata {
            pool_id: PoolIdentifier::PoolId(pid),
            ..create_test_pool(Address::ZERO, Protocol::BalancerV2Weighted)
        };
        tracker.queue_update(WhitelistUpdate::Add(vec![pool]));

        let pool_addr = Address::from_slice(&pid[..20]);
        assert!(
            tracker.is_tracked_address(&pool_addr),
            "pool contract address tracked for fee events"
        );
        assert_eq!(tracker.balancer_pool_id_for_addr(&pool_addr), Some(pid));

        tracker.queue_update(WhitelistUpdate::Remove(vec![PoolIdentifier::PoolId(pid)]));
        assert!(
            !tracker.is_tracked_address(&pool_addr),
            "untracked on remove"
        );
        assert_eq!(tracker.balancer_pool_id_for_addr(&pool_addr), None);
    }

    #[test]
    fn replace_does_not_surface_startup_snapshot_as_newly_added() {
        let mut tracker = PoolTracker::new();
        tracker.queue_update(WhitelistUpdate::Replace(vec![create_test_pool(
            Address::from([3u8; 20]),
            Protocol::UniswapV2,
        )]));
        assert!(
            tracker.take_newly_added().is_empty(),
            "full replace/startup snapshot is hydrated separately, not as live-add"
        );
    }

    #[test]
    fn test_remove_pools() {
        let mut tracker = PoolTracker::new();

        let addr1 = Address::from([1u8; 20]);
        let addr2 = Address::from([2u8; 20]);

        let pool1 = create_test_pool(addr1, Protocol::UniswapV2);
        let pool2 = create_test_pool(addr2, Protocol::UniswapV3);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool1, pool2]));
        assert_eq!(tracker.stats().total_pools, 2);

        // Remove pool1
        tracker.queue_update(WhitelistUpdate::Remove(vec![PoolIdentifier::Address(
            addr1,
        )]));

        assert_eq!(tracker.stats().total_pools, 1);
        assert_eq!(tracker.stats().v2_pools, 0);
        assert_eq!(tracker.stats().v3_pools, 1);
        assert!(!tracker.is_tracked_address(&addr1));
        assert!(tracker.is_tracked_address(&addr2));
    }

    #[test]
    fn test_block_synchronized_updates() {
        let mut tracker = PoolTracker::new();

        let addr1 = Address::from([1u8; 20]);
        let pool1 = create_test_pool(addr1, Protocol::UniswapV2);

        // Start block - updates should be queued
        tracker.begin_block();

        tracker.queue_update(WhitelistUpdate::Add(vec![pool1]));

        // Should still be 0 because we're in a block
        assert_eq!(tracker.stats().total_pools, 0);
        assert!(tracker.has_pending_updates());

        // End block - updates should be applied
        tracker.end_block();

        assert_eq!(tracker.stats().total_pools, 1);
        assert!(!tracker.has_pending_updates());
        assert!(tracker.is_tracked_address(&addr1));
    }

    #[test]
    fn test_no_duplicate_adds() {
        let mut tracker = PoolTracker::new();

        let addr = Address::from([1u8; 20]);
        let pool = create_test_pool(addr, Protocol::UniswapV2);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool.clone()]));
        tracker.queue_update(WhitelistUpdate::Add(vec![pool]));

        // Should only count once
        assert_eq!(tracker.stats().total_pools, 1);
        assert_eq!(tracker.stats().v2_pools, 1);
    }

    #[test]
    fn test_fluid_pool_tracking() {
        let mut tracker = PoolTracker::new();

        let fluid_addr = Address::from([0xAA; 20]);
        let v2_addr = Address::from([0xBB; 20]);

        let fluid_pool = create_test_pool(fluid_addr, Protocol::Fluid);
        let v2_pool = create_test_pool(v2_addr, Protocol::UniswapV2);

        tracker.queue_update(WhitelistUpdate::Add(vec![fluid_pool, v2_pool]));

        assert_eq!(tracker.stats().fluid_pools, 1);
        assert_eq!(tracker.stats().v2_pools, 1);
        assert_eq!(tracker.stats().total_pools, 2);

        // Fluid pool should be tracked by address
        assert!(tracker.is_tracked_address(&fluid_addr));
        assert!(tracker.is_tracked_fluid_pool(&fluid_addr));

        // V2 pool should be tracked but NOT as Fluid
        assert!(tracker.is_tracked_address(&v2_addr));
        assert!(!tracker.is_tracked_fluid_pool(&v2_addr));

        // Liquidity Layer singleton should be auto-added for LogOperate events
        assert!(
            tracker.is_tracked_address(&FLUID_LIQUIDITY_LAYER),
            "Liquidity Layer address should be tracked when Fluid pools exist"
        );
    }

    #[test]
    fn test_fluid_pool_remove() {
        let mut tracker = PoolTracker::new();

        let fluid_addr = Address::from([0xCC; 20]);
        let fluid_pool = create_test_pool(fluid_addr, Protocol::Fluid);

        tracker.queue_update(WhitelistUpdate::Add(vec![fluid_pool]));
        assert_eq!(tracker.stats().fluid_pools, 1);

        tracker.queue_update(WhitelistUpdate::Remove(vec![PoolIdentifier::Address(
            fluid_addr,
        )]));

        assert_eq!(tracker.stats().fluid_pools, 0);
        assert!(!tracker.is_tracked_fluid_pool(&fluid_addr));
    }
}
