# Differential Whitelist Updates Design

## Problem

Current whitelist update mechanism has critical issues:

1. **Full replacement** - Clears all pools then rebuilds, creating a window where NO pools are tracked
2. **Race conditions** - Updates can happen mid-block, causing events to be skipped
3. **Inefficient** - Replacing 1000+ pools when only 1-2 changed is wasteful
4. **Event loss** - Events from pools during the replacement window are permanently lost

## Solution: Block-Synchronized Differential Updates

### Key Principles

1. **Differential updates** - Only add/remove pools that changed (not full replacement)
2. **Update queuing** - Whitelist changes queued, not applied immediately
3. **Block synchronization** - Updates applied **between blocks** to prevent event loss
4. **Atomic application** - All pending updates applied together at block boundary

## Architecture

```
NATS Whitelist Update              ExEx Main Loop
       │                                  │
       ├─ Add [Pool D, Pool E]           ├─ BeginBlock N
       │                                  │  ├─ pool_tracker.begin_block()
       ▼                                  │  ├─ Process events (read-only)
 Queue update                             │  └─ EndBlock N
 (not applied yet)                        │
       │                                  ├─ pool_tracker.end_block()
       │                                  │  └─ Apply pending updates
       │                                  │     ├─ Add Pool D
       │                                  │     └─ Add Pool E
       ▼                                  │
 Update queued                            ├─ BeginBlock N+1
 Waiting for block end...                 │  ├─ pool_tracker.begin_block()
                                          │  ├─ Process events (Pool D, E now tracked!)
                                          │  └─ EndBlock N+1
                                          │
                                          └─ pool_tracker.end_block()
```

## Implementation

### 1. New PoolTracker Methods

```rust
// src/pool_tracker_v2.rs

pub enum WhitelistUpdate {
    Add(Vec<PoolMetadata>),      // Add new pools
    Remove(Vec<PoolIdentifier>), // Remove pools
    Replace(Vec<PoolMetadata>),  // Full replacement (initial load only)
}

impl PoolTracker {
    /// Mark start of block - queue updates instead of applying
    pub fn begin_block(&mut self);

    /// Mark end of block - apply all pending updates atomically
    pub fn end_block(&mut self);

    /// Queue a whitelist update (applied at end of block)
    pub fn queue_update(&mut self, update: WhitelistUpdate);

    /// Internal: Apply all queued updates
    fn apply_pending_updates(&mut self);
}
```

### 2. Updated Main Loop

```rust
// src/main.rs - ChainCommitted handler

for (block, receipts) in new.blocks_and_receipts() {
    let block_number = block.number();
    let block_timestamp = block.timestamp();

    // 🔒 Start block - lock whitelist updates
    {
        let mut pool_tracker = exex.pool_tracker.write().await;
        pool_tracker.begin_block();
    }

    // Send BeginBlock marker
    exex.socket_tx.send(ControlMessage::BeginBlock {
        block_number,
        block_timestamp,
        is_revert: false,
    })?;

    // Process events (read-only access to pool_tracker)
    let pool_tracker = exex.pool_tracker.read().await;
    let mut events_in_block = 0;

    for (tx_index, receipt) in receipts.iter().enumerate() {
        for (log_index, log) in receipt.logs().iter().enumerate() {
            if !pool_tracker.is_tracked_address(&log.address) {
                continue; // Skip non-tracked pools
            }

            // Decode and send event...
            events_in_block += 1;
        }
    }

    drop(pool_tracker); // Release read lock

    // Send EndBlock marker
    exex.socket_tx.send(ControlMessage::EndBlock {
        block_number,
        num_updates: events_in_block,
    })?;

    // 🔓 End block - apply pending whitelist updates
    {
        let mut pool_tracker = exex.pool_tracker.write().await;
        pool_tracker.end_block(); // Applies queued updates atomically
    }
}
```

### 3. NATS Subscriber with Differential Updates

