# Testing Guide for Reth ExEx Liquidity

This document describes the comprehensive test suite added to help debug why events might not be output for watched pools.

## Test Structure

### Integration Tests (`tests/integration_tests.rs`)

These tests verify the complete event processing pipeline:

#### 1. **Event Filtering Tests** (`event_filtering` module)
- `test_v2_pool_address_filtering` - Verifies V2 pools are tracked by address
- `test_v3_pool_address_filtering` - Verifies V3 pools are tracked by address
- `test_v4_pool_id_filtering` - Verifies V4 pools are tracked by pool_id AND PoolManager address
- `test_mixed_protocol_filtering` - Tests tracking multiple protocol versions simultaneously
- `test_remove_pool_from_whitelist` - Verifies pools can be removed from tracking
- `test_block_synchronized_whitelist_updates` - Tests that whitelist updates are synchronized with block boundaries

#### 2. **Event Decoding and Filtering Tests** (`event_decoding_and_filtering` module)
- `test_decode_and_filter_v2_event` - Full pipeline test for V2 events
- `test_decode_and_filter_v3_event` - Full pipeline test for V3 events
- `test_decode_and_filter_v4_event` - Two-stage filtering for V4 events (address then pool_id)
- `test_v4_event_from_untracked_pool_id` - Verifies V4 events from untracked pool_ids are filtered
- `test_event_from_untracked_pool_address` - Verifies events from untracked addresses are filtered

#### 3. **Message Creation Tests** (`message_creation` module)
- `test_create_v2_swap_message` - Verifies V2 message structure
- `test_create_v3_swap_message` - Verifies V3 message structure
- `test_create_v4_swap_message` - Verifies V4 message structure

#### 4. **Block Boundary Tests** (`block_boundaries` module)
- `test_block_boundary_messages` - Tests BeginBlock/EndBlock message creation
- `test_revert_block_message` - Tests revert flag in BeginBlock messages

#### 5. **Serialization Tests** (`serialization` module)
- `test_pool_update_message_serialization` - Tests JSON and bincode serialization
- `test_control_message_serialization` - Tests control message serialization

### Diagnostic Tests (`tests/diagnostic_tests.rs`)

These tests simulate the exact event processing flow from `main.rs` to help identify where events are being lost:

- `test_diagnostic_v2_event_processing` - Step-by-step V2 event processing with detailed output
- `test_diagnostic_v2_event_wrong_pool` - Shows how untracked V2 pools are filtered
- `test_diagnostic_v3_event_processing` - Step-by-step V3 event processing
- `test_diagnostic_v4_event_processing` - Two-stage V4 filtering with diagnostics
- `test_diagnostic_v4_wrong_pool_id` - Shows V4 pool_id filtering behavior
- `test_diagnostic_empty_whitelist` - **CRITICAL**: Shows what happens when whitelist is empty
- `test_diagnostic_whitelist_not_applied` - Shows timing issues with whitelist updates

## Running the Tests

```bash
# Run all tests
cargo test

# Run only integration tests
cargo test --test integration_tests

# Run only diagnostic tests with output
cargo test --test diagnostic_tests -- --nocapture

# Run a specific diagnostic test
cargo test --test diagnostic_tests test_diagnostic_v2_event_processing -- --nocapture
```

## Diagnostic Logging in main.rs

Enhanced logging has been added to help identify filtering issues:

### Event Filtering Logs (DEBUG level)

The `should_process_event` function now logs when events are filtered out:

```
DEBUG Filtered V2 event from untracked pool: 0x1234...
DEBUG Filtered V3 event from untracked pool: 0x5678...
DEBUG Filtered V4 event from untracked pool_id: abcd1234...
```

### Block Processing Logs (INFO level)

```
INFO Block 12345: processed 5 liquidity events
```

### Whitelist Status Warnings

Every 100 blocks, the ExEx logs whitelist statistics:

```
INFO Tracking: 10 pools (3 V2, 5 V3, 2 V4)
```

If the whitelist is empty:

```
WARN ⚠️  No pools in whitelist! Events will be filtered out.
WARN    Check that NATS whitelist updates are being received.
```

