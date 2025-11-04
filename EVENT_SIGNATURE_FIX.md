# Event Signature Fix - Why Events Weren't Being Detected

**Date**: 2025-11-04
**Status**: ✅ FIXED

## Problem

The ExEx was not detecting any events from Uniswap pools despite watching pools with known activity.

## Root Cause

**Event names in the `sol!` macro were incorrect**, causing event signature mismatches.

### What Was Wrong

The original code used custom event names like `UniswapV2Swap`, `UniswapV3Swap`, etc.:

```rust
sol! {
    event UniswapV2Swap(
        address indexed sender,
        uint256 amount0In,
        ...
    );
}
```

This caused Alloy to calculate the signature from:
- `UniswapV2Swap(address,uint256,...)` ❌ Wrong!

The actual on-chain events are simply named `Swap`, `Mint`, `Burn`:
- `Swap(address,uint256,...)` ✅ Correct!

### Signature Comparison

| Event | Wrong Signature (old code) | Correct Signature | Match? |
|-------|---------------------------|-------------------|--------|
| V2 Swap | `0x7791f782...` | `0xd78ad95f...` | ❌ |
| V3 Swap | `0x...` (wrong) | `0xc42079f9...` | ❌ |
| V3 Mint | `0x...` (wrong) | `0x7a53080b...` | ❌ |

**Result**: The ExEx was listening for event signatures that don't exist on-chain, so it never matched any events.

## Solution

Wrapped event definitions in modules to allow using the correct on-chain names while avoiding naming conflicts:

```rust
// V2 events in separate module
mod v2 {
    use super::*;

    sol! {
        /// Event name MUST be "Swap" to match on-chain signature
        #[derive(Debug)]
        event Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );

        event Mint(...);
        event Burn(...);
    }
}

// Re-export with namespaced names to avoid conflicts
use v2::{Swap as UniswapV2Swap, Mint as UniswapV2Mint, Burn as UniswapV2Burn};

// V3 events in separate module
mod v3 {
    use super::*;

    sol! {
        event Swap(...);  // Correct name!
        event Mint(...);
        event Burn(...);
    }
}

use v3::{Swap as UniswapV3Swap, Mint as UniswapV3Mint, Burn as UniswapV3Burn};

// Same for V4...
```

This approach:
1. Uses correct on-chain event names for signature calculation
2. Avoids naming conflicts through module namespacing
3. Provides descriptive names via type aliases

## Verification

### Test Results

All 12 tests now pass:

```
test events::tests::test_event_signatures ... ok
test events::tests::test_decode_v2_swap ... ok
test events::tests::test_decode_v2_mint ... ok
test events::tests::test_decode_v2_burn ... ok
test events::tests::test_decode_v3_swap ... ok
test events::tests::test_decode_v3_mint ... ok
test events::tests::test_decode_v3_burn ... ok
test events::tests::test_decode_v4_swap ... ok
test events::tests::test_decode_v4_modify_liquidity ... ok
test events::tests::test_decode_real_v3_swap_event ... ok  ← Real event data!
test events::tests::test_decode_real_v3_mint_event ... ok  ← Real event data!
test events::tests::test_decode_unknown_event ... ok
```

### Correct Signatures

| Event | Signature | Formula |
|-------|-----------|---------|
| V2 Swap | `0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822` | `keccak256("Swap(address,uint256,uint256,uint256,uint256,address)")` |
| V2 Mint | `0x4c209b5fc8ad50758f13e2e1088ba56a560dff690a1c6fef26394f4c03821c4f` | `keccak256("Mint(address,uint256,uint256)")` |
| V2 Burn | `0xdccd412f0b1252819cb1fd330b93224ca42612892bb3f4f789976e6d81936496` | `keccak256("Burn(address,uint256,uint256,address)")` |
| V3 Swap | `0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67` | `keccak256("Swap(address,address,int256,int256,uint160,uint128,int24)")` |
| V3 Mint | `0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde` | `keccak256("Mint(address,address,int24,int24,uint128,uint256,uint256)")` |
| V3 Burn | `0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c` | `keccak256("Burn(address,int24,int24,uint128,uint256,uint256)")` |
| V4 Swap | `0x9cd312f3503782cb1d29f4114896ca5405e9cf41adf9a23b76f74203d292296e` | `keccak256("Swap(bytes32,address,int128,int128,uint160,uint128,int24)")` |
| V4 ModifyLiquidity | `0x541c041c2cce48e614b3de043c9280f06b6164c0a1741649e2de3c3d375f7974` | `keccak256("ModifyLiquidity(bytes32,address,int24,int24,int256)")` |

## Impact

### Before Fix
- ❌ No events detected from any pools
- ❌ Event signatures didn't match on-chain events
- ❌ Silent failure (no errors, just no matches)

### After Fix
- ✅ All event signatures match on-chain values
- ✅ Events can now be decoded correctly
- ✅ Real event data tested and verified

## Next Steps

1. **Rebuild the ExEx** with corrected signatures:
   ```bash
   cargo build --release
   ```

2. **Deploy to eth-docker** (copy updated binary to server)

3. **Verify event detection** in logs:
   ```bash
   ./ethd logs execution -f | grep -E "MINT|BURN|Swap|📊"
   ```

4. **Expected output** (once events are detected):
   ```
   INFO 🟢 MINT | Block 12345678 | Pool 0x88e6... | ...
   INFO 🔴 BURN | Block 12345679 | Pool 0x88e6... | ...
   INFO 📊 Block 12345680 summary: 5 Mints, 3 Burns
   ```

## Files Modified

- [src/events.rs](src/events.rs) - Complete rewrite of event definitions with correct names
  - Lines 14-49: V2 events in `mod v2`
  - Lines 55-97: V3 events in `mod v3`
  - Lines 103-134: V4 events in `mod v4`
  - Lines 289-610: Comprehensive test suite (12 tests)

## Key Takeaway

**When using Alloy's `sol!` macro, event names MUST match the exact on-chain event names.** The signature hash is calculated from the event name and parameter types. Custom names like "UniswapV2Swap" won't match "Swap" events on-chain.

---

**Status**: ✅ Fix complete and verified. Ready to rebuild and deploy.
