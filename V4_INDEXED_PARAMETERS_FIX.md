# V4 Indexed Parameters Fix

**Date**: 2025-11-06
**Status**: ✅ COMPLETE

## Problem

V4 events were not being decoded correctly because the `poolId` parameter is **indexed**, which means it's stored in the log's `topics` array, not in the `data` field.

### Symptom
- No V4 events appearing in the database despite V4 pools being tracked
- Event signatures were correct
- Filtering logic was correct
- But decoding was failing silently

### Root Cause

The V4 event definitions use indexed parameters:

```solidity
event Swap(
    PoolId indexed id,      // <-- INDEXED (in topics)
    address indexed sender, // <-- INDEXED (in topics)
    int128 amount0,         // in data
    int128 amount1,         // in data
    uint160 sqrtPriceX96,  // in data
    uint128 liquidity,      // in data
    int24 tick,             // in data
    uint24 fee              // in data
);

event ModifyLiquidity(
    PoolId indexed id,      // <-- INDEXED (in topics)
    address indexed sender, // <-- INDEXED (in topics)
    int24 tickLower,        // in data
    int24 tickUpper,        // in data
    int256 liquidityDelta,  // in data
    bytes32 salt            // in data
);
```

## Ethereum Event Structure

When an event is emitted, indexed parameters are stored separately from non-indexed parameters:

### Log Structure
```
Log {
    address: <contract address>,
    topics: [
        topics[0]: event signature hash (keccak256 of event definition)
        topics[1]: first indexed parameter (poolId)
        topics[2]: second indexed parameter (sender)
        topics[3]: third indexed parameter (if any)
    ],
    data: <encoded non-indexed parameters>
}
```

### V4 Swap Event Example
```
topics[0] = 0x40e9cecb... (Swap event signature)
topics[1] = 0xdce6394... (poolId - 32 bytes)
topics[2] = 0x00000000...1234 (sender address - left-padded to 32 bytes)

data = <amount0><amount1><sqrtPriceX96><liquidity><tick><fee>
```

## The Bug

**OLD CODE** (broken):
```rust
// This only decodes the `data` field, missing indexed parameters!
if let Ok(event) = UniswapV4Swap::decode_log_data(&log.data) {
    let pool_id: [u8; 32] = event.poolId.into();  // ❌ poolId is NOT in data!
    // ...
}
```

The code was calling `decode_log_data(&log.data)` which:
1. ✅ Correctly decodes non-indexed parameters from `data`
2. ❌ Does NOT have access to indexed parameters in `topics`
3. ❌ The field `event.poolId` doesn't exist or is undefined

Result: V4 events fail to decode, so they're silently skipped.

## The Fix

**NEW CODE** (working):
```rust
// Extract poolId from topics BEFORE decoding data
if log.topics().len() >= 2 {
    if let Ok(event) = UniswapV4Swap::decode_log_data(&log.data) {
        // Extract poolId from topics[1] (first indexed parameter)
        let pool_id: [u8; 32] = log.topics()[1].into();  // ✅ Get from topics!
        return Some(DecodedEvent::V4Swap {
            pool_id,
            sqrt_price_x96: U256::from(event.sqrtPriceX96),
            liquidity: event.liquidity,
            tick: event.tick.as_i32(),
        });
    }
}
```

### Why V2/V3 Worked But V4 Didn't

**V2/V3 Events**:
- Pool address = `log.address` (the contract emitting the event)
- Pool-specific parameters are NOT indexed
- `decode_log_data(&log.data)` works fine ✅

**V4 Events**:
- Pool address = PoolManager singleton (same for all pools)
- `poolId` is indexed (in topics) to identify specific pool
- Must extract `poolId` from `topics[1]` ✅

## Changes Made

**File**: [src/events.rs](src/events.rs:253-291)

