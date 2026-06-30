// Reth ExEx Liquidity Library
//
// Exposes modules for reuse and testing

pub mod balance_monitor;
pub mod balancer_storage;
pub mod events;
pub mod fluid_decoder;
pub mod nats_client;
pub mod pool_tracker;
pub mod shadow_apply;
pub mod shadow_arena;
pub mod socket;
pub mod swap_monitor;
pub mod transfers;
pub mod types;

// Re-export commonly used items for testing
pub use events::{
    decode_log, fluid_log_operate_pool, is_fluid_log_operate_for_pool, DecodedEvent, EKUBO_CORE,
};
pub use pool_tracker::{
    PoolTracker, WhitelistUpdate, FLUID_LIQUIDITY_LAYER, UNISWAP_V4_POOL_MANAGER,
};
pub use types::{
    ControlMessage, PoolIdentifier, PoolMetadata, PoolUpdate, Protocol, ReorgRange, UpdateType,
};
