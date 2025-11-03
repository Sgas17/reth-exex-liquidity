// Simple NATS pub/sub test to verify connectivity

use async_nats;
use futures::StreamExt;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 Simple NATS Pub/Sub Test\n");

    // Connect
    let client = async_nats::connect("nats://localhost:4222").await?;
    println!("✅ Connected to NATS\n");

    // Subscribe
    let subject = "test.demo";
    let mut subscriber = client.subscribe(subject).await?;
    println!("📡 Subscribed to: {}\n", subject);

    // Publish in a background task
    let pub_client = client.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(500)).await;
        println!("📤 Publishing message...");
        pub_client
            .publish("test.demo", "Hello NATS!".into())
            .await
            .unwrap();
        println!("✅ Published\n");
    });

    // Receive
    println!("⏳ Waiting for message...");
    tokio::select! {
        msg = subscriber.next() => {
            if let Some(msg) = msg {
                println!("📬 Received: {}", String::from_utf8_lossy(&msg.payload));
                println!("   Subject: {}", msg.subject);
                println!("\n✅ Test PASSED!");
            }
        }
        _ = sleep(Duration::from_secs(3)) => {
            println!("\n❌ Test FAILED: No message received");
        }
    }

    Ok(())
}