### Before:
```rust
// Try V4 events
if let Ok(event) = UniswapV4Swap::decode_log_data(&log.data) {
    let pool_id: [u8; 32] = event.poolId.into();  // ❌ WRONG
    // ...
}
```

### After:
```rust
// Try V4 events - poolId is indexed (in topics), not in data!
// topics[0] = event signature, topics[1] = poolId (indexed), topics[2] = sender (indexed)
if log.topics().len() >= 2 {
    if let Ok(event) = UniswapV4Swap::decode_log_data(&log.data) {
        // Extract poolId from topics[1] (first indexed parameter)
        let pool_id: [u8; 32] = log.topics()[1].into();  // ✅ CORRECT
        return Some(DecodedEvent::V4Swap {
            pool_id,
            sqrt_price_x96: U256::from(event.sqrtPriceX96),
            liquidity: event.liquidity,
            tick: event.tick.as_i32(),
        });
    }

    if let Ok(event) = UniswapV4ModifyLiquidity::decode_log_data(&log.data) {
        // Extract poolId from topics[1] (first indexed parameter)
        let pool_id: [u8; 32] = log.topics()[1].into();  // ✅ CORRECT
        // ... rest of decoding
    }
}
```

## Topics Array Index Reference

| Index | Content | Notes |
|-------|---------|-------|
| `topics[0]` | Event signature hash | Always present (keccak256 of event definition) |
| `topics[1]` | poolId (bytes32) | First indexed parameter |
| `topics[2]` | sender (address) | Second indexed parameter (left-padded to 32 bytes) |

## Testing

All 24 tests pass:
```bash
cargo test --lib
# ✅ test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### V4 Test Coverage
- `test_event_signatures` - Verifies correct V4 event signature hashes
- `test_decode_v4_swap` - Tests V4 Swap event decoding
- `test_decode_v4_modify_liquidity` - Tests V4 ModifyLiquidity event decoding

## Build Status

```bash
cargo build --release
# ✅ Finished `release` profile [optimized] target(s)
```

## Expected Behavior After Fix

With this fix, V4 events will now:

1. ✅ Be correctly filtered by PoolManager address
2. ✅ Be decoded successfully with poolId extracted from topics
3. ✅ Be checked against tracked pool IDs
4. ✅ Be sent over the socket to the database consumer
5. ✅ Appear in the database with correct pool identification

### Example V4 Swap Detection Flow

```
1. Block received with transaction containing V4 Swap
2. Log emitted from PoolManager (0x000000000004444c5dc75cb358380d2e3de08a90)
3. Stage 1 filter: PoolManager address is tracked ✅
4. Decode event:
   - Extract poolId from topics[1]: 0xdce6394339af00981949f5f3baf27e3610c76326a700af57e4b3e3ae4977f78d
   - Decode data: amount0, amount1, sqrtPriceX96, liquidity, tick, fee
5. Stage 2 filter: Check if poolId is tracked ✅
6. Create PoolUpdateMessage with V4 swap data
7. Send over socket to database consumer
8. Database records V4 swap event ✅
```

## Why This Bug Was Hard to Spot

1. **Event signatures were correct** - Tests passed for signature calculation
2. **Filtering logic was correct** - PoolManager was being tracked
3. **No error messages** - `decode_log_data` just returned `Err` silently, causing `None` result
4. **V2/V3 worked fine** - Pattern looked the same, but semantics were different

The key insight: **Indexed parameters are NOT part of the `data` field**.

## Related Documentation

- [Ethereum Event Topics](https://docs.soliditylang.org/en/latest/abi-spec.html#events)
- [Alloy Event Decoding](https://alloy.rs/)
- [V4_EVENT_FILTERING.md](V4_EVENT_FILTERING.md) - Two-stage filtering implementation
- [EVENT_SIGNATURE_FIX.md](EVENT_SIGNATURE_FIX.md) - Event name fix (previous)

---

**Status**: ✅ V4 events will now be properly decoded and recorded in the database.
