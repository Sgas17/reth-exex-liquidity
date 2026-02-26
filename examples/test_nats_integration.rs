// Integrated NATS test for whitelist messages
// Tests both publishing and receiving pool whitelist updates

use async_nats;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WhitelistPoolMessage {
    pools: Vec<PoolData>,
    chain: String,
    timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PoolData {
    address: String,
    token0: String,
    token1: String,
    protocol: String,
    factory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tick_spacing: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fee: Option<u32>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 NATS Whitelist Integration Test\n");

    // Connect to NATS
    let client = async_nats::connect("nats://localhost:4222").await?;
    println!("✅ Connected to NATS\n");

    // Subscribe to whitelist updates
    let subject = "whitelist.pools.ethereum.>";
    let mut subscriber = client.subscribe(subject).await?;
    println!("📡 Subscribed to: {}\n", subject);

    // Create test message
    let message = WhitelistPoolMessage {
        pools: vec![PoolData {
            address: "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640".to_string(),
            token0: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            token1: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            protocol: "UniswapV3".to_string(),
            factory: "0x1F98431c8aD98523631AE4a59f267346ea31F984".to_string(),
            tick_spacing: Some(10),
            fee: Some(500),
        }],
        chain: "ethereum".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    // Publish in background task
    let pub_client = client.clone();
    let pub_message = message.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(500)).await;
        println!("📤 Publishing whitelist message...");
        let payload = serde_json::to_vec(&pub_message).unwrap();
        pub_client
            .publish("whitelist.pools.ethereum.all", payload.into())
            .await
            .unwrap();
        println!("✅ Published to: whitelist.pools.ethereum.all\n");
    });

    // Wait for message
    println!("⏳ Waiting for message...\n");
    tokio::select! {
        msg = subscriber.next() => {
            if let Some(msg) = msg {
                println!("📬 Received message!");
                println!("   Subject: {}", msg.subject);
                println!("   Payload size: {} bytes\n", msg.payload.len());

                // Parse message
                match serde_json::from_slice::<WhitelistPoolMessage>(&msg.payload) {
                    Ok(whitelist) => {
                        println!("✅ Successfully parsed whitelist message:");
                        println!("   Chain: {}", whitelist.chain);
                        println!("   Pools: {}", whitelist.pools.len());
                        for (i, pool) in whitelist.pools.iter().enumerate() {
                            println!("\n   Pool #{}:", i + 1);
                            println!("     Address: {}", pool.address);
                            println!("     Protocol: {}", pool.protocol);
                            if let Some(fee) = pool.fee {
                                println!("     Fee: {} bps", fee);
                            }
                        }
                        println!("\n✅ Test PASSED!");
                    }
                    Err(e) => {
                        println!("❌ Failed to parse message: {}", e);
                    }
                }
            }
        }
        _ = sleep(Duration::from_secs(3)) => {
            println!("❌ Test FAILED: No message received within timeout");
        }
    }

    Ok(())
}
