# Blockchain Reorg and Revert Handling

## Overview

The ExEx now properly handles blockchain reorganizations and reverts by decoding events from reverted blocks and sending them with an `is_revert` flag to the orderbook engine via Unix socket.

## Key Concepts

### Chain Revert vs Chain Reorg

**ChainReverted**: Simple rollback of blocks
- Node detects blocks are no longer valid
- Only provides `old` blocks that need to be undone
- No replacement blocks yet

**ChainReorged**: Blockchain reorganization
- Some blocks (`old`) are replaced with different blocks (`new`)
- Both chains share a common ancestor but diverge
- The `new` chain is now canonical
- Common during normal blockchain operations

### Visual Example

```
Before reorg:
  A -> B -> C -> D -> E

After reorg (D and E replaced):
  A -> B -> C -> D' -> E' -> F'

ChainReorged event:
  old = [D, E]       (blocks to revert)
  new = [D', E', F'] (new blocks to apply)
```

## Implementation

### Message Format

Added `is_revert` flag to `PoolUpdateMessage`:

```rust
pub struct PoolUpdateMessage {
    pub pool_id: PoolIdentifier,
    pub protocol: Protocol,
    pub update_type: UpdateType,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub tx_index: u64,
    pub log_index: u64,
    pub is_revert: bool,  // NEW: indicates this is a revert
    pub update: PoolUpdate,
}
```

### Event Handling

#### ChainCommitted (Normal Flow)
```rust
ExExNotification::ChainCommitted { new } => {
    // Process blocks with is_revert = false
    for (block, receipts) in new.blocks_and_receipts() {
        // Decode events from logs
        // Create PoolUpdateMessage with is_revert = false
        // Send to orderbook engine via socket
    }
}
```

#### ChainReorged (Two-Phase Handling)
```rust
ExExNotification::ChainReorged { old, new } => {
    // Step 1: Revert old blocks
    for (block, receipts) in old.blocks_and_receipts() {
        // Decode events from logs
        // Create PoolUpdateMessage with is_revert = true
        // Send to orderbook engine
    }

    // Step 2: Apply new blocks
    for (block, receipts) in new.blocks_and_receipts() {
        // Decode events normally
        // Create PoolUpdateMessage with is_revert = false
        // Send to orderbook engine
    }
}
```

#### ChainReverted (Revert Only)
```rust
ExExNotification::ChainReverted { old } => {
    // Revert blocks only (no new blocks)
    for (block, receipts) in old.blocks_and_receipts() {
        // Decode events from logs
        // Create PoolUpdateMessage with is_revert = true
        // Send to orderbook engine
    }
}
```

## Consumer Responsibilities (poolStateArena)

The poolStateArena receives messages in block-level batches and must handle them atomically:

### Block-Level Message Flow

```
BeginBlock { block_number: 100, is_revert: false }
  PoolUpdate { pool_id: 0xabc..., update_type: Swap, is_revert: false, ... }
  PoolUpdate { pool_id: 0xdef..., update_type: Mint, is_revert: false, ... }
  PoolUpdate { pool_id: 0xabc..., update_type: Swap, is_revert: false, ... }
EndBlock { block_number: 100, num_updates: 3 }

→ Consumer applies all 3 updates atomically
→ Snapshots swap states for pools that had swaps
→ Recalculates optimal paths
→ Notifies trading engine
```

### Handling Reverts

**Swap Reverts:** Cannot be inverted - must restore from snapshot
```rust
if msg.is_revert && matches!(msg.update, PoolUpdate::V3Swap { .. }) {
    // Load snapshot from previous block
    let snapshot = snapshot_cache.get(msg.pool_id, msg.block_number - 1)?;
    pool.sqrt_price_x96 = snapshot.sqrt_price_x96;
    pool.liquidity = snapshot.liquidity;
    pool.tick = snapshot.tick;
}
```

**Mint/Burn Reverts:** Can be inverted directly
```rust
if msg.is_revert && matches!(msg.update, PoolUpdate::V3Liquidity { .. }) {
    // Invert the liquidity delta
    pool.modify_liquidity(tick_lower, tick_upper, -liquidity_delta)?;
}
```

### Minimal Snapshot Strategy

**Only snapshot swap-affected state (52 bytes per pool):**
- `sqrt_price_x96` (32 bytes)
- `liquidity` (16 bytes)
- `tick` (4 bytes)

**Don't snapshot:**
- tickData (reconstructed from mint/burn reverts)
- tickBitmap (reconstructed from mint/burn reverts)
- Positions (reconstructed from mint/burn reverts)

### Example Implementation

