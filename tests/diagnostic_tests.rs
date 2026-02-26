// Diagnostic tests to identify where events are being lost
//
// These tests simulate the exact flow from main.rs to help debug
// why events aren't being output for watched pools.

use alloy_primitives::{address, Address, Log, LogData, B256};
use alloy_sol_types::SolEvent;
use reth_exex_liquidity::{
    decode_log, DecodedEvent, PoolIdentifier, PoolMetadata, PoolTracker, Protocol, WhitelistUpdate,
    UNISWAP_V4_POOL_MANAGER,
};

#[derive(Debug)]
struct EventProcessingResult {
    passed_address_filter: bool,
    decoded_successfully: bool,
    passed_pool_filter: bool,
    should_output: bool,
}

/// Simulates the exact filtering logic from main.rs lines 397-436
fn simulate_event_processing(log: &Log, pool_tracker: &PoolTracker) -> EventProcessingResult {
    let log_address = log.address;

    // Step 1: Quick address filter (main.rs:401)
    let passed_address_filter = pool_tracker.is_tracked_address(&log_address);
    if !passed_address_filter {
        return EventProcessingResult {
            passed_address_filter: false,
            decoded_successfully: false,
            passed_pool_filter: false,
            should_output: false,
        };
    }

    // Step 2: Decode event (main.rs:406)
    let decoded_event = match decode_log(log) {
        Some(event) => event,
        None => {
            return EventProcessingResult {
                passed_address_filter: true,
                decoded_successfully: false,
                passed_pool_filter: false,
                should_output: false,
            };
        }
    };

    // Step 3: Check if we should process this specific event (main.rs:414)
    let passed_pool_filter = match &decoded_event {
        // V2/V3 events: check pool address
        DecodedEvent::V2Swap { pool, .. }
        | DecodedEvent::V2Mint { pool, .. }
        | DecodedEvent::V2Burn { pool, .. }
        | DecodedEvent::V3Swap { pool, .. }
        | DecodedEvent::V3Mint { pool, .. }
        | DecodedEvent::V3Burn { pool, .. } => pool_tracker.is_tracked_address(pool),

        // V4 events: check pool_id (NOT address!)
        DecodedEvent::V4Swap { pool_id, .. } | DecodedEvent::V4ModifyLiquidity { pool_id, .. } => {
            pool_tracker.is_tracked_pool_id(pool_id)
        }
    };

    EventProcessingResult {
        passed_address_filter: true,
        decoded_successfully: true,
        passed_pool_filter,
        should_output: passed_pool_filter,
    }
}

#[test]
fn test_diagnostic_v2_event_processing() {
    println!("\n=== DIAGNOSTIC: V2 Event Processing ===\n");

    // Setup: Create tracker with one V2 pool
    let mut tracker = PoolTracker::new();
    let pool_addr = address!("0000000000000000000000000000000000000001");

    let pool_metadata = PoolMetadata {
        pool_id: PoolIdentifier::Address(pool_addr),
        token0: Address::ZERO,
        token1: Address::ZERO,
        protocol: Protocol::UniswapV2,
        factory: Address::ZERO,
        tick_spacing: None,
        fee: None,
    };

    tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

    println!("✓ Added V2 pool to whitelist: {:?}", pool_addr);
    println!("  Tracker stats: {:?}", tracker.stats());
    println!("  Is tracked: {}", tracker.is_tracked_address(&pool_addr));

    // Create V2 Swap event
    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );
    }

    let log = Log {
        address: pool_addr,
        data: LogData::new_unchecked(
            vec![
                Swap::SIGNATURE_HASH,
                B256::ZERO, // sender
                B256::ZERO, // to
            ],
            vec![0u8; 160].into(),
        ),
    };

    println!("\n✓ Created V2 Swap event log");
    println!("  Log address: {:?}", log.address);
    println!("  Event signature: {:?}", Swap::SIGNATURE_HASH);

    // Process the event
    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results ===");
    println!("  Address filter: {}", result.passed_address_filter);
    println!("  Decoded: {}", result.decoded_successfully);
    println!("  Pool filter: {}", result.passed_pool_filter);
    println!("  Should output: {}", result.should_output);

    assert!(
        result.passed_address_filter,
        "❌ FAILED: Address filter should pass"
    );
    assert!(
        result.decoded_successfully,
        "❌ FAILED: Event should decode"
    );
    assert!(
        result.passed_pool_filter,
        "❌ FAILED: Pool filter should pass"
    );
    assert!(result.should_output, "❌ FAILED: Event should be output");

    println!("\n✅ SUCCESS: V2 event would be output correctly\n");
}

