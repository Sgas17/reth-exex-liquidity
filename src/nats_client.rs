// NATS Client for Whitelist Updates
//
// Subscribes to the orchestrator's canonical pool whitelist
// (`whitelist.pools.{chain}.{full,add,remove}`), which carries token addresses,
// decimals, and protocol metadata the ExEx arena writer needs.

use crate::types::{PoolIdentifier, PoolMetadata, Protocol, TokenMetadata};
use alloy_primitives::Address;
use async_nats::Client;
use eyre::Result;
use futures::StreamExt;
use serde::Deserialize;
use std::str::FromStr;
use std::time::Duration;
use tracing::{info, warn};

// ── Rich (`.full`) whitelist parsing (ITE-16) ───────────────────────────────
//
// The ExEx historically consumed the address-only `.minimal` topic. As the
// arena writer it also needs token addresses + decimals + protocol metadata,
// which the orchestrator already publishes on `whitelist.pools.{chain}.full` as
// the richer `WhitelistPool`. These deser structs mirror that wire format;
// `extra_tokens` is carried for multi-token Curve/Balancer hydration, and
// `additional_data.version` is carried for CurveTwoCrypto storage-layout selection.
// Other protocol fields not yet consumed (ekubo/fluid config) remain ignored
// until the protocols that need them are hydrated.

/// Token entry in the rich whitelist (`common::Token` on the wire).
#[derive(Debug, Clone, Deserialize)]
struct CanonicalToken {
    address: String,
    decimals: u8,
}

fn canonical_token_to_metadata(t: &CanonicalToken) -> Option<TokenMetadata> {
    Some(TokenMetadata {
        address: Address::from_str(&t.address).ok()?,
        decimals: t.decimals,
    })
}

/// Pool entry in the rich whitelist (orchestrator `WhitelistPool`).
#[derive(Debug, Clone, Deserialize)]
struct CanonicalPool {
    address: String,
    protocol: String,
    token0: CanonicalToken,
    token1: CanonicalToken,
    #[serde(default)]
    fee: Option<u32>,
    #[serde(default)]
    tick_spacing: Option<i32>,
    #[serde(default)]
    pool_id: Option<String>,
    #[serde(default)]
    factory: Option<String>,
    #[serde(default)]
    extra_tokens: Vec<CanonicalToken>,
    #[serde(default)]
    ekubo_fee: Option<u64>,
    #[serde(default)]
    ekubo_type_config: Option<u32>,
    #[serde(default)]
    additional_data: Option<serde_json::Value>,
}

/// Full rich-snapshot envelope (`whitelist.pools.{chain}.full`).
#[derive(Debug, Clone, Deserialize)]
struct FullSnapshotMessage {
    chain: String,
    pools: Vec<CanonicalPool>,
}

/// Map a whitelist protocol string to the ExEx `Protocol`.
fn protocol_from_str(s: &str) -> Option<Protocol> {
    Some(match s {
        "v2" | "uniswap_v2" => Protocol::UniswapV2,
        "v3" | "uniswap_v3" => Protocol::UniswapV3,
        "v4" | "uniswap_v4" => Protocol::UniswapV4,
        "ekubo" => Protocol::Ekubo,
        "curve_stable" => Protocol::CurveStable,
        "curve_twocrypto" => Protocol::CurveTwoCrypto,
        "curve_tricrypto" => Protocol::CurveTricrypto,
        "balancer_v2_weighted" => Protocol::BalancerV2Weighted,
        "fluid" => Protocol::Fluid,
        _ => return None,
    })
}

/// Parse a 20-byte pool address or, for `pool_id`-keyed protocols, the 32-byte id.
fn parse_pool_identifier(address: &str, pool_id: Option<&str>) -> Option<PoolIdentifier> {
    let key = pool_id.unwrap_or(address);
    let hex_str = key.strip_prefix("0x").unwrap_or(key);
    if hex_str.len() == 64 {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(hex_str, &mut bytes).ok()?;
        Some(PoolIdentifier::PoolId(bytes))
    } else {
        Address::from_str(key).ok().map(PoolIdentifier::Address)
    }
}

