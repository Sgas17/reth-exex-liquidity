// Pool Tracker - Maintains list of pools to track from dynamicWhitelist
//
// Pools are loaded from NATS messages published by dynamicWhitelist orchestrator

use crate::types::{PoolIdentifier, PoolMetadata, Protocol};
use alloy_primitives::Address;
use std::collections::{HashMap, HashSet};
use tracing::info;

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

    /// Statistics
    v2_count: usize,
    v3_count: usize,
    v4_count: usize,
}

impl PoolTracker {
    pub fn new() -> Self {
        Self {
            pools_by_address: HashMap::new(),
            pools_by_id: HashMap::new(),
            tracked_addresses: HashSet::new(),
            tracked_pool_ids: HashSet::new(),
            v2_count: 0,
            v3_count: 0,
            v4_count: 0,
        }
    }

    /// Update the pool whitelist
    pub fn update_whitelist(&mut self, pools: Vec<PoolMetadata>) {
        info!("Updating pool whitelist with {} pools", pools.len());

        // Clear existing
        self.pools_by_address.clear();
        self.pools_by_id.clear();
        self.tracked_addresses.clear();
        self.tracked_pool_ids.clear();
        self.v2_count = 0;
        self.v3_count = 0;
        self.v4_count = 0;

        // Add new pools
        for pool in pools {
            match &pool.pool_id {
                PoolIdentifier::Address(addr) => {
                    self.tracked_addresses.insert(*addr);
                    self.pools_by_address.insert(*addr, pool.clone());
                }
                PoolIdentifier::PoolId(id) => {
                    self.tracked_pool_ids.insert(*id);
                    self.pools_by_id.insert(*id, pool.clone());
                }
            }

            // Update counts
            match pool.protocol {
                Protocol::UniswapV2 => self.v2_count += 1,
                Protocol::UniswapV3 => self.v3_count += 1,
                Protocol::UniswapV4 => self.v4_count += 1,
            }
        }

        info!(
            "Whitelist updated: {} V2, {} V3, {} V4 pools",
            self.v2_count, self.v3_count, self.v4_count
        );
    }

    /// Check if an address is a tracked pool
    pub fn is_tracked_address(&self, address: &Address) -> bool {
        self.tracked_addresses.contains(address)
    }

    /// Check if a pool ID is tracked
    pub fn is_tracked_pool_id(&self, pool_id: &[u8; 32]) -> bool {
        self.tracked_pool_ids.contains(pool_id)
    }

    /// Get pool metadata by address
    pub fn get_by_address(&self, address: &Address) -> Option<&PoolMetadata> {
        self.pools_by_address.get(address)
    }

    /// Get pool metadata by pool ID
    pub fn get_by_pool_id(&self, pool_id: &[u8; 32]) -> Option<&PoolMetadata> {
        self.pools_by_id.get(pool_id)
    }

    /// Get all tracked addresses
    pub fn tracked_addresses(&self) -> &HashSet<Address> {
        &self.tracked_addresses
    }

    /// Get all tracked pool IDs
    pub fn tracked_pool_ids(&self) -> &HashSet<[u8; 32]> {
        &self.tracked_pool_ids
    }

    /// Get statistics
    pub fn stats(&self) -> PoolTrackerStats {
        PoolTrackerStats {
            total_pools: self.pools_by_address.len() + self.pools_by_id.len(),
            v2_pools: self.v2_count,
            v3_pools: self.v3_count,
            v4_pools: self.v4_count,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PoolTrackerStats {
    pub total_pools: usize,
    pub v2_pools: usize,
    pub v3_pools: usize,
    pub v4_pools: usize,
}

impl Default for PoolTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_tracker() {
        let mut tracker = PoolTracker::new();

        let pool = PoolMetadata {
            pool_id: PoolIdentifier::Address(Address::ZERO),
            token0: Address::ZERO,
            token1: Address::ZERO,
            protocol: Protocol::UniswapV2,
            factory: Address::ZERO,
            tick_spacing: None,
            fee: None,
        };

        tracker.update_whitelist(vec![pool]);

        assert!(tracker.is_tracked_address(&Address::ZERO));
        assert_eq!(tracker.stats().v2_pools, 1);
    }
}
