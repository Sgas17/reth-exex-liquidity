# Current Work Status - 2025-11-06

## Problem Summary
After pulling recent changes from both `reth-exex-liquidity` and `eventCaptureService` repositories, we lost working fixes that were in place. The pulled code was outdated and overwrote critical bug fixes.

## What We're Working On
Fixing deserialization issues between Rust ExEx (reth-exex-liquidity) and Python eventCaptureService so that liquidity events are properly captured and stored in TimescaleDB.

## Current Status

### ExEx (reth-exex-liquidity)
✅ **WORKING** - Rebuilt with all fixes from commit 577c30a:
- Socket broadcast implementation restored
- Event detection using decode_log() (not decode_log_data())
- Enhanced diagnostic logging
- V4 event signatures fixed (fee and salt fields added)
- Whitelist received successfully (5 pools: 2 V3, 2 V2, 1 V4)
- Events being detected and sent to socket

### eventCaptureService
⚠️ **PARTIALLY WORKING** - Some fixes applied, still debugging:
- Service running and connected to ExEx socket
- Some events deserializing correctly (sqrt_price_x96 and tick values now correct!)
- BUT: Events not being stored (0 updates stored)

## Critical Issues Found

### Issue 1: Enum Order Mismatch
**Problem**: Python `PoolUpdateVariant` enum didn't match Rust `types.rs` order

**Rust truth** (types.rs):
```rust
pub enum PoolUpdate {
    V2Swap { amount0: I256, amount1: I256 },        // 0
    V2Liquidity { amount0: I256, amount1: I256 },   // 1
    V3Swap { sqrt_price_x96: U256, ... },           // 2
    V3Liquidity { tick_lower: i32, ... },           // 3
    V4Swap { sqrt_price_x96: U256, ... },           // 4
    V4Liquidity { tick_lower: i32, ... },           // 5
}
```

**Fixed** ✅

### Issue 2: sqrt_price_x96 Masking
**Problem**: sqrtPriceX96 is uint160 (20 bytes) but stored in U256 (32 bytes). Reading all 32 bytes gave values 2^144 times too large.

**Fix**: Mask to 160 bits after reading U256:
```python
sqrt_price_raw = decoder.read_u256()
sqrt_price_x96 = sqrt_price_raw & ((1 << 160) - 1)  # CRITICAL!
```

**Status**: ✅ Fixed and verified working (seeing correct values like ~10^47 instead of ~10^76)

### Issue 3: U256 Serialization
**Problem**: Unclear if alloy_primitives::U256 serializes with or without u64 length prefix

**Current approach**: Reading with length prefix (8 bytes) + 32 bytes data
```python
length = self.read_u64()  # Should be 32
bytes_data = self.read_bytes(32)
result = int.from_bytes(bytes_data, byteorder='little')
```

**Status**: ✅ Seems to be working (getting correct sqrt_price_x96 values)

### Issue 4: Address/PoolId Serialization
**Problem**: Unclear if Address (20 bytes) and [u8; 32] serialize with or without length prefix

**Error seen**: "Expected pool_id length 32, got 10952946954240911068" (huge number = reading wrong bytes)

**Options**:
1. WITH length prefix: `read_u64()` then `read_bytes(20/32)`
2. WITHOUT length prefix: just `read_bytes(20/32)`

**Status**: ⚠️ NEEDS TESTING - Currently trying WITHOUT length prefix

### Issue 5: tx_hash Field Removed
**Problem**: Rust `PoolUpdateMessage` doesn't have `tx_hash` field, but old Python code tried to read it

**Fix**: Removed from bincode.py deserialization
**Status**: ✅ Fixed

### Issue 6: Missing Imports & Config
**Problems from pulled code**:
- Missing `from dotenv import load_dotenv` in main.py
- Relative imports instead of absolute imports
- Missing `text()` wrapper for SQL queries

**Status**: ✅ All fixed

## Working Stash

The stash (`stash@{0}`) contains most working fixes, but has wrong enum order for PoolUpdateVariant.

**Key files in stash**:
- `utils/bincode.py` - Has sqrt_price_x96 masking, U256 length prefix handling, enum updates
- `storage/schemas.py` - Has transaction_hash optional handling
- `capture/liquidity_capture.py` - Has skip logic for Ping/Pong messages
- `main.py`, `storage/timescaledb.py`, `capture/whitelist_sync.py` - Import and config fixes

## Next Steps

### Immediate (to get events storing):
1. **Test Address/PoolId serialization** - Try both with and without length prefix to see which works
   - Current hypothesis: NO length prefix (alloy primitives serialize as raw bytes)

2. **Verify schemas.py is correct** - Check that transaction_hash is optional:
   ```python
   required_fields = ["pool_address", "protocol", "event_type"]  # NOT transaction_hash
   "transaction_hash": update.get("transaction_hash", "")  # Optional, default empty
   ```

3. **Check for other validation issues** - Why are valid updates being skipped?

### Once Events Storing Successfully:
1. **Verify sqrt_price_x96 values** - Query database and confirm values are in correct range (~10^33 for tick ~195000)
2. **Test V4 event detection** - Monitor for V4 pool events (currently have 1 V4 pool in whitelist)
3. **Commit all working fixes** - Save to git so they don't get lost again!

## Key Learned: Bincode Serialization for alloy_primitives

| Type | Rust | Serialization |
|------|------|---------------|
| `U256` | `alloy_primitives::U256` | ✅ u64 length prefix (32) + 32 bytes little-endian |
| `I256` | `alloy_primitives::I256` | Same as U256, then convert to signed |
| `Address` | `alloy_primitives::Address` | ⚠️ TESTING: Raw 20 bytes (NO prefix) |
| `[u8; 32]` | Fixed array | ⚠️ TESTING: Raw 32 bytes (NO prefix) |
| `u128` | Primitive | 16 bytes little-endian |
| `i128` | Primitive | 16 bytes little-endian, sign-extend |
| `i32` | Primitive | 4 bytes little-endian |

## Contact Points

- **ExEx logs**: `docker logs eth-docker-execution-1 --tail 50`
- **eventCaptureService logs**: `tail -50 /tmp/eventcapture.log`
- **Database**: `psql -h localhost -p 5432 -U itrcap -d itrcap`
- **Test whitelist**: `python3 /home/sam-sullivan/reth-exex-liquidity/test_whitelist.py`

## Important Commands

```bash
# Rebuild ExEx (Ubuntu 22.04 for GLIBC 2.35)
docker run --rm -v /home/sam-sullivan/reth-exex-liquidity:/workspace -w /workspace ubuntu:22.04 bash -c "apt-get update -qq && apt-get install -y -qq curl build-essential > /dev/null 2>&1 && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y > /dev/null 2>&1 && . \$HOME/.cargo/env && cargo build --release"

# Restart ExEx
cd /home/sam-sullivan/eth-docker && ./ethd restart execution

# Restart eventCaptureService
pkill -f "python main.py" && cd /home/sam-sullivan/eventCaptureService && python main.py > /tmp/eventcapture.log 2>&1 &

# Send test whitelist
python3 /home/sam-sullivan/reth-exex-liquidity/test_whitelist.py

# Check recent events in database
PGPASSWORD=17rc4p psql -h localhost -p 5432 -U itrcap -d itrcap -c "SELECT block_number, sqrt_price_x96, tick, protocol FROM network_1_liquidity_updates WHERE event_type = 'Swap' ORDER BY block_number DESC LIMIT 10;"
```
