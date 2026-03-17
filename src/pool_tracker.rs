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

    /// Pending whitelist updates (applied between blocks)
    pending_updates: VecDeque<WhitelistUpdate>,

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
            pending_updates: VecDeque::new(),
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
                WhitelistUpdate::Add(pools) => self.add_pools(pools),
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

    /// Add pools to the whitelist
    fn add_pools(&mut self, pools: Vec<PoolMetadata>) {
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
        self.v2_count = 0;
        self.v3_count = 0;
        self.v4_count = 0;
        self.ekubo_count = 0;
        self.curve_stable_count = 0;
        self.curve_twocrypto_count = 0;
        self.curve_tricrypto_count = 0;
        self.balancer_v2_count = 0;
        self.fluid_count = 0;

        // Add new pools
        self.add_pools(pools);
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
            fluid_pools: self.fluid_count,
        }
    }

    /// Check if there are pending updates
    #[allow(dead_code)]
    pub fn has_pending_updates(&self) -> bool {
        !self.pending_updates.is_empty()
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