```rust
// Spawn NATS subscriber task
let pool_tracker = exex.pool_tracker.clone();
tokio::spawn(async move {
    while let Some(message) = subscriber.next().await {
        match parse_whitelist_message(&message) {
            Ok(WhitelistMessage::Add { pools }) => {
                pool_tracker.write().await.queue_update(
                    WhitelistUpdate::Add(pools)
                );
            }
            Ok(WhitelistMessage::Remove { pool_ids }) => {
                pool_tracker.write().await.queue_update(
                    WhitelistUpdate::Remove(pool_ids)
                );
            }
            Ok(WhitelistMessage::FullList { pools }) => {
                // Initial load or full sync
                pool_tracker.write().await.queue_update(
                    WhitelistUpdate::Replace(pools)
                );
            }
            Err(e) => warn!("Failed to parse whitelist: {}", e),
        }
    }
});
```

## NATS Message Format

Update the dynamicWhitelist NATS publisher to support differential updates:

### Current Format (Full Replacement)
```json
{
  "pools": [
    {
      "address": "0x...",
      "protocol": "V3",
      "token0": "0x...",
      "token1": "0x...",
      "fee": 3000,
      "tick_spacing": 60
    }
  ]
}
```

### New Format (Differential)
```json
{
  "type": "add",  // or "remove", "full"
  "pools": [
    {
      "address": "0x...",
      "protocol": "V3",
      "token0": "0x...",
      "token1": "0x...",
      "fee": 3000,
      "tick_spacing": 60
    }
  ]
}
```

**Or for removals:**
```json
{
  "type": "remove",
  "pool_ids": [
    "0xPoolAddress1",
    "0xPoolAddress2"
  ]
}
```

**Backward compatibility:** If `type` field is missing, treat as `"full"` (old behavior).

## Timeline Example

```
Block N:   Tracking {Pool A, Pool B, Pool C}
           │
           ├─ BeginBlock N
           │  └─ pool_tracker.begin_block() (lock updates)
           │
           ├─ NATS publishes: Add [Pool D, Pool E]
           │  └─ Queued, not applied yet
           │
           ├─ Process events for block N
           │  └─ Only Pool A, B, C tracked (Pool D, E not yet active)
           │
           ├─ EndBlock N
           │  └─ pool_tracker.end_block()
           │     └─ Apply: Add Pool D, Pool E
           │
Block N+1: Tracking {Pool A, Pool B, Pool C, Pool D, Pool E}
           │
           ├─ BeginBlock N+1
           │  └─ pool_tracker.begin_block()
           │
           ├─ Process events for block N+1
           │  └─ All pools tracked, including D and E ✅
           │
           ├─ EndBlock N+1
           │  └─ pool_tracker.end_block()
```

## Benefits

### ✅ Zero Event Loss
- Updates only applied between blocks
- No mid-block whitelist changes
- Complete block consistency

### ✅ Efficiency
- Only modified pools are added/removed
- No unnecessary clearing and rebuilding
- Minimal lock contention

### ✅ Correctness
- Atomic updates at block boundaries
- Read-only access during event processing
- No race conditions

### ✅ Simplicity
- Clear synchronization points (BeginBlock/EndBlock)
- Easy to reason about
- Testable with unit tests

## Migration Path

### Phase 1: Add V2 Pool Tracker
1. ✅ Create `pool_tracker_v2.rs` with differential updates
2. ✅ Add tests for add/remove/replace operations
3. ✅ Add block synchronization tests

### Phase 2: Update Main Loop
1. Add `begin_block()` calls at start of each block
2. Add `end_block()` calls after EndBlock message sent
3. Test with existing NATS messages (backward compatible)

### Phase 3: Update dynamicWhitelist Publisher
1. Add `type` field to NATS messages
2. Implement differential publishing:
   - On pool selection: publish `Add` for new pools
   - On pool removal: publish `Remove` for removed pools
   - On startup: publish `Full` for complete list
3. Maintain backward compatibility (no `type` = full replacement)

### Phase 4: Replace Old Pool Tracker
1. Replace `pool_tracker.rs` with `pool_tracker_v2.rs`
2. Update all imports
3. Remove old implementation

## Testing Strategy

