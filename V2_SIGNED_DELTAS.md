# V2 Signed Reserve Deltas Fix

**Date**: 2025-11-06
**Status**: ✅ COMPLETE

## Problem

The V2 event handling was sending **unsigned** reserve amounts (U256), but the Python consumer expected **signed deltas** (I256) to maintain pool state:

- **Mint**: Positive amounts (liquidity added to reserves)
- **Burn**: Negative amounts (liquidity removed from reserves)
- **Swap**: One positive (token in), one negative (token out)

The database consumer needs to maintain reserve state by applying these signed deltas:
```python
new_reserve0 = current_reserve0 + delta0  # delta can be positive or negative
new_reserve1 = current_reserve1 + delta1
```

## Original Python Logic

```python
# Burn: negative deltas
if msg['block_event']['topics'][0] == topics[os.getenv('CHAIN')]['V2']['topics']['burn']:
    amount0 = -int(msg['block_event']['data'][2:66], 16)
    amount1 = -int(msg['block_event']['data'][66:130], 16)

# Mint: positive deltas
elif msg['block_event']['topics'][0] == topics[os.getenv('CHAIN')]['V2']['topics']['mint']:
    amount0 = int(msg['block_event']['data'][2:66], 16)
    amount1 = int(msg['block_event']['data'][66:130], 16)

# Swap: one positive (in), one negative (out)
elif msg['block_event']['topics'][0] == topics[os.getenv('CHAIN')]['V2']['topics']['swap']:
    amount0In = int(msg['block_event']['data'][2:66], 16)
    amount1In = int(msg['block_event']['data'][66:130], 16)
    amount0Out = -int(msg['block_event']['data'][130:194], 16)  # Negative!
    amount1Out = -int(msg['block_event']['data'][194:], 16)     # Negative!

    if amount0In == 0:
        amount0 = amount0Out  # Negative (token0 going out)
        amount1 = amount1In   # Positive (token1 coming in)
    else:
        amount0 = amount0In   # Positive (token0 coming in)
        amount1 = amount1Out  # Negative (token1 going out)
```

## Solution

Changed V2 reserve deltas from unsigned (U256) to signed (I256) and implemented proper sign logic.

### 1. Updated Type Definition

**File**: [src/types.rs](src/types.rs:5)

```rust
use alloy_primitives::{Address, I256, U256};  // Added I256

pub enum PoolUpdate {
    /// V2 Reserve Delta (signed - positive for adds, negative for removes)
    /// Mint: both positive, Burn: both negative, Swap: one positive (in), one negative (out)
    V2Reserves { reserve0: I256, reserve1: I256 },  // Changed from U256 to I256
    // ... other variants
}
```

### 2. Updated V2 Swap Logic

**File**: [src/main.rs](src/main.rs:71-106)

```rust
DecodedEvent::V2Swap {
    pool,
    amount0_in,
    amount1_in,
    amount0_out,
    amount1_out,
} => {
    // Calculate signed reserve deltas for V2 swaps
    // Match Python logic: if amount0In == 0, use (negative out, positive in), else (positive in, negative out)
    let (amount0, amount1) = if amount0_in == U256::ZERO {
        // Token1 -> Token0 swap: token0 going OUT (negative), token1 coming IN (positive)
        let delta0 = -I256::try_from(amount0_out).unwrap_or(I256::ZERO);
        let delta1 = I256::try_from(amount1_in).unwrap_or(I256::ZERO);
        (delta0, delta1)
    } else {
        // Token0 -> Token1 swap: token0 coming IN (positive), token1 going OUT (negative)
        let delta0 = I256::try_from(amount0_in).unwrap_or(I256::ZERO);
        let delta1 = -I256::try_from(amount1_out).unwrap_or(I256::ZERO);
        (delta0, delta1)
    };

    Some(PoolUpdateMessage {
        // ...
        update: PoolUpdate::V2Reserves {
            reserve0: amount0,  // Signed I256
            reserve1: amount1,  // Signed I256
        },
    })
}
```

### 3. Updated V2 Mint Logic

**File**: [src/main.rs](src/main.rs:108-131)

```rust
DecodedEvent::V2Mint {
    pool,
    amount0,
    amount1,
} => {
    // Mint: positive deltas (liquidity added)
    let delta0 = I256::try_from(amount0).unwrap_or(I256::ZERO);
    let delta1 = I256::try_from(amount1).unwrap_or(I256::ZERO);

    Some(PoolUpdateMessage {
        // ...
        update: PoolUpdate::V2Reserves {
            reserve0: delta0,  // Positive
            reserve1: delta1,  // Positive
        },
    })
}
```

### 4. Updated V2 Burn Logic

**File**: [src/main.rs](src/main.rs:133-156)