```rust
struct PoolStateArena {
    pools: MmapPoolStates,
    snapshot_cache: SnapshotCache,
    pending_block: Option<PendingBlock>,
}

struct PendingBlock {
    block_number: u64,
    is_revert: bool,
    updates: Vec<PoolUpdateMessage>,
}

struct SnapshotCache {
    // (pool_id, block_number) → minimal swap state
    states: HashMap<(PoolId, u64), PoolSwapSnapshot>,
    ring_buffer_depth: u64, // 128 blocks recommended
}

struct PoolSwapSnapshot {
    sqrt_price_x96: U256,
    liquidity: u128,
    tick: i32,
}

impl PoolStateArena {
    async fn handle_message(&mut self, msg: ControlMessage) {
        match msg {
            ControlMessage::BeginBlock { block_number, is_revert, .. } => {
                self.pending_block = Some(PendingBlock {
                    block_number,
                    is_revert,
                    updates: Vec::new(),
                });
            }

            ControlMessage::PoolUpdate(update) => {
                // Buffer the update
                if let Some(pending) = &mut self.pending_block {
                    pending.updates.push(update);
                }
            }

            ControlMessage::EndBlock { num_updates, .. } => {
                // Validate and process complete block
                if let Some(pending) = self.pending_block.take() {
                    assert_eq!(pending.updates.len() as u64, num_updates);
                    self.process_complete_block(pending)?;
                }
            }
        }
    }

    fn process_complete_block(&mut self, block: PendingBlock) -> Result<()> {
        if block.is_revert {
            // Revert swaps using snapshots
            for update in &block.updates {
                if is_swap(update) {
                    let snapshot = self.snapshot_cache
                        .get(update.pool_id, block.block_number - 1)?;
                    self.pools.restore_swap_state(update.pool_id, snapshot);
                }
            }

            // Invert mint/burn operations
            for update in &block.updates {
                if is_liquidity(update) {
                    self.apply_inverted_liquidity(update)?;
                }
            }
        } else {
            // Forward: apply all updates
            for update in &block.updates {
                self.apply_update(update)?;
            }

            // Snapshot pools that had swaps
            let pools_with_swaps: HashSet<_> = block.updates
                .iter()
                .filter(|u| is_swap(u))
                .map(|u| u.pool_id)
                .collect();

            for pool_id in pools_with_swaps {
                let state = self.get_swap_snapshot(pool_id)?;
                self.snapshot_cache.save(pool_id, block.block_number, state);
            }
        }

        // Block complete - recalculate paths with consistent state
        self.recalculate_optimal_paths()?;
        self.notify_trading_engine()?;

        Ok(())
    }
}
```

### Memory Usage (Real-World)

With **~1000 watched pools** and **128 block history:**

**Typical block:** 30 pools swap
- 30 pools × 52 bytes × 128 blocks = **200 KB**

**Heavy block:** 200 pools swap
- 200 pools × 52 bytes × 128 blocks = **1.3 MB**

This is extremely manageable and provides 25+ minutes of reorg protection.

## Benefits of This Approach

1. **Complete Information**: Consumer receives all events from reverted blocks, not just block numbers
2. **Stateless ExEx**: ExEx doesn't need to track state history, just forwards events
3. **Flexible Consumer**: poolStateArena can implement sophisticated rollback strategies
4. **Efficient**: No need for the ExEx to query historical state

## Testing Reorgs

To test reorg handling in development:

1. Use a test network where reorgs are common (e.g., Holesky, Sepolia)
2. Monitor logs for reorg events:
   ```
   ⚠️  Chain reorg detected: reverting 2 old blocks, applying 3 new blocks
   Step 1: Reverting 2 old blocks
   Block 12345: reverted 5 liquidity events
   Block 12346: reverted 3 liquidity events
   Step 2: Processing 3 new blocks
   Block 12345: processed 4 liquidity events
   Block 12346: processed 2 liquidity events
   Block 12347: processed 6 liquidity events
   ✅ Reorg handled successfully
   ```

## Future Enhancements

1. **Reorg Metrics**: Track reorg frequency and depth
2. **State Snapshots**: Periodically checkpoint pool states for faster rollback
3. **Reorg Alerts**: Notify monitoring systems of significant reorgs
4. **Validation**: Verify pool states match after reorg resolution

## Related Files

- [src/main.rs](src/main.rs) - Main ExEx logic with reorg handling
- [src/types.rs](src/types.rs) - Message types with `is_revert` flag
- [src/socket.rs](src/socket.rs) - Unix socket for sending updates
- [NATS_INTEGRATION_COMPLETE.md](NATS_INTEGRATION_COMPLETE.md) - NATS integration details