fn canonical_pool_to_metadata(p: &CanonicalPool) -> Option<PoolMetadata> {
    let protocol = protocol_from_str(&p.protocol)?;
    let pool_id = parse_pool_identifier(&p.address, p.pool_id.as_deref())?;
    let token0 = Address::from_str(&p.token0.address).ok()?;
    let token1 = Address::from_str(&p.token1.address).ok()?;
    let factory = p
        .factory
        .as_deref()
        .and_then(|f| Address::from_str(f).ok())
        .unwrap_or(Address::ZERO);
    let extra_tokens = p
        .extra_tokens
        .iter()
        .filter_map(canonical_token_to_metadata)
        .collect();
    let twocrypto_version = p
        .additional_data
        .as_ref()
        .and_then(|data| data.get("version"))
        .and_then(|version| version.as_str())
        .map(str::to_owned);
    let (balancer_weights, balancer_swap_fee) = if protocol == Protocol::BalancerV2Weighted {
        (
            parse_balancer_weights(p.additional_data.as_ref()),
            p.additional_data
                .as_ref()
                .and_then(|d| d.get("swap_fee"))
                .and_then(|v| v.as_u64()),
        )
    } else {
        (None, None)
    };
    Some(PoolMetadata {
        pool_id,
        token0,
        token1,
        protocol,
        factory,
        tick_spacing: p.tick_spacing,
        fee: p.fee,
        token0_decimals: Some(p.token0.decimals),
        token1_decimals: Some(p.token1.decimals),
        extra_tokens,
        twocrypto_version,
        ekubo_fee: p.ekubo_fee,
        ekubo_type_config: p.ekubo_type_config,
        balancer_weights,
        balancer_swap_fee,
    })
}

/// Parse Balancer V2 normalized weights from whitelist `additional_data.weights`
/// (a JSON array, 1e18 scale). Each entry may be a number or a decimal/hex string.
/// Returns `None` if absent or any element is unparseable — hydration then skips
/// the pool (data-integrity rule: never default on-chain/weight metadata).
fn parse_balancer_weights(additional_data: Option<&serde_json::Value>) -> Option<Vec<u64>> {
    let weights = additional_data?.get("weights")?.as_array()?;
    let mut out = Vec::with_capacity(weights.len());
    for value in weights {
        let w = if let Some(n) = value.as_u64() {
            n
        } else {
            let s = value.as_str()?.trim();
            match s.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok()?,
                None => s.parse::<u64>().ok()?,
            }
        };
        out.push(w);
    }
    Some(out)
}

/// Parse the rich `.full` whitelist snapshot into enriched `PoolMetadata`,
/// carrying real token addresses + decimals. Pools with an unknown protocol or
/// unparseable addresses are skipped (logged), never defaulted.
pub fn parse_full_snapshot(payload: &[u8]) -> Result<Vec<PoolMetadata>> {
    let snapshot: FullSnapshotMessage = serde_json::from_slice(payload)?;
    let mut pools = Vec::with_capacity(snapshot.pools.len());
    for p in &snapshot.pools {
        match canonical_pool_to_metadata(p) {
            Some(meta) => pools.push(meta),
            None => warn!("Skipping unparseable whitelist pool {}", p.address),
        }
    }
    info!(
        "Parsed rich whitelist snapshot: {} pools for {}",
        pools.len(),
        snapshot.chain
    );
    Ok(pools)
}

/// Remove envelope (`whitelist.pools.{chain}.remove`): pool addresses to drop.
#[derive(Debug, Clone, Deserialize)]
struct RemoveSnapshotMessage {
    chain: String,
    pool_addresses: Vec<String>,
}

