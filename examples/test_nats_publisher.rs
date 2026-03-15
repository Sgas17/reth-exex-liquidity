// Test NATS Publisher
//
// Publishes a mock whitelist message to test ExEx NATS integration

use reth_exex_liquidity::nats_client::WhitelistPoolMessage;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Test NATS Publisher");

    // Connect to NATS
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
    println!("Connecting to NATS at {}", nats_url);

    let client = async_nats::connect(&nats_url).await?;
    println!("✅ Connected to NATS");

    // Create a test minimal whitelist message (addresses + parallel protocols).
    let pools = vec![
        // Uniswap V2 USDC/WETH
        "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".to_string(),
        // Uniswap V3 USDC/WETH 0.05%
        "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
        // Uniswap V3 WBTC/WETH 0.3%
        "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed".to_string(),
        // Uniswap V4 pool (mock - bytes32 pool ID)
        "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
    ];
    let protocols = vec![
        "v2".to_string(),
        "v3".to_string(),
        "v3".to_string(),
        "v4".to_string(),
    ];

    let message = WhitelistPoolMessage {
        message_type: "full".to_string(),
        pools,
        protocols,
        chain: "ethereum".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        snapshot_id: None,
    };

    // Serialize to JSON
    let payload = serde_json::to_vec(&message)?;

    // Publish to NATS
    let subject = "whitelist.pools.ethereum.all";
    println!("\n📤 Publishing whitelist to subject: {}", subject);
    println!("   {} pools included", message.pools.len());

    client.publish(subject.to_string(), payload.into()).await?;

    println!("✅ Whitelist published successfully!");
    println!("\nMessage details:");
    println!("  Chain: {}", message.chain);
    println!("  Pools: {}", message.pools.len());
    println!(
        "  - {} V2 pools",
        message.protocols.iter().filter(|p| p.as_str() == "v2").count()
    );
    println!(
        "  - {} V3 pools",
        message.protocols.iter().filter(|p| p.as_str() == "v3").count()
    );
    println!(
        "  - {} V4 pools",
        message.protocols.iter().filter(|p| p.as_str() == "v4").count()
    );

    println!("\n💡 The ExEx should receive this message if it's running with:");
    println!("   NATS_URL={}", nats_url);
    println!("   CHAIN=ethereum");

    Ok(())
}