```rust
DecodedEvent::V2Burn {
    pool,
    amount0,
    amount1,
} => {
    // Burn: negative deltas (liquidity removed)
    let delta0 = -I256::try_from(amount0).unwrap_or(I256::ZERO);
    let delta1 = -I256::try_from(amount1).unwrap_or(I256::ZERO);

    Some(PoolUpdateMessage {
        // ...
        update: PoolUpdate::V2Reserves {
            reserve0: delta0,  // Negative
            reserve1: delta1,  // Negative
        },
    })
}
```

## Event Sign Logic Summary

| Event Type | reserve0 (amount0) | reserve1 (amount1) | Logic |
|------------|-------------------|-------------------|--------|
| **Mint**   | Positive (+)      | Positive (+)      | Liquidity added to pool |
| **Burn**   | Negative (-)      | Negative (-)      | Liquidity removed from pool |
| **Swap** (0→1) | Positive (+) | Negative (-) | Token0 in, Token1 out |
| **Swap** (1→0) | Negative (-) | Positive (+) | Token0 out, Token1 in |

## Database State Update

The Python consumer can now simply apply deltas to maintain reserves:

```python
# OLD (required parsing event type and applying signs)
if event_type == 'mint':
    reserve0 += amount0
    reserve1 += amount1
elif event_type == 'burn':
    reserve0 -= amount0
    reserve1 -= amount1
elif event_type == 'swap':
    # Complex logic...

# NEW (just apply signed deltas)
reserve0 += amount0  # amount0 is already signed
reserve1 += amount1  # amount1 is already signed
```

## Serialization Format

When serialized to JSON over the Unix socket, I256 values are sent as signed decimal strings:

```json
{
  "pool_id": {"Address": "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"},
  "protocol": "UniswapV2",
  "update_type": "Swap",
  "block_number": 12345678,
  "update": {
    "V2Reserves": {
      "reserve0": "1000000000000000000",      // Positive (token in)
      "reserve1": "-500000000000000000"       // Negative (token out)
    }
  }
}
```

## Example Scenarios

### Scenario 1: USDC → WETH Swap
- User swaps 1000 USDC for WETH
- `amount1_in = 1000e6` (USDC coming in)
- `amount0_out = 0.5e18` (WETH going out)
- **Result**: `reserve0 = -0.5e18` (negative), `reserve1 = 1000e6` (positive)

### Scenario 2: Add Liquidity (Mint)
- User adds 1 WETH + 2000 USDC
- `amount0 = 1e18`, `amount1 = 2000e6`
- **Result**: `reserve0 = 1e18` (positive), `reserve1 = 2000e6` (positive)

### Scenario 3: Remove Liquidity (Burn)
- User removes 0.5 WETH + 1000 USDC
- `amount0 = 0.5e18`, `amount1 = 1000e6`
- **Result**: `reserve0 = -0.5e18` (negative), `reserve1 = -1000e6` (negative)

## Testing

All 24 tests pass with signed deltas:

```bash
cargo test --lib
# ✅ test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Build Status

```bash
cargo build --release
# ✅ Finished `release` profile [optimized] target(s)
```

## Database Schema Implications

The Python consumer can now simplify the database schema and processing:

**OLD Schema** (required storing all swap components):
```sql
CREATE TABLE v2_events (
    pool_address TEXT,
    amount0_in NUMERIC,
    amount1_in NUMERIC,
    amount0_out NUMERIC,
    amount1_out NUMERIC,
    -- Complex logic to calculate net deltas
);
```

**NEW Schema** (just store signed deltas):
```sql
CREATE TABLE pool_events (
    pool_address TEXT,
    reserve0_delta NUMERIC,  -- Signed delta
    reserve1_delta NUMERIC,  -- Signed delta
    -- Simple addition to maintain state
);
```

## Backwards Compatibility

**Breaking Change**: This changes the socket message format. The Python consumer must be updated to:

1. Expect I256 (signed) instead of U256 (unsigned) for V2 reserves
2. Remove complex swap direction logic (signs are now in the data)
3. Simplify database schema to store signed deltas

## Files Modified

- [src/types.rs](src/types.rs) - Changed V2Reserves from U256 to I256, added I256 import
- [src/main.rs](src/main.rs) - Updated V2 Swap/Mint/Burn logic with correct signs, added I256 import

## Next Steps

1. **Update Python Consumer**:
   ```python
   # Parse signed deltas
   reserve0_delta = int(msg['update']['V2Reserves']['reserve0'])  # Can be negative!
   reserve1_delta = int(msg['update']['V2Reserves']['reserve1'])  # Can be negative!

   # Apply to current state
   new_reserve0 = current_reserve0 + reserve0_delta
   new_reserve1 = current_reserve1 + reserve1_delta
   ```

2. **Update Database Schema**:
   - Drop unused columns: `amount0_in`, `amount1_in`, `amount0_out`, `amount1_out`
   - Keep only: `reserve0_delta`, `reserve1_delta` (both signed)

3. **Test with Real Data**:
   - Monitor V2 pools with known activity
   - Verify reserve calculations match on-chain state

---

**Status**: ✅ Implementation complete. ExEx now sends correct signed deltas matching legacy Python logic.
