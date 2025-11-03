// NATS Client for Whitelist Updates
//
// Subscribes to pool whitelist updates from dynamicWhitelist service
// Minimal format: Just pool addresses (ExEx only needs addresses for filtering)

use crate::types::{PoolIdentifier, PoolMetadata, Protocol};
use alloy_primitives::Address;
use async_nats::Client;
use eyre::Result;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{info, warn};

/// Minimal whitelist message from dynamicWhitelist
/// Supports three message types:
/// 1. Add: { "type": "add", "pools": ["0x..."], ... }
/// 2. Remove: { "type": "remove", "pools": ["0x..."], ... }
/// 3. Full: { "type": "full", "pools": ["0x..."], ... } (backward compatible)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistPoolMessage {
    #[serde(rename = "type", default = "default_message_type")]
    pub message_type: String,  // "add", "remove", or "full"
    pub pools: Vec<String>,    // Pool addresses
    pub chain: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<i64>,
}

/// Default message type for backward compatibility (messages without "type" field)
fn default_message_type() -> String {
    "full".to_string()
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

    /// Convert minimal whitelist message to appropriate WhitelistUpdate
    ///
    /// Handles three message types:
    /// - "add": Adds new pools to the whitelist
    /// - "remove": Removes pools from the whitelist (pools = addresses to remove)
    /// - "full": Full replacement (backward compatible)
    pub fn convert_to_pool_update(
        &self,
        message: WhitelistPoolMessage,
    ) -> Result<crate::pool_tracker::WhitelistUpdate> {
        use crate::pool_tracker::WhitelistUpdate as Update;

        match message.message_type.as_str() {
            "add" => {
                let pools = self.parse_pool_addresses(&message.pools)?;
                info!(
                    "📥 Received ADD update: +{} pools for {} (snapshot: {:?})",
                    pools.len(),
                    message.chain,
                    message.snapshot_id
                );
                Ok(Update::Add(pools))
            }
            "remove" => {
                let pool_ids = self.parse_pool_identifiers(&message.pools)?;
                info!(
                    "📥 Received REMOVE update: -{} pools for {} (snapshot: {:?})",
                    pool_ids.len(),
                    message.chain,
                    message.snapshot_id
                );
                Ok(Update::Remove(pool_ids))
            }
            "full" => {
                let pools = self.parse_pool_addresses(&message.pools)?;
                info!(
                    "📥 Received FULL update: {} pools for {} (snapshot: {:?})",
                    pools.len(),
                    message.chain,
                    message.snapshot_id
                );
                Ok(Update::Replace(pools))
            }
            unknown => {
                warn!(
                    "Unknown message type '{}', treating as full replacement",
                    unknown
                );
                let pools = self.parse_pool_addresses(&message.pools)?;
                Ok(Update::Replace(pools))
            }
        }
    }

    /// Parse pool addresses into PoolMetadata (for Add/Full updates)
    fn parse_pool_addresses(&self, addresses: &[String]) -> Result<Vec<PoolMetadata>> {
        let mut pools = Vec::new();

        for address_str in addresses {
            let addr_hex = if address_str.starts_with("0x") {
                &address_str[2..]
            } else {
                address_str
            };

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
                match Address::from_str(address_str) {
                    Ok(addr) => PoolIdentifier::Address(addr),
                    Err(e) => {
                        warn!("Failed to parse address {}: {}, skipping", address_str, e);
                        continue;
                    }
                }
            };

            pools.push(PoolMetadata {
                pool_id,
                token0: Address::ZERO,
                token1: Address::ZERO,
                protocol: Protocol::UniswapV3,
                factory: Address::ZERO,
                tick_spacing: None,
                fee: None,
            });
        }

        Ok(pools)
    }

    /// Parse pool addresses into PoolIdentifiers (for Remove updates)
    fn parse_pool_identifiers(&self, addresses: &[String]) -> Result<Vec<PoolIdentifier>> {
        let mut pool_ids = Vec::new();

        for address_str in addresses {
            let addr_hex = if address_str.starts_with("0x") {
                &address_str[2..]
            } else {
                address_str
            };

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
                match Address::from_str(address_str) {
                    Ok(addr) => PoolIdentifier::Address(addr),
                    Err(e) => {
                        warn!("Failed to parse address {}: {}, skipping", address_str, e);
                        continue;
                    }
                }
            };

            pool_ids.push(pool_id);
        }

        Ok(pool_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_message_backward_compat() {
        // Test backward compatibility - message without "type" field defaults to "full"
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
        assert_eq!(message.message_type, "full"); // Should default to "full"
    }

    #[test]
    fn test_parse_differential_messages() {
        // Test "add" message
        let add_json = r#"{
            "type": "add",
            "pools": ["0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"],
            "chain": "ethereum",
            "timestamp": "2025-10-30T15:30:00Z",
            "snapshot_id": 1234567890
        }"#;
        let add_msg: WhitelistPoolMessage = serde_json::from_str(add_json).unwrap();
        assert_eq!(add_msg.message_type, "add");
        assert_eq!(add_msg.snapshot_id, Some(1234567890));

        // Test "remove" message
        let remove_json = r#"{
            "type": "remove",
            "pools": ["0xcbcdf9626bc03e24f779434178a73a0b4bad62ed"],
            "chain": "ethereum",
            "timestamp": "2025-10-30T15:30:00Z",
            "snapshot_id": 1234567891
        }"#;
        let remove_msg: WhitelistPoolMessage = serde_json::from_str(remove_json).unwrap();
        assert_eq!(remove_msg.message_type, "remove");
    }

    #[tokio::test]
    async fn test_convert_to_pool_update() {
        use crate::pool_tracker::WhitelistUpdate;

        // Create a real client for testing (will fail to connect, but that's ok)
        let client = match WhitelistNatsClient::connect("nats://localhost:4222").await {
            Ok(c) => c,
            Err(_) => {
                // If NATS not available, skip test
                println!("Skipping test - NATS not available");
                return;
            }
        };

        // Test Add update
        let add_message = WhitelistPoolMessage {
            message_type: "add".to_string(),
            pools: vec!["0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string()],
            chain: "ethereum".to_string(),
            timestamp: "2025-10-30T15:30:00Z".to_string(),
            snapshot_id: Some(123),
        };
        let update = client.convert_to_pool_update(add_message).unwrap();
        assert!(matches!(update, WhitelistUpdate::Add(_)));

        // Test Remove update
        let remove_message = WhitelistPoolMessage {
            message_type: "remove".to_string(),
            pools: vec!["0xcbcdf9626bc03e24f779434178a73a0b4bad62ed".to_string()],
            chain: "ethereum".to_string(),
            timestamp: "2025-10-30T15:30:00Z".to_string(),
            snapshot_id: Some(124),
        };
        let update = client.convert_to_pool_update(remove_message).unwrap();
        assert!(matches!(update, WhitelistUpdate::Remove(_)));

        // Test Full/Replace update
        let full_message = WhitelistPoolMessage {
            message_type: "full".to_string(),
            pools: vec![
                "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed".to_string(),
            ],
            chain: "ethereum".to_string(),
            timestamp: "2025-10-30T15:30:00Z".to_string(),
            snapshot_id: Some(125),
        };
        let update = client.convert_to_pool_update(full_message).unwrap();
        if let WhitelistUpdate::Replace(pools) = update {
            assert_eq!(pools.len(), 2);
        } else {
            panic!("Expected Replace update");
        }
    }
}
