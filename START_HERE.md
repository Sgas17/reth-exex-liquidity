# Reth ExEx Liquidity Tracker - Quick Start

**Project Location**: `/home/sam-sullivan/reth-exex-liquidity/`

## Current Status: Build Blocked

The Phase 1 implementation is **complete** but the build is blocked by a missing system dependency.

### Fix the Build (Do This First)

```bash
# Install missing dependency
sudo apt install libclang-dev

# Build the ExEx (takes 5-15 minutes first time)
cd /home/sam-sullivan/reth-exex-liquidity
cargo build --release

# Binary will be at: target/release/exex
```

## What We Built

### Phase 1: Minimal ExEx ✅ (Code Complete, Needs Build)

**File**: [src/main.rs](src/main.rs)

The ExEx decodes Uniswap V3 liquidity events (Mint/Burn) in real-time:
- Filters 4 high-volume pools (USDC/WETH, WBTC/WETH, etc.)
- Decodes events using Alloy's type-safe sol! macro
- Logs events with full details
- Handles chain reorgs (logging only for now)

**Expected Output** (when running):
```
INFO Liquidity ExEx started
INFO Tracking 4 pools
INFO 🟢 MINT | Block 12345678 | Pool 0x88e6... | Ticks [-887220, -887210]
INFO 🔴 BURN | Block 12345679 | Pool 0x88e6... | Ticks [-887220, -887210]
INFO 📊 Block 12345678 summary: 5 Mints, 3 Burns
```

## How to Run (After Build Succeeds)

The ExEx **IS** a complete Reth node. You run it like this:

```bash
# Simple test (syncs from recent block)
./target/release/exex node \
  --chain mainnet \
  --datadir /tmp/reth-test \
  --http

# Production (full archive node)
./target/release/exex node \
  --chain mainnet \
  --datadir /mnt/nvme/reth \
  --http \
  --http.port 8545 \
  --authrpc.port 8551 \
  --authrpc.jwtsecret /path/to/jwt.hex \
  --full
```

Your ExEx code runs automatically as part of the Reth node process.

## Complete Documentation

All documentation is in this directory:

### 📚 Main Docs

1. **[DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md)** - How to deploy and run
   - Three deployment methods explained
   - Configuration options
   - Monitoring and troubleshooting
   - Systemd service setup

2. **[GETTING_STARTED.md](GETTING_STARTED.md)** - Implementation roadmap
   - Phase 1: Minimal ExEx (current)
   - Phase 2: gRPC streaming
   - Phase 3: Database integration
   - Phase 4: Production hardening

3. **[README.md](README.md)** - Project overview
   - Architecture comparison (old vs. new)
   - Performance expectations (60-3600x improvement)
   - Technology stack

4. **[IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md)** - Progress tracker
   - Current phase status
   - Success criteria
   - Next steps

### 🔧 Code

- **[src/main.rs](src/main.rs)** - The ExEx implementation
- **[Cargo.toml](Cargo.toml)** - Dependencies (Reth v1.8.2, Alloy, etc.)
- **[proto/liquidity.proto](proto/liquidity.proto)** - gRPC definitions (Phase 2)

### 📖 Reference