#[test]
fn test_diagnostic_v2_event_wrong_pool() {
    println!("\n=== DIAGNOSTIC: V2 Event from Untracked Pool ===\n");

    let mut tracker = PoolTracker::new();
    let tracked_pool = address!("0000000000000000000000000000000000000001");
    let untracked_pool = address!("0000000000000000000000000000000000000002");

    let pool_metadata = PoolMetadata {
        pool_id: PoolIdentifier::Address(tracked_pool),
        token0: Address::ZERO,
        token1: Address::ZERO,
        protocol: Protocol::UniswapV2,
        factory: Address::ZERO,
        tick_spacing: None,
        fee: None,
    };

    tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

    println!("✓ Tracking pool: {:?}", tracked_pool);
    println!("✓ Event from pool: {:?}", untracked_pool);

    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );
    }

    let log = Log {
        address: untracked_pool, // Different pool!
        data: LogData::new_unchecked(
            vec![Swap::SIGNATURE_HASH, B256::ZERO, B256::ZERO],
            vec![0u8; 160].into(),
        ),
    };

    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results ===");
    println!("  Address filter: {}", result.passed_address_filter);
    println!("  Decoded: {}", result.decoded_successfully);
    println!("  Pool filter: {}", result.passed_pool_filter);
    println!("  Should output: {}", result.should_output);

    assert!(
        !result.passed_address_filter,
        "Address filter should FAIL for untracked pool"
    );
    assert!(!result.should_output, "Event should NOT be output");

    println!("\n✅ SUCCESS: Untracked pool correctly filtered out\n");
}

#[test]
fn test_diagnostic_v3_event_processing() {
    println!("\n=== DIAGNOSTIC: V3 Event Processing ===\n");

    let mut tracker = PoolTracker::new();
    let pool_addr = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"); // USDC/WETH

    let pool_metadata = PoolMetadata {
        pool_id: PoolIdentifier::Address(pool_addr),
        token0: Address::ZERO,
        token1: Address::ZERO,
        protocol: Protocol::UniswapV3,
        factory: Address::ZERO,
        tick_spacing: Some(60),
        fee: Some(3000),
    };

    tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

    println!("✓ Added V3 pool to whitelist: {:?}", pool_addr);

    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            address indexed recipient,
            int256 amount0,
            int256 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );
    }

    let log = Log {
        address: pool_addr,
        data: LogData::new_unchecked(
            vec![Swap::SIGNATURE_HASH, B256::ZERO, B256::ZERO],
            vec![0u8; 224].into(),
        ),
    };

    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results ===");
    println!("  Address filter: {}", result.passed_address_filter);
    println!("  Decoded: {}", result.decoded_successfully);
    println!("  Pool filter: {}", result.passed_pool_filter);
    println!("  Should output: {}", result.should_output);

    assert!(result.should_output, "❌ FAILED: V3 event should be output");

    println!("\n✅ SUCCESS: V3 event would be output correctly\n");
}