## Common Issues and Debugging Steps

### Issue 1: No events being output

**Symptoms**: ExEx runs but no events are sent to socket

**Debugging steps**:

1. **Check if whitelist is empty**:
   ```
   # Look for this warning in logs
   WARN ⚠️  No pools in whitelist!
   ```

2. **Enable DEBUG logging**:
   ```bash
   RUST_LOG=debug ./exex
   ```

3. **Look for filter logs**:
   ```
   DEBUG Filtered V2 event from untracked pool: ...
   ```

4. **Run diagnostic tests**:
   ```bash
   cargo test --test diagnostic_tests test_diagnostic_empty_whitelist -- --nocapture
   ```

**Common causes**:
- NATS server not running or not reachable
- No whitelist updates sent from dynamicWhitelist service
- Whitelist updates queued but not applied (block synchronization issue)

### Issue 2: Events seen but filtered out

**Symptoms**: Logs show events decoded but not output

**Debugging steps**:

1. **Check event source address**:
   - For V2/V3: Is the pool address in the whitelist?
   - For V4: Is the PoolManager address tracked? Is the pool_id in the whitelist?

2. **Run diagnostic test**:
   ```bash
   cargo test --test diagnostic_tests test_diagnostic_v4_event_processing -- --nocapture
   ```

3. **Verify pool_id vs address**:
   - V4 uses TWO-STAGE filtering:
     - Stage 1: PoolManager address filter (0x000000000004444c5dc75cb358380d2e3de08a90)
     - Stage 2: pool_id filter (from event data)

### Issue 3: Whitelist updates not applied

**Symptoms**: NATS messages received but pools still not tracked

**Debugging steps**:

1. **Check for pending updates**:
   ```
   INFO Queuing add: 5 pools
   ```

2. **Verify block synchronization**:
   - Updates are queued during block processing
   - Updates are applied AFTER block ends (between BeginBlock and next block)

3. **Run diagnostic test**:
   ```bash
   cargo test --test diagnostic_tests test_diagnostic_whitelist_not_applied -- --nocapture
   ```

### Issue 4: V4 events not working

**Symptoms**: V2/V3 events work but V4 events filtered out

**Debugging steps**:

1. **Verify PoolManager is tracked**:
   ```
   # Should see in logs:
   INFO 🔧 Added PoolManager address to tracked addresses for V4 events
   ```

2. **Check pool_id format**:
   - V4 pool_id is bytes32, not an address
   - Verify the pool_id in the whitelist matches the event data

3. **Run V4 diagnostic tests**:
   ```bash
   cargo test --test diagnostic_tests test_diagnostic_v4 -- --nocapture
   ```

## Event Processing Flow

Understanding the complete flow helps identify where filtering occurs:

```
┌─────────────────────────────────────────────────────────────┐
│ 1. Notification received (ChainCommitted)                   │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. For each block in notification                           │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Begin block (lock whitelist updates)                     │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. For each transaction receipt                             │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. For each log in receipt                                  │
│    ├─ Step 1: Address filter (99.9% filtered here)         │
│    ├─ Step 2: Decode event (only known event types)        │
│    ├─ Step 3: Pool/PoolID filter                           │
│    └─ Step 4: Create and send message                      │
└────────────────────────┬────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. End block (apply pending whitelist updates)              │
└─────────────────────────────────────────────────────────────┘
```

## Testing Checklist

Before assuming there's a bug, verify:

- [ ] NATS server is running and accessible
- [ ] Whitelist updates are being sent from dynamicWhitelist service
- [ ] At least one pool is in the whitelist (check stats logs)
- [ ] Pool addresses/pool_ids in whitelist match the events you expect
- [ ] For V4: PoolManager address is tracked
- [ ] DEBUG logging is enabled to see filtering decisions
- [ ] Integration tests pass: `cargo test --test integration_tests`
- [ ] Diagnostic tests pass: `cargo test --test diagnostic_tests`

## Additional Resources

- `V4_EVENT_FILTERING.md` - Explains V4 two-stage filtering
- `REORG_HANDLING.md` - Explains chain reorganization handling
- `STATUS.md` - Current implementation status
