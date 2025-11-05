// Integration tests for Liquidity ExEx
//
// These tests verify the complete event processing pipeline to help debug
// why events might not be output for watched pools.

use alloy_primitives::{address, Address, Log, LogData, B256, U256};
use alloy_sol_types::SolEvent;
use reth_exex_liquidity::{
    events::{decode_log, DecodedEvent},
    pool_tracker::{PoolTracker, WhitelistUpdate, UNISWAP_V4_POOL_MANAGER},
    types::{ControlMessage, PoolIdentifier, PoolMetadata, PoolUpdate, Protocol, UpdateType},
};

mod event_filtering {
    use super::*;

    fn create_v2_pool_metadata(addr: Address) -> PoolMetadata {
        PoolMetadata {
            pool_id: PoolIdentifier::Address(addr),
            token0: Address::ZERO,
            token1: Address::ZERO,
            protocol: Protocol::UniswapV2,
            factory: Address::ZERO,
            tick_spacing: None,
            fee: None,
        }
    }

    fn create_v3_pool_metadata(addr: Address) -> PoolMetadata {
        PoolMetadata {
            pool_id: PoolIdentifier::Address(addr),
            token0: Address::ZERO,
            token1: Address::ZERO,
            protocol: Protocol::UniswapV3,
            factory: Address::ZERO,
            tick_spacing: Some(60),
            fee: Some(3000),
        }
    }

    fn create_v4_pool_metadata(pool_id: [u8; 32]) -> PoolMetadata {
        PoolMetadata {
            pool_id: PoolIdentifier::PoolId(pool_id),
            token0: Address::ZERO,
            token1: Address::ZERO,
            protocol: Protocol::UniswapV4,
            factory: UNISWAP_V4_POOL_MANAGER,
            tick_spacing: Some(60),
            fee: Some(3000),
        }
    }

    #[test]
    fn test_v2_pool_address_filtering() {
        let mut tracker = PoolTracker::new();

        // Add V2 pool to whitelist
        let pool_addr = address!("0000000000000000000000000000000000000001");
        let pool_metadata = create_v2_pool_metadata(pool_addr);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

        // Verify pool is tracked
        assert!(
            tracker.is_tracked_address(&pool_addr),
            "Pool address should be tracked"
        );
        assert_eq!(tracker.stats().v2_pools, 1);
        assert_eq!(tracker.stats().total_pools, 1);

        // Test that a different address is NOT tracked
        let other_addr = address!("0000000000000000000000000000000000000002");
        assert!(
            !tracker.is_tracked_address(&other_addr),
            "Different address should not be tracked"
        );
    }

    #[test]
    fn test_v3_pool_address_filtering() {
        let mut tracker = PoolTracker::new();

        // Add V3 pool to whitelist
        let pool_addr = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"); // USDC/WETH
        let pool_metadata = create_v3_pool_metadata(pool_addr);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

        // Verify pool is tracked
        assert!(
            tracker.is_tracked_address(&pool_addr),
            "V3 pool address should be tracked"
        );
        assert_eq!(tracker.stats().v3_pools, 1);
        assert_eq!(tracker.stats().total_pools, 1);
    }

    #[test]
    fn test_v4_pool_id_filtering() {
        let mut tracker = PoolTracker::new();

        // Add V4 pool to whitelist
        let pool_id = [1u8; 32];
        let pool_metadata = create_v4_pool_metadata(pool_id);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

        // Verify pool_id is tracked
        assert!(
            tracker.is_tracked_pool_id(&pool_id),
            "V4 pool_id should be tracked"
        );

        // Verify PoolManager address is also tracked (needed to receive events)
        assert!(
            tracker.is_tracked_address(&UNISWAP_V4_POOL_MANAGER),
            "PoolManager address should be tracked for V4 pools"
        );

        assert_eq!(tracker.stats().v4_pools, 1);
        assert_eq!(tracker.stats().total_pools, 1);

        // Test that a different pool_id is NOT tracked
        let other_pool_id = [2u8; 32];
        assert!(
            !tracker.is_tracked_pool_id(&other_pool_id),
            "Different pool_id should not be tracked"
        );
    }

