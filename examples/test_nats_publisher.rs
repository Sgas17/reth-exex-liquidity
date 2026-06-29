// Test NATS Publisher
//
// Publishes a mock *rich* (`.full`) whitelist snapshot to manually test ExEx
// NATS integration against the canonical whitelist format the ExEx now consumes
// (token addresses + decimals + protocol metadata).

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Test NATS Publisher (rich .full whitelist)");

    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
    println!("Connecting to NATS at {}", nats_url);

    let client = async_nats::connect(&nats_url).await?;
    println!("✅ Connected to NATS");

    // Canonical FullSnapshot payload, matching whitelist_orchestrator's
    // `WhitelistPool` wire format (token0/token1 as `{address, symbol, decimals}`).
    let payload = serde_json::json!({
        "snapshot_id": 1,
        "chain": "ethereum",
        "pools": [
            {
                "address": "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
                "protocol": "v2",
                "token0": {"address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "symbol": "USDC", "decimals": 6},
                "token1": {"address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "symbol": "WETH", "decimals": 18}
            },
            {
                "address": "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
                "protocol": "v3",
                "token0": {"address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", "symbol": "USDC", "decimals": 6},
                "token1": {"address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", "symbol": "WETH", "decimals": 18},
                "fee": 500,
                "tick_spacing": 10
            }
        ]
    });

    let subject = "whitelist.pools.ethereum.full";
    println!("\n📤 Publishing rich whitelist to subject: {subject}");
    client
        .publish(subject.to_string(), serde_json::to_vec(&payload)?.into())
        .await?;
    println!("✅ Whitelist published successfully (2 pools)");

    println!("\n💡 The ExEx should receive this if running with:");
    println!("   NATS_URL={nats_url}");
    println!("   CHAIN=ethereum");

    Ok(())
}