### Unit Tests
```rust
#[test]
fn test_block_synchronized_updates() {
    let mut tracker = PoolTracker::new();

    // Start block
    tracker.begin_block();

    // Queue update during block
    tracker.queue_update(WhitelistUpdate::Add(vec![pool1]));

    // Should not be applied yet
    assert_eq!(tracker.stats().total_pools, 0);

    // End block - updates applied
    tracker.end_block();

    assert_eq!(tracker.stats().total_pools, 1);
}
```

### Integration Tests
1. Simulate NATS publishing differential updates
2. Process blocks concurrently
3. Verify no events lost
4. Verify correct pool tracking state

### Stress Tests
1. Rapid whitelist updates (100/sec)
2. Large pool counts (10,000+ pools)
3. Concurrent add/remove operations
4. Reorg handling with pending updates

## Performance Characteristics

### Memory
- **Old:** O(n) temporary allocation for full replacement
- **New:** O(k) where k = changed pools (typically k << n)

### Lock Contention
- **Old:** Write lock held during full clear + rebuild (~10-50ms)
- **New:** Write lock held only for differential update (~0.1-1ms)

### CPU
- **Old:** O(n) to clear + O(n) to rebuild = O(2n)
- **New:** O(k) to add/remove changed pools

### Example (1000 pools, 5 changed)
- **Old:** 1000 clears + 1000 inserts = 2000 operations
- **New:** 5 adds or removes = 5 operations
- **Speedup:** 400x faster 🚀

## Edge Cases

### Case 1: Update During Reorg
**Scenario:** Whitelist update arrives while processing reorg.

**Solution:** Updates still queued, applied after reorg completes.

```rust
ExExNotification::ChainReorged { old, new } => {
    // Process old blocks (revert)
    for block in old.blocks() {
        pool_tracker.begin_block();
        // ... process revert
        pool_tracker.end_block(); // Updates may be applied here
    }

    // Process new blocks (forward)
    for block in new.blocks() {
        pool_tracker.begin_block();
        // ... process forward
        pool_tracker.end_block(); // Or here
    }
}
```

### Case 2: Multiple Updates Queued
**Scenario:** Multiple NATS messages before block ends.

**Solution:** All queued in order, applied atomically at end of block.

```rust
pending_updates = [
    Add([Pool D]),
    Add([Pool E]),
    Remove([Pool A]),
]

// At end_block():
// - Add Pool D
// - Add Pool E
// - Remove Pool A
// All applied atomically
```

### Case 3: Add Then Remove Same Pool
**Scenario:** Pool added and removed in same block.

**Solution:** Both operations queued and applied in order (net effect: no change).

**Optimization:** Could detect and cancel out opposite operations, but not critical.

### Case 4: NATS Disconnected
**Scenario:** NATS connection lost, no updates received.

**Solution:** ExEx continues with last known whitelist (already cached).

```rust
// No updates queued = no changes applied
// Cached whitelist remains stable
```

## Monitoring

### Metrics to Track
```rust
pub struct WhitelistMetrics {
    pending_updates: usize,        // Current queue size
    updates_applied: u64,          // Total updates applied
    pools_added: u64,              // Pools added over time
    pools_removed: u64,            // Pools removed over time
    update_latency_ms: f64,        // Time from NATS msg to application
    largest_queue_size: usize,     // Max pending updates
}
```

### Logging
```
INFO  Queuing add: 2 pools
DEBUG   Pool 0xABC...: V3 USDC/WETH
DEBUG   Pool 0xDEF...: V3 WETH/DAI
INFO  Applying 3 pending whitelist updates
INFO  Added 2 new pools to whitelist
INFO  Whitelist now tracking: 450 V2, 580 V3, 15 V4 pools (total: 1045)
```

### Alerts
- ⚠️ Pending queue size > 100 (too many updates, may indicate slow blocks)
- ⚠️ Update latency > 60 seconds (NATS messages not being processed)
- 🚨 Whitelist empty (critical - all pools removed by mistake)

## Summary

The differential update system provides:

✅ **Zero event loss** - Block-synchronized updates
✅ **Efficiency** - Only changed pools updated
✅ **Correctness** - Atomic updates at safe boundaries
✅ **Simplicity** - Clear synchronization model
✅ **Performance** - 100-400x faster for typical updates

**Next step:** Implement Phase 2 (update main loop) and test with existing NATS messages.