    #[test]
    fn test_mixed_protocol_filtering() {
        let mut tracker = PoolTracker::new();

        // Add pools from all protocols
        let v2_addr = address!("0000000000000000000000000000000000000001");
        let v3_addr = address!("0000000000000000000000000000000000000002");
        let v4_pool_id = [1u8; 32];

        let pools = vec![
            create_v2_pool_metadata(v2_addr),
            create_v3_pool_metadata(v3_addr),
            create_v4_pool_metadata(v4_pool_id),
        ];

        tracker.queue_update(WhitelistUpdate::Add(pools));

        // Verify all pools are tracked
        assert!(tracker.is_tracked_address(&v2_addr));
        assert!(tracker.is_tracked_address(&v3_addr));
        assert!(tracker.is_tracked_pool_id(&v4_pool_id));
        assert!(tracker.is_tracked_address(&UNISWAP_V4_POOL_MANAGER));

        // Verify stats
        let stats = tracker.stats();
        assert_eq!(stats.v2_pools, 1, "Should track 1 V2 pool");
        assert_eq!(stats.v3_pools, 1, "Should track 1 V3 pool");
        assert_eq!(stats.v4_pools, 1, "Should track 1 V4 pool");
        assert_eq!(stats.total_pools, 3, "Should track 3 pools total");
    }

    #[test]
    fn test_remove_pool_from_whitelist() {
        let mut tracker = PoolTracker::new();

        // Add pool
        let pool_addr = address!("0000000000000000000000000000000000000001");
        let pool_metadata = create_v2_pool_metadata(pool_addr);

        tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));
        assert!(tracker.is_tracked_address(&pool_addr));

        // Remove pool
        tracker.queue_update(WhitelistUpdate::Remove(vec![PoolIdentifier::Address(
            pool_addr,
        )]));

        // Verify pool is no longer tracked
        assert!(
            !tracker.is_tracked_address(&pool_addr),
            "Pool should be removed from tracking"
        );
        assert_eq!(tracker.stats().total_pools, 0);
    }

    #[test]
    fn test_block_synchronized_whitelist_updates() {
        let mut tracker = PoolTracker::new();

        let pool_addr = address!("0000000000000000000000000000000000000001");
        let pool_metadata = create_v2_pool_metadata(pool_addr);

        // Begin block processing
        tracker.begin_block();

        // Queue update during block
        tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

        // Update should be pending, not applied yet
        assert!(
            tracker.has_pending_updates(),
            "Should have pending updates during block"
        );
        assert!(
            !tracker.is_tracked_address(&pool_addr),
            "Pool should not be tracked yet (update pending)"
        );

        // End block - update should be applied
        tracker.end_block();

        // Now pool should be tracked
        assert!(
            !tracker.has_pending_updates(),
            "Should not have pending updates after block"
        );
        assert!(
            tracker.is_tracked_address(&pool_addr),
            "Pool should be tracked after end_block"
        );
    }
}

mod event_decoding_and_filtering {
    use super::*;

    // Helper to create V2 Swap event
    fn create_v2_swap_log(pool_addr: Address) -> Log {
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

        Log {
            address: pool_addr,
            data: LogData::new_unchecked(
                vec![
                    Swap::SIGNATURE_HASH,
                    B256::ZERO, // sender
                    B256::ZERO, // to
                ],
                vec![0u8; 160].into(), // 5 uint256 values
            ),
        }
    }

    // Helper to create V3 Swap event
    fn create_v3_swap_log(pool_addr: Address) -> Log {
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

        Log {
            address: pool_addr,
            data: LogData::new_unchecked(
                vec![
                    Swap::SIGNATURE_HASH,
                    B256::ZERO, // sender
                    B256::ZERO, // recipient
                ],
                vec![0u8; 224].into(),
            ),
        }
    }

    // Helper to create V4 Swap event
    fn create_v4_swap_log(pool_id: [u8; 32]) -> Log {
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

        Log {
            address: UNISWAP_V4_POOL_MANAGER,
            data: LogData::new_unchecked(
                vec![
                    Swap::SIGNATURE_HASH,
                    B256::from(pool_id),
                    B256::ZERO, // sender
                ],
                vec![0u8; 224].into(),
            ),
        }
    }

    #[test]
    fn test_decode_and_filter_v2_event() {
        // Create pool tracker with one V2 pool
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

        // Create V2 Swap log from tracked pool
        let log = create_v2_swap_log(pool_addr);

        // Step 1: Address filter
        assert!(
            tracker.is_tracked_address(&log.address),
            "Pool address should pass address filter"
        );

        // Step 2: Decode event
        let decoded = decode_log(&log);
        assert!(decoded.is_some(), "Event should decode successfully");

        // Step 3: Check if we should process this event
        let decoded_event = decoded.unwrap();
        match &decoded_event {
            DecodedEvent::V2Swap { pool, .. } => {
                assert!(
                    tracker.is_tracked_address(pool),
                    "Decoded pool address should be tracked"
                );
            }
            _ => panic!("Expected V2Swap event"),
        }
    }