#[test]
fn test_diagnostic_v4_event_processing() {
    println!("\n=== DIAGNOSTIC: V4 Event Processing (Two-Stage Filter) ===\n");

    let mut tracker = PoolTracker::new();
    let pool_id = [1u8; 32];

    let pool_metadata = PoolMetadata {
        pool_id: PoolIdentifier::PoolId(pool_id),
        token0: Address::ZERO,
        token1: Address::ZERO,
        protocol: Protocol::UniswapV4,
        factory: UNISWAP_V4_POOL_MANAGER,
        tick_spacing: Some(60),
        fee: Some(3000),
    };

    tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

    println!("✓ Added V4 pool to whitelist");
    println!("  Pool ID: {:?}", hex::encode(pool_id));
    println!("  PoolManager: {:?}", UNISWAP_V4_POOL_MANAGER);
    println!(
        "  PoolManager tracked: {}",
        tracker.is_tracked_address(&UNISWAP_V4_POOL_MANAGER)
    );
    println!(
        "  Pool ID tracked: {}",
        tracker.is_tracked_pool_id(&pool_id)
    );

    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            bytes32 indexed poolId,
            address indexed sender,
            int128 amount0,
            int128 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );
    }

    let log = Log {
        address: UNISWAP_V4_POOL_MANAGER,
        data: LogData::new_unchecked(
            vec![Swap::SIGNATURE_HASH, B256::from(pool_id), B256::ZERO],
            vec![0u8; 224].into(),
        ),
    };

    println!("\n✓ Created V4 Swap event log");
    println!("  Log address: {:?}", log.address);

    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results ===");
    println!(
        "  Stage 1 - Address filter (PoolManager): {}",
        result.passed_address_filter
    );
    println!("  Stage 2 - Event decoded: {}", result.decoded_successfully);
    println!("  Stage 3 - Pool ID filter: {}", result.passed_pool_filter);
    println!("  Should output: {}", result.should_output);

    assert!(
        result.passed_address_filter,
        "❌ FAILED: PoolManager address should be tracked"
    );
    assert!(
        result.decoded_successfully,
        "❌ FAILED: V4 event should decode"
    );
    assert!(
        result.passed_pool_filter,
        "❌ FAILED: Pool ID should be tracked"
    );
    assert!(result.should_output, "❌ FAILED: V4 event should be output");

    println!("\n✅ SUCCESS: V4 event would be output correctly\n");
}

#[test]
fn test_diagnostic_v4_wrong_pool_id() {
    println!("\n=== DIAGNOSTIC: V4 Event from Untracked Pool ID ===\n");

    let mut tracker = PoolTracker::new();
    let tracked_pool_id = [1u8; 32];
    let untracked_pool_id = [2u8; 32];

    let pool_metadata = PoolMetadata {
        pool_id: PoolIdentifier::PoolId(tracked_pool_id),
        token0: Address::ZERO,
        token1: Address::ZERO,
        protocol: Protocol::UniswapV4,
        factory: UNISWAP_V4_POOL_MANAGER,
        tick_spacing: Some(60),
        fee: Some(3000),
    };

    tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

    println!("✓ Tracking pool ID: {:?}", hex::encode(tracked_pool_id));
    println!("✓ Event from pool ID: {:?}", hex::encode(untracked_pool_id));
    println!(
        "  PoolManager tracked: {}",
        tracker.is_tracked_address(&UNISWAP_V4_POOL_MANAGER)
    );

    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            bytes32 indexed poolId,
            address indexed sender,
            int128 amount0,
            int128 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );
    }

    let log = Log {
        address: UNISWAP_V4_POOL_MANAGER,
        data: LogData::new_unchecked(
            vec![
                Swap::SIGNATURE_HASH,
                B256::from(untracked_pool_id), // Different pool ID!
                B256::ZERO,
            ],
            vec![0u8; 224].into(),
        ),
    };

    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results ===");
    println!(
        "  Stage 1 - Address filter (PoolManager): {}",
        result.passed_address_filter
    );
    println!("  Stage 2 - Event decoded: {}", result.decoded_successfully);
    println!("  Stage 3 - Pool ID filter: {}", result.passed_pool_filter);
    println!("  Should output: {}", result.should_output);

    assert!(
        result.passed_address_filter,
        "PoolManager address should pass (stage 1)"
    );
    assert!(result.decoded_successfully, "Event should decode (stage 2)");
    assert!(
        !result.passed_pool_filter,
        "Untracked pool ID should FAIL (stage 3)"
    );
    assert!(!result.should_output, "Event should NOT be output");

    println!("\n✅ SUCCESS: Untracked V4 pool ID correctly filtered out\n");
}

