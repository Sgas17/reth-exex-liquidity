use super::db::TransferDb;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{info, warn};

/// Spawn aggregation task — runs every 5 minutes.
pub fn spawn_aggregator(db: Arc<TransferDb>) {
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            match db.run_aggregation().await {
                Ok(()) => info!("Aggregation completed"),
                Err(e) => warn!("Aggregation failed: {}", e),
            }
        }
    });
}

/// Spawn cleanup task — runs every 24 hours.
pub fn spawn_cleanup(db: Arc<TransferDb>) {
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(86400));
        loop {
            tick.tick().await;
            match db.cleanup_old_transfers().await {
                Ok(deleted) => info!("Cleanup: deleted {} old transfers", deleted),
                Err(e) => warn!("Cleanup failed: {}", e),
            }
        }
    });
}