    #[test]
    fn test_decode_and_filter_v3_event() {
        let mut tracker = PoolTracker::new();
        let pool_addr = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");

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

        let log = create_v3_swap_log(pool_addr);

        // Address filter should pass
        assert!(tracker.is_tracked_address(&log.address));

        // Decode and verify
        let decoded = decode_log(&log).expect("V3 event should decode");

        match &decoded {
            DecodedEvent::V3Swap { pool, .. } => {
                assert!(tracker.is_tracked_address(pool));
            }
            _ => panic!("Expected V3Swap event"),
        }
    }

    #[test]
    fn test_decode_and_filter_v4_event() {
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

        let log = create_v4_swap_log(pool_id);

        // Step 1: Address filter - should match PoolManager
        assert_eq!(log.address, UNISWAP_V4_POOL_MANAGER);
        assert!(
            tracker.is_tracked_address(&log.address),
            "PoolManager address should be tracked"
        );

        // Step 2: Decode event
        let decoded = decode_log(&log).expect("V4 event should decode");

        // Step 3: Check pool_id (second-stage filter for V4)
        match &decoded {
            DecodedEvent::V4Swap {
                pool_id: event_pool_id,
                ..
            } => {
                assert!(
                    tracker.is_tracked_pool_id(event_pool_id),
                    "Pool ID from event should be tracked"
                );
            }
            _ => panic!("Expected V4Swap event"),
        }
    }

    #[test]
    fn test_v4_event_from_untracked_pool_id() {
        let mut tracker = PoolTracker::new();

        // Add one V4 pool
        let tracked_pool_id = [1u8; 32];
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

        // Create event from a DIFFERENT pool_id
        let untracked_pool_id = [2u8; 32];
        let log = create_v4_swap_log(untracked_pool_id);

        // Step 1: Address filter passes (PoolManager is tracked)
        assert!(tracker.is_tracked_address(&log.address));

        // Step 2: Decode event
        let decoded = decode_log(&log).expect("Event should decode");

        // Step 3: Pool ID filter should FAIL
        match &decoded {
            DecodedEvent::V4Swap {
                pool_id: event_pool_id,
                ..
            } => {
                assert!(
                    !tracker.is_tracked_pool_id(event_pool_id),
                    "Untracked pool_id should not pass filter"
                );
            }
            _ => panic!("Expected V4Swap event"),
        }
    }

    #[test]
    fn test_event_from_untracked_pool_address() {
        let mut tracker = PoolTracker::new();

        // Add one pool to tracker
        let tracked_addr = address!("0000000000000000000000000000000000000001");
        let pool_metadata = PoolMetadata {
            pool_id: PoolIdentifier::Address(tracked_addr),
            token0: Address::ZERO,
            token1: Address::ZERO,
            protocol: Protocol::UniswapV2,
            factory: Address::ZERO,
            tick_spacing: None,
            fee: None,
        };

        tracker.queue_update(WhitelistUpdate::Add(vec![pool_metadata]));

        // Create event from DIFFERENT pool
        let untracked_addr = address!("0000000000000000000000000000000000000002");
        let log = create_v2_swap_log(untracked_addr);

        // Address filter should FAIL
        assert!(
            !tracker.is_tracked_address(&log.address),
            "Untracked pool address should not pass filter"
        );

        // This event should be filtered out early and never reach decoding
    }
}

mod message_creation {
    use super::*;

    #[test]
    fn test_create_v2_swap_message() {
        // Simulate creating a PoolUpdateMessage from a V2 Swap event
        let pool_addr = address!("0000000000000000000000000000000000000001");

        let event = DecodedEvent::V2Swap {
            pool: pool_addr,
            amount0_in: U256::from(1000),
            amount1_in: U256::from(0),
            amount0_out: U256::from(0),
            amount1_out: U256::from(500),
        };

        // In main.rs, this would be created by create_pool_update()
        // We'll manually create it here to test the structure
        let message = reth_exex_liquidity::types::PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(pool_addr),
            protocol: Protocol::UniswapV2,
            update_type: UpdateType::Swap,
            block_number: 12345,
            block_timestamp: 1234567890,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V2Reserves {
                reserve0: U256::from(1000),
                reserve1: U256::from(500),
            },
        };

        // Verify message structure
        assert_eq!(message.block_number, 12345);
        assert!(!message.is_revert);
        assert_eq!(message.protocol, Protocol::UniswapV2);
        assert_eq!(message.update_type, UpdateType::Swap);