#[test]
fn test_diagnostic_empty_whitelist() {
    println!("\n=== DIAGNOSTIC: Empty Whitelist (No Pools Tracked) ===\n");

    let tracker = PoolTracker::new();

    println!("✓ Created empty tracker");
    println!("  Tracker stats: {:?}", tracker.stats());

    let pool_addr = address!("0000000000000000000000000000000000000001");

    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );
    }

    let log = Log {
        address: pool_addr,
        data: LogData::new_unchecked(
            vec![Swap::SIGNATURE_HASH, B256::ZERO, B256::ZERO],
            vec![0u8; 160].into(),
        ),
    };

    println!("✓ Created event from pool: {:?}", pool_addr);

    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results ===");
    println!("  Address filter: {}", result.passed_address_filter);
    println!("  Decoded: {}", result.decoded_successfully);
    println!("  Pool filter: {}", result.passed_pool_filter);
    println!("  Should output: {}", result.should_output);

    assert!(
        !result.passed_address_filter,
        "Address filter should FAIL with empty whitelist"
    );
    assert!(!result.should_output, "No events should be output");

    println!("\n✅ SUCCESS: Empty whitelist correctly filters all events\n");
    println!("⚠️  NOTE: If your whitelist is empty, NO events will be output!");
    println!("          Check that NATS updates are being received and applied.\n");
}

#[test]
fn test_diagnostic_whitelist_not_applied() {
    println!("\n=== DIAGNOSTIC: Pending Whitelist Update Not Applied ===\n");

    let mut tracker = PoolTracker::new();
    let pool_addr = address!("0000000000000000000000000000000000000001");

    let pool_metadata = PoolMetadata {
        pool_id: PoolIdentifier::Address(pool_addr),
        token0: Address::ZERO,
        token1: Address::ZERO,
        protocol: Protocol::UniswapV2,
        factory: Address::ZERO,
        tick_spacing: None,
        fee: None,
    };

    // Begin block BEFORE queuing update
    tracker.begin_block();

    // Queue update during block
    tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

    println!("✓ Queued whitelist update during block processing");
    println!("  Has pending updates: {}", tracker.has_pending_updates());
    println!("  Pool tracked: {}", tracker.is_tracked_address(&pool_addr));

    use alloy_sol_types::sol;
    sol! {
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );
    }

    let log = Log {
        address: pool_addr,
        data: LogData::new_unchecked(
            vec![Swap::SIGNATURE_HASH, B256::ZERO, B256::ZERO],
            vec![0u8; 160].into(),
        ),
    };

    let result = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results (Update Still Pending) ===");
    println!("  Address filter: {}", result.passed_address_filter);
    println!("  Should output: {}", result.should_output);

    assert!(
        !result.should_output,
        "Event should NOT be output while update is pending"
    );

    // Now end the block
    tracker.end_block();

    println!("\n✓ Called end_block() - updates applied");
    println!("  Has pending updates: {}", tracker.has_pending_updates());
    println!("  Pool tracked: {}", tracker.is_tracked_address(&pool_addr));

    let result_after = simulate_event_processing(&log, &tracker);

    println!("\n=== Processing Results (After Update Applied) ===");
    println!("  Address filter: {}", result_after.passed_address_filter);
    println!("  Should output: {}", result_after.should_output);

    assert!(
        result_after.should_output,
        "Event SHOULD be output after update applied"
    );

    println!("\n✅ SUCCESS: Whitelist updates correctly synchronized with blocks\n");
    println!("⚠️  NOTE: If events arrive before whitelist update is applied,");
    println!("          they will be filtered out!\n");
}
