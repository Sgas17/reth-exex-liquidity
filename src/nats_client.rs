// NATS Client for Whitelist Updates
//
// Subscribes to pool whitelist updates from dynamicWhitelist service

use crate::types::{PoolIdentifier, PoolMetadata, Protocol, WhitelistUpdate};
use alloy_primitives::Address;
use async_nats::Client;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{debug, info, warn};

/// Pool data from dynamicWhitelist
/// Optimized format for our needs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistPoolMessage {
    pub chain: String,
    pub pools: Vec<PoolData>,
    pub generated_at: String,
    pub metadata: Option<WhitelistMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolData {
    /// Pool address (V2/V3) or pool_id as hex string (V4)
    pub address: String,

    /// Token addresses
    pub token0: String,
    pub token1: String,

    /// Protocol identifier
    pub protocol: String, // "v2", "v3", "v4"

    /// Factory address
    pub factory: String,

    /// V3/V4 specific fields
    pub tick_spacing: Option<i32>,
    pub fee: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistMetadata {
    pub total_pools: usize,
    pub v2_count: Option<usize>,
    pub v3_count: Option<usize>,
    pub v4_count: Option<usize>,
}

/// NATS client for whitelist subscriptions
pub struct WhitelistNatsClient {
    client: Client,
}

impl WhitelistNatsClient {
    /// Connect to NATS server
    pub async fn connect(nats_url: &str) -> Result<Self> {
        info!("Connecting to NATS at {}", nats_url);

        let client = async_nats::connect(nats_url).await?;

        info!("Connected to NATS successfully");

        Ok(Self { client })
    }

    /// Subscribe to whitelist updates for a specific chain
    pub async fn subscribe_whitelist(&self, chain: &str) -> Result<async_nats::Subscriber> {
        // Subscribe to all pool updates for this chain
        let subject = format!("whitelist.pools.{}.>", chain);

        info!("Subscribing to NATS subject: {}", subject);

        let subscriber = self.client.subscribe(subject).await?;

        Ok(subscriber)
    }

    /// Parse a whitelist message from NATS
    pub fn parse_message(&self, payload: &[u8]) -> Result<WhitelistPoolMessage> {
        let message: WhitelistPoolMessage = serde_json::from_slice(payload)?;

        debug!(
            "Parsed whitelist message: {} pools for {}",
            message.pools.len(),
            message.chain
        );

        Ok(message)
    }

    /// Convert WhitelistPoolMessage to our internal format
    pub fn convert_to_pool_metadata(
        &self,
        message: WhitelistPoolMessage,
    ) -> Result<WhitelistUpdate> {
        let mut pools = Vec::new();

        for pool_data in message.pools {
            // Parse protocol
            let protocol = match pool_data.protocol.to_lowercase().as_str() {
                "v2" | "uniswap_v2" | "sushiswap_v2" => Protocol::UniswapV2,
                "v3" | "uniswap_v3" | "sushiswap_v3" => Protocol::UniswapV3,
                "v4" | "uniswap_v4" => Protocol::UniswapV4,
                other => {
                    warn!("Unknown protocol: {}, skipping pool", other);
                    continue;
                }
            };

            // Parse pool identifier
            let pool_id = if protocol == Protocol::UniswapV4 {
                // V4 uses bytes32 pool ID
                // Address string should be 66 chars (0x + 64 hex chars)
                let hex_str = pool_data.address.trim_start_matches("0x");
                if hex_str.len() != 64 {
                    warn!("Invalid V4 pool ID length: {}, skipping", pool_data.address);
                    continue;
                }

                let mut pool_id_bytes = [0u8; 32];
                if let Err(e) = hex::decode_to_slice(hex_str, &mut pool_id_bytes) {
                    warn!(
                        "Failed to decode V4 pool ID {}: {}, skipping",
                        pool_data.address, e
                    );
                    continue;
                }

                PoolIdentifier::PoolId(pool_id_bytes)
            } else {
                // V2/V3 use address
                match Address::from_str(&pool_data.address) {
                    Ok(addr) => PoolIdentifier::Address(addr),
                    Err(e) => {
                        warn!(
                            "Invalid pool address {}: {}, skipping",
                            pool_data.address, e
                        );
                        continue;
                    }
                }
            };

            // Parse token addresses
            let token0 = match Address::from_str(&pool_data.token0) {
                Ok(addr) => addr,
                Err(e) => {
                    warn!(
                        "Invalid token0 address {}: {}, skipping pool",
                        pool_data.token0, e
                    );
                    continue;
                }
            };

            let token1 = match Address::from_str(&pool_data.token1) {
                Ok(addr) => addr,
                Err(e) => {
                    warn!(
                        "Invalid token1 address {}: {}, skipping pool",
                        pool_data.token1, e
                    );
                    continue;
                }
            };

            let factory = match Address::from_str(&pool_data.factory) {
                Ok(addr) => addr,
                Err(e) => {
                    warn!(
                        "Invalid factory address {}: {}, skipping pool",
                        pool_data.factory, e
                    );
                    continue;
                }
            };

            pools.push(PoolMetadata {
                pool_id,
                token0,
                token1,
                protocol,
                factory,
                tick_spacing: pool_data.tick_spacing,
                fee: pool_data.fee,
            });
        }

        info!(
            "Converted {} pools from whitelist message for chain {}",
            pools.len(),
            message.chain
        );

        Ok(WhitelistUpdate {
            chain: message.chain,
            generated_at: message.generated_at,
            pools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_v2_pool() {
        let pool_data = PoolData {
            address: "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
            token0: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            protocol: "v2".to_string(),
            factory: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f".to_string(),
            tick_spacing: None,
            fee: None,
        };

        let message = WhitelistPoolMessage {
            chain: "ethereum".to_string(),
            pools: vec![pool_data],
            generated_at: "2024-01-01T00:00:00Z".to_string(),
            metadata: None,
        };

        let client = WhitelistNatsClient {
            client: unsafe { std::mem::zeroed() }, // Dummy for test
        };

        let result = client.convert_to_pool_metadata(message).unwrap();
        assert_eq!(result.pools.len(), 1);
        assert_eq!(result.pools[0].protocol, Protocol::UniswapV2);
    }

    #[test]
    fn test_parse_v4_pool() {
        // V4 pool ID is bytes32 (64 hex chars)
        let pool_data = PoolData {
            address: "0x1234567890123456789012345678901234567890123456789012345678901234"
                .to_string(),
            token0: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            protocol: "v4".to_string(),
            factory: "0x0000000000000000000000000000000000000000".to_string(),
            tick_spacing: Some(60),
            fee: Some(3000),
        };

        let message = WhitelistPoolMessage {
            chain: "ethereum".to_string(),
            pools: vec![pool_data],
            generated_at: "2024-01-01T00:00:00Z".to_string(),
            metadata: None,
        };

        let client = WhitelistNatsClient {
            client: unsafe { std::mem::zeroed() }, // Dummy for test
        };

        let result = client.convert_to_pool_metadata(message).unwrap();
        assert_eq!(result.pools.len(), 1);
        assert_eq!(result.pools[0].protocol, Protocol::UniswapV4);
        assert!(result.pools[0].pool_id.as_pool_id().is_some());
    }
}