        match message.update {
            PoolUpdate::V2Reserves { reserve0, reserve1 } => {
                assert_eq!(reserve0, U256::from(1000));
                assert_eq!(reserve1, U256::from(500));
            }
            _ => panic!("Expected V2Reserves"),
        }
    }

    #[test]
    fn test_create_v3_swap_message() {
        let pool_addr = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");

        let message = reth_exex_liquidity::types::PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(pool_addr),
            protocol: Protocol::UniswapV3,
            update_type: UpdateType::Swap,
            block_number: 12345,
            block_timestamp: 1234567890,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V3Swap {
                sqrt_price_x96: U256::from(1u128 << 96),
                liquidity: 1000000,
                tick: 200000,
            },
        };

        assert_eq!(message.protocol, Protocol::UniswapV3);
        match message.update {
            PoolUpdate::V3Swap {
                sqrt_price_x96,
                liquidity,
                tick,
            } => {
                assert!(sqrt_price_x96 > U256::ZERO);
                assert_eq!(liquidity, 1000000);
                assert_eq!(tick, 200000);
            }
            _ => panic!("Expected V3Swap"),
        }
    }

    #[test]
    fn test_create_v4_swap_message() {
        let pool_id = [1u8; 32];

        let message = reth_exex_liquidity::types::PoolUpdateMessage {
            pool_id: PoolIdentifier::PoolId(pool_id),
            protocol: Protocol::UniswapV4,
            update_type: UpdateType::Swap,
            block_number: 12345,
            block_timestamp: 1234567890,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V4Swap {
                sqrt_price_x96: U256::from(1u128 << 96),
                liquidity: 1000000,
                tick: 200000,
            },
        };

        assert_eq!(message.protocol, Protocol::UniswapV4);
        match &message.pool_id {
            PoolIdentifier::PoolId(id) => {
                assert_eq!(id, &pool_id);
            }
            _ => panic!("Expected PoolId"),
        }
    }
}

mod block_boundaries {
    use super::*;

    #[test]
    fn test_block_boundary_messages() {
        // Test BeginBlock message creation
        let begin_block = ControlMessage::BeginBlock {
            block_number: 12345,
            block_timestamp: 1234567890,
            is_revert: false,
        };

        match begin_block {
            ControlMessage::BeginBlock {
                block_number,
                block_timestamp,
                is_revert,
            } => {
                assert_eq!(block_number, 12345);
                assert_eq!(block_timestamp, 1234567890);
                assert!(!is_revert);
            }
            _ => panic!("Expected BeginBlock"),
        }

        // Test EndBlock message creation
        let end_block = ControlMessage::EndBlock {
            block_number: 12345,
            num_updates: 5,
        };

        match end_block {
            ControlMessage::EndBlock {
                block_number,
                num_updates,
            } => {
                assert_eq!(block_number, 12345);
                assert_eq!(num_updates, 5);
            }
            _ => panic!("Expected EndBlock"),
        }
    }

    #[test]
    fn test_revert_block_message() {
        // Test BeginBlock with revert flag
        let begin_block_revert = ControlMessage::BeginBlock {
            block_number: 12345,
            block_timestamp: 1234567890,
            is_revert: true,
        };

        match begin_block_revert {
            ControlMessage::BeginBlock { is_revert, .. } => {
                assert!(is_revert, "Revert flag should be set");
            }
            _ => panic!("Expected BeginBlock"),
        }
    }
}

mod serialization {
    use super::*;

    #[test]
    fn test_pool_update_message_serialization() {
        let message = reth_exex_liquidity::types::PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(Address::ZERO),
            protocol: Protocol::UniswapV2,
            update_type: UpdateType::Swap,
            block_number: 12345,
            block_timestamp: 1234567890,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V2Reserves {
                reserve0: U256::from(1000),
                reserve1: U256::from(500),
            },
        };

        // Test JSON serialization
        let json = serde_json::to_string(&message).expect("Should serialize to JSON");
        assert!(json.contains("\"protocol\":\"UniswapV2\""));

        // Test bincode serialization (used by socket)
        let encoded = bincode::serialize(&message).expect("Should serialize with bincode");
        let decoded: reth_exex_liquidity::types::PoolUpdateMessage =
            bincode::deserialize(&encoded).expect("Should deserialize");

        assert_eq!(decoded.block_number, message.block_number);
        assert_eq!(decoded.protocol, message.protocol);
    }

    #[test]
    fn test_control_message_serialization() {
        let msg = ControlMessage::BeginBlock {
            block_number: 12345,
            block_timestamp: 1234567890,
            is_revert: false,
        };

        let encoded = bincode::serialize(&msg).expect("Should serialize");
        let decoded: ControlMessage = bincode::deserialize(&encoded).expect("Should deserialize");

        match decoded {
            ControlMessage::BeginBlock { block_number, .. } => {
                assert_eq!(block_number, 12345);
            }
            _ => panic!("Expected BeginBlock"),
        }
    }
}
