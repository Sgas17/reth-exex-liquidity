// Test NATS Publisher
//
// Publishes a mock whitelist message to test ExEx NATS integration

use reth_exex_liquidity::nats_client::{PoolData, WhitelistPoolMessage};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Test NATS Publisher");

    // Connect to NATS
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());
    println!("Connecting to NATS at {}", nats_url);

    let client = async_nats::connect(&nats_url).await?;
    println!("✅ Connected to NATS");

    // Create a test whitelist message
    let test_pools = vec![
        // Uniswap V2 USDC/WETH
        PoolData {
            address: "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".to_string(),
            token0: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(), // USDC
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(), // WETH
            protocol: "v2".to_string(),
            factory: "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f".to_string(),
            tick_spacing: None,
            fee: None,
        },
        // Uniswap V3 USDC/WETH 0.05%
        PoolData {
            address: "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
            token0: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(), // USDC
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(), // WETH
            protocol: "v3".to_string(),
            factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
            tick_spacing: Some(10),
            fee: Some(500),
        },
        // Uniswap V3 WBTC/WETH 0.3%
        PoolData {
            address: "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed".to_string(),
            token0: "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599".to_string(), // WBTC
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(), // WETH
            protocol: "v3".to_string(),
            factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
            tick_spacing: Some(60),
            fee: Some(3000),
        },
        // Uniswap V4 pool (mock - using bytes32 pool ID)
        PoolData {
            address: "0x0000000000000000000000000000000000000000000000000000000000000001"
                .to_string(),
            token0: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(), // USDC
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(), // WETH
            protocol: "v4".to_string(),
            factory: "0x0000000000000000000000000000000000000000".to_string(),
            tick_spacing: Some(60),
            fee: Some(3000),
        },
    ];

    let message = WhitelistPoolMessage {
        chain: "ethereum".to_string(),
        pools: test_pools,
        generated_at: chrono::Utc::now().to_rfc3339(),
        metadata: None,
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
        message.pools.iter().filter(|p| p.protocol == "v2").count()
    );
    println!(
        "  - {} V3 pools",
        message.pools.iter().filter(|p| p.protocol == "v3").count()
    );
    println!(
        "  - {} V4 pools",
        message.pools.iter().filter(|p| p.protocol == "v4").count()
    );

    println!("\n💡 The ExEx should receive this message if it's running with:");
    println!("   NATS_URL={}", nats_url);
    println!("   CHAIN=ethereum");

    Ok(())
}
