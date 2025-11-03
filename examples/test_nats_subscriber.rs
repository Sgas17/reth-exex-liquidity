// Test NATS Subscriber - Receives pool whitelist updates
//
// This is a simple test to verify NATS integration works.
// In production, the ExEx will subscribe to these messages.

use async_nats;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔌 Connecting to NATS at nats://localhost:4222");

    let client = async_nats::connect("nats://localhost:4222").await?;
    println!("✅ Connected to NATS");

    let subject = "whitelist.pools.ethereum.>";
    println!("📡 Subscribing to: {}", subject);

    let mut subscriber = client.subscribe(subject).await?;
    println!("✅ Subscribed! Waiting for messages...\n");

    let mut count = 0;
    while let Some(message) = subscriber.next().await {
        count += 1;

        println!("📬 Message #{} received:", count);
        println!("   Subject: {}", message.subject);
        println!("   Payload length: {} bytes", message.payload.len());

        // Try to parse as JSON
        match serde_json::from_slice::<serde_json::Value>(&message.payload) {
            Ok(json) => {
                println!("   JSON: {}", serde_json::to_string_pretty(&json)?);
            }
            Err(e) => {
                println!("   Raw: {}", String::from_utf8_lossy(&message.payload));
                println!("   (JSON parse error: {})", e);
            }
        }
        println!();

        // Exit after receiving 3 messages (for testing)
        if count >= 3 {
            println!("✅ Received {} messages, exiting", count);
            break;
        }
    }

    Ok(())
}