/// Parse a canonical remove snapshot into pool identifiers.
pub fn parse_remove_snapshot(payload: &[u8]) -> Result<Vec<PoolIdentifier>> {
    let msg: RemoveSnapshotMessage = serde_json::from_slice(payload)?;
    let mut ids = Vec::with_capacity(msg.pool_addresses.len());
    for a in &msg.pool_addresses {
        match parse_pool_identifier(a, None) {
            Some(id) => ids.push(id),
            None => warn!("Skipping unparseable remove address {}", a),
        }
    }
    info!(
        "Parsed rich whitelist remove: {} pools for {}",
        ids.len(),
        msg.chain
    );
    Ok(ids)
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

    /// Subscribe to the canonical per-chain whitelist for live deltas.
    ///
    /// Subscribes to the wildcard `whitelist.pools.{chain}.*` and the caller
    /// dispatches by subject suffix (`.full` / `.add` / `.remove`) via
    /// [`WhitelistNatsClient::canonical_update`], ignoring the legacy `.minimal`
    /// topic. These carry enriched metadata (token decimals + protocol fields).
    pub async fn subscribe_whitelist(&self, chain: &str) -> Result<async_nats::Subscriber> {
        let subject = format!("whitelist.pools.{}.*", chain);
        let subscriber = self.client.subscribe(subject.clone()).await?;
        info!("Subscribed to NATS subject: {}", subject);
        Ok(subscriber)
    }

    /// Subscribe to the canonical rich full whitelist subject.
    ///
    /// Startup hydration uses this with `request_reseed()` so ExEx receives the
    /// same `WhitelistPool` payload as arena readers: token addresses, decimals,
    /// fee/tick metadata, and protocol-specific fields.
    pub async fn subscribe_full_whitelist(&self, chain: &str) -> Result<async_nats::Subscriber> {
        let subject = format!("whitelist.pools.{}.full", chain);
        let subscriber = self.client.subscribe(subject.clone()).await?;
        info!("Subscribed to rich whitelist subject: {}", subject);
        Ok(subscriber)
    }

    /// Ask whitelist_service to re-publish cached full snapshots on the standard
    /// subjects (`whitelist.pools.{chain}.full`, minimal, HL perps).
    pub async fn request_reseed(&self) -> Result<()> {
        self.client.publish("whitelist.reseed", "".into()).await?;
        info!("Requested whitelist reseed");
        Ok(())
    }

    /// Wait for one rich full snapshot from a `.full` subscription and parse it.
    pub async fn next_full_snapshot(
        &self,
        subscriber: &mut async_nats::Subscriber,
        timeout: Duration,
    ) -> Result<Vec<PoolMetadata>> {
        let message = tokio::time::timeout(timeout, subscriber.next())
            .await
            .map_err(|_| eyre::eyre!("timed out waiting for rich whitelist full snapshot"))?
            .ok_or_else(|| eyre::eyre!("rich whitelist full subscription closed"))?;

        parse_full_snapshot(&message.payload)
    }

    /// Dispatch a canonical whitelist message (by `.full` / `.add` / `.remove`
    /// subject suffix) into a `WhitelistUpdate` carrying enriched metadata
    /// (token addresses + decimals + protocol fields). Returns `Ok(None)` for
    /// ignored subjects (e.g. the legacy `.minimal`).
    pub fn canonical_update(
        subject_suffix: &str,
        payload: &[u8],
    ) -> Result<Option<crate::pool_tracker::WhitelistUpdate>> {
        use crate::pool_tracker::WhitelistUpdate as Update;
        // AddSnapshot shares FullSnapshot's shape (chain + Vec<WhitelistPool>).
        let update = match subject_suffix {
            "full" => Update::Replace(parse_full_snapshot(payload)?),
            "add" => Update::Add(parse_full_snapshot(payload)?),
            "remove" => Update::Remove(parse_remove_snapshot(payload)?),
            _ => return Ok(None),
        };
        Ok(Some(update))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_snapshot_carries_token_decimals() {
        // A rich `.full` whitelist payload as published by the orchestrator.
        let json = r#"{
            "snapshot_id": 1,
            "chain": "ethereum",
            "pools": [
                {
                    "address": "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
                    "protocol": "v2",
                    "token0": {"address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "symbol": "USDC", "decimals": 6},
                    "token1": {"address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "symbol": "WETH", "decimals": 18},
                    "fee": 3000,
                    "extra_tokens": []
                }
            ]
        }"#;

        let pools = super::parse_full_snapshot(json.as_bytes()).expect("parse full snapshot");
        assert_eq!(pools.len(), 1);
        let p = &pools[0];
        assert_eq!(p.protocol, Protocol::UniswapV2);
        assert!(matches!(p.pool_id, PoolIdentifier::Address(_)));
        assert_ne!(p.token0, Address::ZERO);
        assert_ne!(p.token1, Address::ZERO);
        assert_eq!(p.token0_decimals, Some(6));
        assert_eq!(p.token1_decimals, Some(18));
        assert_eq!(p.fee, Some(3000));
    }

    #[test]
    fn parse_full_snapshot_carries_balancer_weights() {
        // Balancer V2 weighted pool with poolId + additional_data.weights.
        let json = r#"{
            "snapshot_id": 1,
            "chain": "ethereum",
            "pools": [
                {
                    "address": "0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56",
                    "pool_id": "0x5c6ee304399dbdb9c8ef030ab642b10820db8f56000200000000000000000014",
                    "protocol": "balancer_v2_weighted",
                    "token0": {"address": "0xba100000625a3754423978a60c9317c58a424e3D", "symbol": "BAL", "decimals": 18},
                    "token1": {"address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "symbol": "WETH", "decimals": 18},
                    "extra_tokens": [],
                    "additional_data": {"weights": ["800000000000000000", "200000000000000000"], "swap_fee": 3000000000000000}
                }
            ]
        }"#;

        let pools = super::parse_full_snapshot(json.as_bytes()).expect("parse full snapshot");
        assert_eq!(pools.len(), 1);
        let p = &pools[0];
        assert_eq!(p.protocol, Protocol::BalancerV2Weighted);
        assert!(matches!(p.pool_id, PoolIdentifier::PoolId(_)));
        assert_eq!(
            p.balancer_weights.as_deref(),
            Some(&[800_000_000_000_000_000u64, 200_000_000_000_000_000][..]),
            "80/20 weights parsed from additional_data.weights"
        );
        assert_eq!(
            p.balancer_swap_fee,
            Some(3_000_000_000_000_000),
            "swap_fee parsed from additional_data.swap_fee"
        );
    }

    #[test]
    fn parse_full_snapshot_carries_twocrypto_version() {
        let json = br#"{"snapshot_id":1,"chain":"ethereum","pools":[{"address":"0x0000000000000000000000000000000000000001","protocol":"curve_twocrypto","token0":{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","decimals":6},"token1":{"address":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","symbol":"WETH","decimals":18},"additional_data":{"version":"v2.0.0"}}]}"#;

        let pools = super::parse_full_snapshot(json).expect("parse full snapshot");
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].protocol, Protocol::CurveTwoCrypto);
        assert_eq!(pools[0].twocrypto_version.as_deref(), Some("v2.0.0"));
    }

    #[test]
    fn parse_full_snapshot_carries_ekubo_metadata() {
        let json = br#"{"snapshot_id":1,"chain":"ethereum","pools":[{"address":"0x00000000000014aA86C5d3c41765bb24e11bd701","protocol":"ekubo","token0":{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","decimals":6},"token1":{"address":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","symbol":"WETH","decimals":18},"tick_spacing":10,"pool_id":"0x1111111111111111111111111111111111111111111111111111111111111111","factory":"0x00000000000014aA86C5d3c41765bb24e11bd701","ekubo_fee":42,"ekubo_type_config":2147483658}]}"#;

        let pools = super::parse_full_snapshot(json).expect("parse full snapshot");
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].protocol, Protocol::Ekubo);
        assert_eq!(pools[0].tick_spacing, Some(10));
        assert_eq!(pools[0].ekubo_fee, Some(42));
        assert_eq!(pools[0].ekubo_type_config, Some(0x8000_000a));
    }

    #[test]
    fn parse_full_snapshot_carries_extra_tokens() {
        let json = br#"{"snapshot_id":1,"chain":"ethereum","pools":[{"address":"0x0000000000000000000000000000000000000001","protocol":"curve_tricrypto","token0":{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","decimals":6},"token1":{"address":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","symbol":"WETH","decimals":18},"extra_tokens":[{"address":"0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599","symbol":"WBTC","decimals":8}]}]}"#;

        let pools = super::parse_full_snapshot(json).expect("parse full snapshot");
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].protocol, Protocol::CurveTricrypto);
        assert_eq!(pools[0].extra_tokens.len(), 1);
        assert_eq!(pools[0].extra_tokens[0].decimals, 8);
        assert_ne!(pools[0].extra_tokens[0].address, Address::ZERO);
    }

    const FULL_V2: &[u8] = br#"{"snapshot_id":1,"chain":"ethereum","pools":[{"address":"0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc","protocol":"v2","token0":{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","decimals":6},"token1":{"address":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","symbol":"WETH","decimals":18}}]}"#;

    #[test]
    fn canonical_update_dispatches_by_subject() {
        use crate::pool_tracker::WhitelistUpdate;
        match WhitelistNatsClient::canonical_update("full", FULL_V2)
            .unwrap()
            .unwrap()
        {
            WhitelistUpdate::Replace(p) => assert_eq!(p.len(), 1),
            other => panic!("expected Replace, got {other:?}"),
        }
        match WhitelistNatsClient::canonical_update("add", FULL_V2)
            .unwrap()
            .unwrap()
        {
            WhitelistUpdate::Add(p) => assert_eq!(p.len(), 1),
            other => panic!("expected Add, got {other:?}"),
        }
        // Legacy/unknown subjects are ignored.
        assert!(WhitelistNatsClient::canonical_update("minimal", FULL_V2)
            .unwrap()
            .is_none());
        assert!(WhitelistNatsClient::canonical_update("other", FULL_V2)
            .unwrap()
            .is_none());
    }

    #[test]
    fn canonical_remove_parses_pool_id_and_address() {
        use crate::pool_tracker::WhitelistUpdate;
        // A 32-byte (V4-style) pool id and a 20-byte (V2-style) address.
        let remove = br#"{"snapshot_id":1,"chain":"ethereum","pool_addresses":["0x0000000000000000000000000000000000000000000000000000000000000002","0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"]}"#;
        match WhitelistNatsClient::canonical_update("remove", remove)
            .unwrap()
            .unwrap()
        {
            WhitelistUpdate::Remove(ids) => {
                assert_eq!(ids.len(), 2);
                assert!(
                    matches!(ids[0], PoolIdentifier::PoolId(_)),
                    "32-byte -> PoolId"
                );
                assert!(
                    matches!(ids[1], PoolIdentifier::Address(_)),
                    "20-byte -> Address"
                );
            }
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    /// End-to-end (round 04 regression): two V4 pools sharing one PoolManager
    /// address are both tracked by `pool_id`, and a canonical remove by `pool_id`
    /// drops exactly one of them.
    #[test]
    fn canonical_add_then_remove_v4_by_pool_id_updates_tracker() {
        use crate::pool_tracker::{PoolTracker, WhitelistUpdate};
        let add = br#"{"snapshot_id":1,"chain":"ethereum","pools":[
            {"address":"0x000000000000000000000000000000000000beef","protocol":"v4","pool_id":"0x0000000000000000000000000000000000000000000000000000000000000001","token0":{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","decimals":6},"token1":{"address":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","symbol":"WETH","decimals":18}},
            {"address":"0x000000000000000000000000000000000000beef","protocol":"v4","pool_id":"0x0000000000000000000000000000000000000000000000000000000000000002","token0":{"address":"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48","symbol":"USDC","decimals":6},"token1":{"address":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","symbol":"WETH","decimals":18}}]}"#;
        let remove = br#"{"snapshot_id":2,"chain":"ethereum","pool_addresses":["0x0000000000000000000000000000000000000000000000000000000000000002"]}"#;

        let mut id1 = [0u8; 32];
        id1[31] = 1;
        let mut id2 = [0u8; 32];
        id2[31] = 2;

        let mut tracker = PoolTracker::new();

        let Some(WhitelistUpdate::Add(pools)) =
            WhitelistNatsClient::canonical_update("add", add).unwrap()
        else {
            panic!("expected Add");
        };
        assert_eq!(
            pools.len(),
            2,
            "both V4 pools must parse despite shared address"
        );
        tracker.queue_update(WhitelistUpdate::Add(pools));
        assert!(tracker.get_by_pool_id(&id1).is_some());
        assert!(tracker.get_by_pool_id(&id2).is_some());

        let Some(WhitelistUpdate::Remove(ids)) =
            WhitelistNatsClient::canonical_update("remove", remove).unwrap()
        else {
            panic!("expected Remove");
        };
        tracker.queue_update(WhitelistUpdate::Remove(ids));
        assert!(tracker.get_by_pool_id(&id1).is_some(), "id1 must remain");
        assert!(
            tracker.get_by_pool_id(&id2).is_none(),
            "id2 removed by pool_id"
        );
    }
}
