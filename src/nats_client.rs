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
/// 1. Add: { "type": "add", "pools": ["0x..."], "protocols": ["v2", "v3", "v4"], ... }
/// 2. Remove: { "type": "remove", "pools": ["0x..."], "protocols": ["v2", "v3", "v4"], ... }
/// 3. Full: { "type": "full", "pools": ["0x..."], "protocols": ["v2", "v3", "v4"], ... }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhitelistPoolMessage {
    #[serde(rename = "type", default = "default_message_type")]
    pub message_type: String, // "add", "remove", or "full"
    pub pools: Vec<String>, // Pool addresses (20 bytes for V2/V3, 32 bytes for V4)
    #[serde(default)]
    pub protocols: Vec<String>, // Protocol version for each pool: "v2", "v3", or "v4"
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
                let pools = self.parse_pool_addresses(&message.pools, &message.protocols)?;
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
                let pools = self.parse_pool_addresses(&message.pools, &message.protocols)?;
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
                let pools = self.parse_pool_addresses(&message.pools, &message.protocols)?;
                Ok(Update::Replace(pools))
            }
        }
    }

    /// Parse pool addresses into PoolMetadata (for Add/Full updates)
    fn parse_pool_addresses(
        &self,
        addresses: &[String],
        protocols: &[String],
    ) -> Result<Vec<PoolMetadata>> {
        // Require protocols array to match pools array length
        if addresses.len() != protocols.len() {
            return Err(eyre::eyre!(
                "Pools and protocols array length mismatch: {} pools vs {} protocols. Cannot safely parse whitelist.",
                addresses.len(),
                protocols.len()
            ));
        }

        let mut pools = Vec::new();

        for (i, address_str) in addresses.iter().enumerate() {
            let protocol_str = &protocols[i];
            let protocol = match protocol_str.as_str() {
                "v2" => Protocol::UniswapV2,
                "v3" => Protocol::UniswapV3,
                "v4" => Protocol::UniswapV4,
                unknown => {
                    warn!(
                        "Unknown protocol '{}' for pool {}, skipping",
                        unknown, address_str
                    );
                    continue;
                }
            };

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
                protocol,
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
    fn test_parse_minimal_message_with_protocols() {
        // Test new format with protocols array
        let json = r#"{
            "type": "add",
            "pools": [
                "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed"
            ],
            "protocols": ["v3", "v2"],
            "chain": "ethereum",
            "timestamp": "2025-10-30T15:30:00Z"
        }"#;

        let message: WhitelistPoolMessage = serde_json::from_str(json).unwrap();
        assert_eq!(message.pools.len(), 2);
        assert_eq!(message.protocols.len(), 2);
        assert_eq!(message.protocols[0], "v3");
        assert_eq!(message.protocols[1], "v2");
        assert_eq!(message.chain, "ethereum");
        assert_eq!(message.message_type, "add");
    }

    #[test]
    fn test_parse_differential_messages() {
        // Test "add" message
        let add_json = r#"{
            "type": "add",
            "pools": ["0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"],
            "protocols": ["v3"],
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
            "protocols": ["v3"],
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
            protocols: vec!["v3".to_string()],
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
            protocols: vec!["v3".to_string()],
            chain: "ethereum".to_string(),
            timestamp: "2025-10-30T15:30:00Z".to_string(),
            snapshot_id: Some(124),
        };
        let update = client.convert_to_pool_update(remove_message).unwrap();
        assert!(matches!(update, WhitelistUpdate::Remove(_)));

        // Test Full/Replace update with mixed protocols
        let full_message = WhitelistPoolMessage {
            message_type: "full".to_string(),
            pools: vec![
                "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed".to_string(),
            ],
            protocols: vec!["v3".to_string(), "v2".to_string()],
            chain: "ethereum".to_string(),
            timestamp: "2025-10-30T15:30:00Z".to_string(),
            snapshot_id: Some(125),
        };
        let update = client.convert_to_pool_update(full_message).unwrap();
        if let WhitelistUpdate::Replace(pools) = update {
            assert_eq!(pools.len(), 2);
            assert_eq!(pools[0].protocol, Protocol::UniswapV3);
            assert_eq!(pools[1].protocol, Protocol::UniswapV2);
        } else {
            panic!("Expected Replace update");
        }
    }

    #[test]
    fn test_parse_v2_and_v4_pools() {
        // Test message with real V2 (USDC/WETH on Uniswap V2) and real V4 pool
        // V2: USDC/WETH pair - 0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc
        // V4: Real highly-used V4 pool - 0xdce6394339af00981949f5f3baf27e3610c76326a700af57e4b3e3ae4977f78d
        let json = r#"{
            "type": "add",
            "pools": [
                "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
                "0xdce6394339af00981949f5f3baf27e3610c76326a700af57e4b3e3ae4977f78d"
            ],
            "protocols": ["v2", "v4"],
            "chain": "ethereum",
            "timestamp": "2025-11-04T18:00:00Z",
            "snapshot_id": 1730750400000
        }"#;

        let message: WhitelistPoolMessage = serde_json::from_str(json).unwrap();
        assert_eq!(message.pools.len(), 2);
        assert_eq!(message.protocols.len(), 2);
        assert_eq!(message.protocols[0], "v2");
        assert_eq!(message.protocols[1], "v4");

        // Verify address lengths
        assert_eq!(message.pools[0].len(), 42); // 0x + 40 hex chars for V2
        assert_eq!(message.pools[1].len(), 66); // 0x + 64 hex chars for V4
    }

    #[tokio::test]
    async fn test_convert_v2_and_v4_pools() {
        use crate::pool_tracker::WhitelistUpdate;

        // Create a client (will fail to connect, but that's ok for parsing test)
        let client = match WhitelistNatsClient::connect("nats://localhost:4222").await {
            Ok(c) => c,
            Err(_) => {
                // If NATS not available, skip test
                println!("Skipping test - NATS not available");
                return;
            }
        };

        // Test parsing real V2 USDC/WETH pair and real V4 pool together
        let message = WhitelistPoolMessage {
            message_type: "add".to_string(),
            pools: vec![
                "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".to_string(), // V2 USDC/WETH
                "0xdce6394339af00981949f5f3baf27e3610c76326a700af57e4b3e3ae4977f78d".to_string(), // V4 pool
            ],
            protocols: vec!["v2".to_string(), "v4".to_string()],
            chain: "ethereum".to_string(),
            timestamp: "2025-11-04T18:00:00Z".to_string(),
            snapshot_id: Some(1730750400000),
        };

        let update = client.convert_to_pool_update(message).unwrap();
        if let WhitelistUpdate::Add(pools) = update {
            assert_eq!(pools.len(), 2);

            // Check V2 pool
            assert_eq!(pools[0].protocol, Protocol::UniswapV2);
            assert!(matches!(pools[0].pool_id, PoolIdentifier::Address(_)));

            // Check V4 pool
            assert_eq!(pools[1].protocol, Protocol::UniswapV4);
            assert!(matches!(pools[1].pool_id, PoolIdentifier::PoolId(_)));
        } else {
            panic!("Expected Add update");
        }
    }
}