- **[reth-exex-examples](https://github.com/paradigmxyz/reth-exex-examples)** - Official examples
- **[Reth Docs](https://reth.rs/exex/overview)** - ExEx documentation

## Related: Completed Systems

### ✅ Liquidity Snapshot System (Production Ready)

Located in `/home/sam-sullivan/dynamicWhitelist/`:

**Storage Layers**:
- [src/core/storage/postgres_liquidity.py](../dynamicWhitelist/src/core/storage/postgres_liquidity.py) - Snapshot storage
- [src/core/storage/timescaledb_liquidity.py](../dynamicWhitelist/src/core/storage/timescaledb_liquidity.py) - Event storage

**Processing**:
- [src/processors/pools/unified_liquidity_processor.py](../dynamicWhitelist/src/processors/pools/unified_liquidity_processor.py) - Main processor

**Automation**:
- [src/scripts/run_liquidity_snapshot_generator.py](../dynamicWhitelist/src/scripts/run_liquidity_snapshot_generator.py) - Scheduled script
- [src/scripts/setup_liquidity_snapshot_cron.sh](../dynamicWhitelist/src/scripts/setup_liquidity_snapshot_cron.sh) - Cron installer

**Performance**: ~20 second startup (vs. 30+ minutes full replay)

### ✅ Pool Creation System (Production Ready)

- [src/processors/pipeline/uniswap_pool_pipeline.py](../dynamicWhitelist/src/processors/pipeline/uniswap_pool_pipeline.py) - Unified V2/V3/V4 pipeline
- All pools stored in PostgreSQL
- Incremental processing working

## Next Steps When You Return

### Immediate (5 minutes)

1. Install libclang-dev and build:
   ```bash
   sudo apt install libclang-dev
   cd /home/sam-sullivan/reth-exex-liquidity
   cargo build --release
   ```

### Phase 1 Testing (1-2 hours)

2. Test the ExEx:
   ```bash
   # Quick test with recent blocks
   ./target/release/exex node \
     --chain mainnet \
     --datadir /tmp/reth-test \
     --http

   # Watch for events in logs
   # Should see: "🟢 MINT" and "🔴 BURN" messages
   ```

3. Verify:
   - ✓ ExEx connects to Reth
   - ✓ Events are decoded correctly
   - ✓ Pool filtering works
   - ✓ No crashes or errors

### Phase 2 (Week 1-2)

4. Implement gRPC streaming:
   - Add gRPC server to Rust code
   - Create Python consumer
   - Stream events to Python in real-time

### Phase 3 (Week 2-3)

5. Database integration:
   - Connect Python consumer to PostgreSQL/TimescaleDB
   - Use existing storage layers
   - Validate against parquet-based system

### Phase 4 (Week 3-4)

6. Production hardening:
   - Proper reorg handling
   - Load pools from database
   - Monitoring/metrics
   - Deploy to production

## Performance Goals

**Current (Parquet-based)**:
- Latency: 5-60 minutes
- Throughput: ~1K events/sec

**Target (ExEx-based)**:
- Latency: <1 second
- Throughput: 10-50K events/sec
- **Improvement**: 60-3600x latency, 10-50x throughput

## Key Insights

1. **ExEx IS the Reth node**: You don't add it to a running node - you compile it into the node itself

2. **Zero IPC overhead**: ExEx runs in-process with direct access to block data

3. **Real-time processing**: Events processed as blocks are imported (microseconds to milliseconds)

4. **Replaces parquet pipeline**: Eventually replaces Cryo → Parquet → Processing flow

5. **Separate project for testing**: We created this as a standalone project to validate before integrating into main system

## Questions?

- **How do I add more pools?**: Edit `TRACKED_POOLS` constant in [src/main.rs](src/main.rs) (line 41)
- **How do I change what's logged?**: Edit the `info!()` calls in [src/main.rs](src/main.rs) (lines 96-127)
- **Can I run this without syncing the full chain?**: Yes! Use `--debug.tip <recent-block-hash>` to start from a recent block
- **Do I need to stop my existing Reth node?**: Yes, this IS your Reth node (or run on different ports)

## Resources Cloned

- `/home/sam-sullivan/reth-exex-examples/` - Official examples repository (cloned)
  - Check `minimal/` for the simplest example
  - Check `remote/` for remote ExEx example

## Summary

**What's Done**:
- ✅ Complete Phase 1 implementation (code)
- ✅ Comprehensive documentation
- ✅ Project structure set up
- ✅ Dependencies configured

**What's Blocked**:
- ⏸️ Build (needs libclang-dev)

**What's Next**:
- 🔄 Install dependency and build
- 🔄 Test with Reth node
- 🔄 Implement Phase 2 (gRPC)

---

**Run this when you return**:
```bash
sudo apt install libclang-dev
cd /home/sam-sullivan/reth-exex-liquidity
cargo build --release
./target/release/exex node --chain mainnet --datadir /tmp/reth-test --http
```

Good luck! 🚀
