// NATS Client for Whitelist Updates
//
// Subscribes to pool whitelist updates from dynamicWhitelist service
// Minimal format: Just pool addresses (ExEx only needs addresses for filtering)

use crate::types::{PoolIdentifier, PoolMetadata, Protocol, WhitelistUpdate};
use alloy_primitives::Address;
use async_nats::Client;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, warn};

/// Minimal whitelist message from dynamicWhitelist
/// Format: { "pools": ["0x...", "0x..."], "chain": "ethereum", "timestamp": "..." }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistPoolMessage {
    pub pools: Vec<String>,  // Just pool addresses!
    pub chain: String,
    pub timestamp: String,
}

/// NATS client for whitelist subscriptions
pub struct WhitelistNatsClient {
    client: Client,
}

impl WhitelistNatsClient {
    /// Connect to NATS server
    pub async fn connect(nats_url: &str) -> Result<Self> {
        let client = async_nats::connect(nats_url).await?;
        info!("Connected to NATS at {}", nats_url);
        Ok(Self { client })
    }

    /// Subscribe to whitelist updates for a specific chain
    /// Subscribes to: whitelist.pools.{chain}.minimal
    pub async fn subscribe_whitelist(&self, chain: &str) -> Result<async_nats::Subscriber> {
        // Subscribe to minimal topic (just addresses, optimized for ExEx)
        let subject = format!("whitelist.pools.{}.minimal", chain);
        let subscriber = self.client.subscribe(subject.clone()).await?;
        info!("Subscribed to NATS subject: {}", subject);
        Ok(subscriber)
    }

    /// Parse a whitelist message from NATS
    pub fn parse_message(&self, payload: &[u8]) -> Result<WhitelistPoolMessage> {
        let message: WhitelistPoolMessage = serde_json::from_slice(payload)?;
        info!(
            "Parsed whitelist message: {} pools for {}",
            message.pools.len(),
            message.chain
        );
        Ok(message)
    }

    /// Convert minimal whitelist message (just addresses) to internal PoolMetadata
    ///
    /// Note: Since we only receive addresses, we populate other fields with defaults.
    /// The ExEx only needs addresses for filtering - decode_log() auto-detects the protocol.
    pub fn convert_to_pool_metadata(
        &self,
        message: WhitelistPoolMessage,
    ) -> Result<WhitelistUpdate> {
        let mut pools = Vec::new();

        for address_str in message.pools {
            // Remove 0x prefix if present
            let addr_hex = if address_str.starts_with("0x") {
                &address_str[2..]
            } else {
                &address_str
            };

            // Determine if this is a V4 pool (64 hex chars = 32 bytes) or V2/V3 (40 hex chars = 20 bytes)
            let pool_id = if addr_hex.len() == 64 {
                // V4 pool ID (bytes32)
                let mut bytes = [0u8; 32];
                match hex::decode_to_slice(addr_hex, &mut bytes) {
                    Ok(_) => PoolIdentifier::PoolId(bytes),
                    Err(e) => {
                        warn!("Failed to decode pool_id {}: {}, skipping", address_str, e);
                        continue;
                    }
                }
            } else {
                // V2/V3 pool address (20 bytes)
                match Address::from_str(&address_str) {
                    Ok(addr) => PoolIdentifier::Address(addr),
                    Err(e) => {
                        warn!("Failed to parse address {}: {}, skipping", address_str, e);
                        continue;
                    }
                }
            };

            // Since we only have addresses, populate with defaults
            // The ExEx doesn't need these fields - it only uses the address for filtering
            pools.push(PoolMetadata {
                pool_id,
                token0: Address::ZERO,           // Not needed for ExEx filtering
                token1: Address::ZERO,           // Not needed for ExEx filtering
                protocol: Protocol::UniswapV3,   // Default, not used for filtering
                factory: Address::ZERO,          // Not needed for ExEx filtering
                tick_spacing: None,
                fee: None,
            });
        }

        info!(
            "Converted {} pool addresses to metadata for chain {}",
            pools.len(),
            message.chain
        );

        Ok(WhitelistUpdate {
            chain: message.chain,
            generated_at: message.timestamp,
            pools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_message() {
        let json = r#"{
            "pools": [
                "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed"
            ],
            "chain": "ethereum",
            "timestamp": "2025-10-30T15:30:00Z"
        }"#;

        let message: WhitelistPoolMessage = serde_json::from_str(json).unwrap();
        assert_eq!(message.pools.len(), 2);
        assert_eq!(message.chain, "ethereum");
    }

    #[test]
    fn test_convert_addresses() {
        let client = WhitelistNatsClient {
            client: async_nats::Client::new(),
        };

        let message = WhitelistPoolMessage {
            pools: vec![
                "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed".to_string(),
            ],
            chain: "ethereum".to_string(),
            timestamp: "2025-10-30T15:30:00Z".to_string(),
        };

        let update = client.convert_to_pool_metadata(message).unwrap();
        assert_eq!(update.pools.len(), 2);

        // Check first pool is an Address (V2/V3)
        match &update.pools[0].pool_id {
            PoolIdentifier::Address(_) => {}
            _ => panic!("Expected Address identifier"),
        }
    }
}
