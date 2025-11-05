// Reth ExEx Liquidity Library
//
// Exposes modules for reuse and testing

pub mod events;
pub mod nats_client;
pub mod pool_tracker;
pub mod socket;
pub mod types;

// Re-export commonly used items for testing
pub use events::{decode_log, DecodedEvent};
pub use pool_tracker::{PoolTracker, WhitelistUpdate, UNISWAP_V4_POOL_MANAGER};
pub use types::{ControlMessage, PoolIdentifier, PoolMetadata, PoolUpdate, Protocol, UpdateType};
