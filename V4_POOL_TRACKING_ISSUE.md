# V4 Pool Tracking Issue

**Status**: 🔴 CRITICAL BUG - V4 events not being tracked
**Date**: 2025-11-04

## Problem

The ExEx is currently checking `log.address` against tracked pool addresses, but **V4 events come from the PoolManager singleton contract**, not individual pool addresses.

## Current Broken Logic

```rust
// src/main.rs:379
for (tx_index, receipt) in receipts.iter().enumerate() {
    for (log_index, log) in receipt.logs().iter().enumerate() {
        let log_address = log.address;

        if !pool_tracker.is_tracked_address(&log_address) {
            continue;  // ❌ Skips ALL V4 events!
        }

        if let Some(decoded_event) = decode_log(log) {
            // Process event...
        }
    }
}
```

**Why this breaks V4**:
1. All V4 events are emitted from the **PoolManager singleton** (not individual pools)
2. We check if `log.address` (PoolManager) is in tracked pools → it's not
3. We skip the event before even decoding it
4. Result: **Zero V4 events detected**

## Expected NATS Message Format

### V2/V3 Pools (Current - Works Fine)

```json
{
  "type": "add",
  "chain": "ethereum",
  "timestamp": "2025-11-04T12:00:00Z",
  "snapshot_id": 1730635200000,
  "pools": [
    {
      "pool_id": {
        "Address": "0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"
      },
      "token0": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
      "token1": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
      "protocol": "UniswapV3",
      "factory": "0x1f98431c8ad98523631ae4a59f267346ea31f984",
      "tick_spacing": 60,
      "fee": 3000
    }
  ]
}
```

**For V2/V3**: The `pool_id.Address` field contains the pool contract address.

### V4 Pools (Expected Format)

```json
{
  "type": "add",
  "chain": "ethereum",
  "timestamp": "2025-11-04T12:00:00Z",
  "snapshot_id": 1730635200000,
  "pools": [
    {
      "pool_id": {
        "PoolId": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"
      },
      "token0": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
      "token1": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
      "protocol": "UniswapV4",
      "factory": "0x0000000000000000000000000000000000000000",  // PoolManager address
      "tick_spacing": 60,
      "fee": 3000
    }
  ]
}
```

**For V4**: The `pool_id.PoolId` field contains the 32-byte pool ID (bytes32).

## How V4 Events Work

### Event Emission

```solidity
// PoolManager.sol (0x...)
contract PoolManager {
    function swap(PoolKey calldata key, ...) external {
        bytes32 poolId = key.toId();

        // Event emitted FROM PoolManager, WITH poolId as indexed parameter
        emit Swap(poolId, msg.sender, amount0, amount1, sqrtPriceX96, liquidity, tick);
    }
}
```

### Event Structure

```
Event: Swap(bytes32 indexed poolId, address indexed sender, ...)

Log:
  address: 0x... (PoolManager address)  ← Same for ALL V4 pools
  topics: [
    0x9cd312f3... (Swap signature),
    0x1234...   (poolId - THIS identifies which pool)  ← Different per pool
    0xabc...    (sender)
  ]
  data: [amount0, amount1, sqrtPriceX96, liquidity, tick]
```

## Required Fix

### Step 1: Track PoolManager Address

For V4 pools, we need to track the PoolManager contract address:

```rust
// In PoolTracker::add_pools()
match pool_metadata.protocol {
    Protocol::UniswapV2 | Protocol::UniswapV3 => {
        // Track pool address directly
        self.tracked_addresses.insert(pool_address);
    }
    Protocol::UniswapV4 => {
        // Track PoolManager address (singleton)
        self.tracked_addresses.insert(UNISWAP_V4_POOL_MANAGER);
        // Also track poolId for filtering after decoding
        self.tracked_pool_ids.insert(pool_id);
    }
}
```

### Step 2: Update Event Filtering Logic

```rust
// src/main.rs - Updated filtering
for (log_index, log) in receipt.logs().iter().enumerate() {
    let log_address = log.address;

    // Decode first to check V4 poolId
    let decoded_event = match decode_log(log) {
        Some(event) => event,
        None => continue,
    };

    // Check if we should process this event
    let should_process = match &decoded_event {
        // V2/V3: Check pool address
        DecodedEvent::V2Swap { pool, .. }
        | DecodedEvent::V2Mint { pool, .. }
        | DecodedEvent::V2Burn { pool, .. }
        | DecodedEvent::V3Swap { pool, .. }
        | DecodedEvent::V3Mint { pool, .. }
        | DecodedEvent::V3Burn { pool, .. } => {
            pool_tracker.is_tracked_address(pool)
        }

        // V4: Check poolId (not address!)
        DecodedEvent::V4Swap { pool_id, .. }
        | DecodedEvent::V4ModifyLiquidity { pool_id, .. } => {
            pool_tracker.is_tracked_pool_id(pool_id)
        }
    };

    if !should_process {
        continue;
    }

    // Process event...
}
```

## PoolManager Address

**Mainnet**: Not yet deployed (V4 still in testing)
**Testnets**:
- Sepolia: `0x...` (TBD)
- Goerli: `0x...` (TBD)

**For now**: Use `address!("0000000000000000000000000000000000000000")` as placeholder.

## dynamicWhitelist Changes Needed

The `dynamicWhitelist` needs to:

1. **Query V4 pools differently**: Don't query by pool address, query by poolId
2. **Include PoolManager address**: For V4 pools, the `factory` field should contain the PoolManager address
3. **Provide pool_id as PoolId enum variant**: Use `{"PoolId": "0x..."}` not `{"Address": "0x..."}`

### Example dynamicWhitelist Query

```python
# For V2/V3 (works fine)
pool_address = "0x88e6..."
pool_metadata = {
    "pool_id": {"Address": pool_address},
    "protocol": "UniswapV3",
    ...
}

# For V4 (NEW)
pool_id_bytes32 = calculate_pool_id(token0, token1, fee, tick_spacing, hooks)
pool_metadata = {
    "pool_id": {"PoolId": pool_id_bytes32.hex()},
    "protocol": "UniswapV4",
    "factory": POOL_MANAGER_ADDRESS,  # Not a factory, but the singleton
    ...
}
```

## Summary

**Current State**:
- ✅ V2/V3 events: Working (filtered by pool address)
- ❌ V4 events: **Broken** (all skipped because PoolManager address not tracked)

**Required Changes**:
1. Track PoolManager address when V4 pools are added
2. Decode events first, then check poolId for V4 events
3. Update dynamicWhitelist to provide V4 pool data correctly

**Priority**: 🔴 High - Blocks all V4 event detection

---

## Testing V4 Detection

Once fixed, test with:

```bash
# 1. Publish a test V4 pool via NATS
nats pub "whitelist.pools.ethereum.minimal" '{
  "type": "add",
  "chain": "ethereum",
  "pools": [{
    "pool_id": {"PoolId": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"},
    "token0": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    "token1": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
    "protocol": "UniswapV4",
    "factory": "0x0000000000000000000000000000000000000000"
  }]
}'

# 2. Check ExEx logs for:
# "Added 1 V4 pool(s) to whitelist"
# "Tracking PoolManager address: 0x..."

# 3. When V4 events occur, should see:
# "📥 V4 Swap detected: poolId=0x1234..., tick=..."
```
