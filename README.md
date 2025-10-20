# Reth ExEx for Real-Time Liquidity Tracking

A Reth Execution Extension (ExEx) that monitors Uniswap V3/V4 liquidity events (Mint/Burn) in real-time and streams them to a Python consumer via gRPC.

## Overview

This project replaces the traditional parquet-based ETL pipeline with a high-performance, real-time event streaming system:

**Old Architecture:**
```
Reth → JSON-RPC → Cryo → Parquet → Python → Database
(minutes-hours latency, multiple processes)
```

**New Architecture:**
```
Reth (with ExEx) → gRPC → Python → Database
(real-time, 10-100x faster)
```

## Project Structure

```
reth-exex-liquidity/
├── Cargo.toml                    # Rust dependencies
├── build.rs                      # Protobuf code generation
├── proto/
│   └── liquidity.proto           # gRPC protocol definitions
├── src/
│   ├── main.rs                   # ExEx entry point (TO BE CREATED)
│   ├── grpc_server.rs            # gRPC server implementation (TO BE CREATED)
│   └── pool_tracker.rs           # Pool address tracking (TO BE CREATED)
└── python-consumer/
    ├── requirements.txt          # Python dependencies (TO BE CREATED)
    ├── consumer.py               # Python gRPC client (TO BE CREATED)
    └── test_consumer.py          # Tests and validation (TO BE CREATED)
```

## Prerequisites

### Rust Setup

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install required tools
rustup default stable
```

### Reth Node

You need a running Reth node. Options:

**Option 1: Local Reth (Recommended for Development)**
```bash
# Clone and build Reth
git clone https://github.com/paradigmxyz/reth.git
cd reth
cargo build --release --features exex

# Run Reth (sync required)
./target/release/reth node
```

**Option 2: Use Existing Reth Instance**
If you already have Reth running, note the IPC/HTTP endpoint.

## Development Phases

### Phase 1: Minimal ExEx (Week 1) - START HERE

**Goal**: Get basic ExEx running that prints Uniswap events to console

**Tasks**:
1. ✅ Set up Cargo project
2. ✅ Create protobuf definitions
3. ⏳ Implement minimal ExEx (decode events, print to console)
4. ⏳ Test with Reth node

**Success Criteria**:
- ExEx compiles
- Connects to Reth node
- Prints Mint/Burn events from known Uniswap pools

### Phase 2: gRPC Streaming (Week 1-2)

**Goal**: Stream events to Python consumer

**Tasks**:
1. Implement gRPC server in Rust
2. Create Python gRPC client
3. Stream events in real-time
4. Handle basic reconnection logic

**Success Criteria**:
- Python receives events via gRPC
- Events match Etherscan data
- Handles Reth restarts gracefully

### Phase 3: Database Integration (Week 2-3)

**Goal**: Connect to existing dynamicWhitelist storage

**Tasks**:
1. Import storage functions from main project
2. Store events to TimescaleDB
3. Update tick maps in memory
4. Save periodic snapshots to PostgreSQL

**Success Criteria**:
- Events stored in database
- Tick maps match parquet-based system
- Snapshots consistent with legacy data

### Phase 4: Production Hardening (Week 3-4)

**Goal**: Handle reorgs, errors, and edge cases

**Tasks**:
1. Implement reorg handling
2. Add state recovery from snapshots
3. Monitor performance metrics
4. Load pool list dynamically from database

**Success Criteria**:
- Reorgs handled correctly
- Performance: 10-100x faster than parquet
- Ready for parallel production deployment

## Quick Start (After Implementation)

### 1. Build the ExEx

```bash
cd reth-exex-liquidity
cargo build --release
```

### 2. Run ExEx with Reth

```bash
# Option A: Run as part of Reth binary (production)
reth node --exex reth-exex-liquidity

# Option B: Run standalone (development)
./target/release/exex --reth-ipc /path/to/reth.ipc
```

### 3. Run Python Consumer

```bash
cd python-consumer
pip install -r requirements.txt
python consumer.py
```

## Configuration

### Pool Tracking

Create `config.json` with pools to track:

```json
{
  "tracked_pools": [
    "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",  // USDC/WETH 0.05%
    "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8"   // USDC/WETH 0.3%
  ],
  "factory_addresses": [
    "0x1F98431c8aD98523631AE4a59f267346ea31F984"   // Uniswap V3 Factory
  ]
}
```

Or load dynamically from database (Phase 4).

### gRPC Configuration

Set environment variables:

```bash
export GRPC_HOST=0.0.0.0
export GRPC_PORT=10000
export LOG_LEVEL=info
```

## Testing

### Unit Tests (Rust)

```bash
cargo test
```

### Integration Tests (Python)

```bash
cd python-consumer
pytest test_consumer.py
```

### Validation Against Parquet System

```bash
# Compare outputs
python validate.py --pool 0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640 --blocks 100
```

## Performance Expectations

| Metric | Parquet System | ExEx System | Improvement |
|--------|----------------|-------------|-------------|
| Latency | 5-60 minutes | <1 second | **60-3600x** |
| Throughput | ~1K events/sec | ~10-50K events/sec | **10-50x** |
| CPU Usage | Medium | Low | **Better** |
| Storage | Large (parquet files) | Minimal | **Much better** |
| Reorg Handling | Manual | Automatic | **Native** |

## Monitoring

### ExEx Metrics

```bash
# Check ExEx status
curl http://localhost:9001/metrics | grep exex

# Key metrics:
# - exex_events_processed_total
# - exex_events_per_second
# - exex_blocks_behind_tip
```

### Python Consumer Metrics

```python
# In consumer.py
print(f"Events received: {total_events}")
print(f"Events/second: {events_per_second}")
print(f"Current block: {current_block}")
```

## Troubleshooting

### ExEx Won't Start

**Problem**: `Error: Failed to connect to Reth node`

**Solution**:
- Check Reth is running: `ps aux | grep reth`
- Verify IPC path: `ls /path/to/reth.ipc`
- Check permissions: `chmod 660 /path/to/reth.ipc`

### No Events Received

**Problem**: Python consumer connected but no events

**Solution**:
- Verify pools are tracked: Check `tracked_pools` config
- Confirm blocks are being mined: Check Reth sync status
- Enable debug logging: `export LOG_LEVEL=debug`

### Events Don't Match Etherscan

**Problem**: Event counts or data differs from Etherscan

**Solution**:
- Check event decoding: Compare ABI with Etherscan
- Verify block numbers: Ensure full sync
- Review logs for decode errors

## Development Roadmap

- [x] Phase 1: Project setup
- [ ] Phase 1: Minimal ExEx implementation
- [ ] Phase 2: gRPC streaming
- [ ] Phase 3: Database integration
- [ ] Phase 4: Production hardening
- [ ] Phase 5: Multi-protocol support (V4, Sushiswap, etc.)

## Resources

- [Reth ExEx Documentation](https://reth.rs/exex/overview)
- [Reth ExEx Examples](https://github.com/paradigmxyz/reth-exex-examples)
- [Paradigm Blog Post](https://www.paradigm.xyz/2024/05/reth-exex)
- [Alloy Documentation](https://alloy.rs/)

## Next Steps

**Immediate (Phase 1)**:
1. Review existing reth-exex-examples/remote
2. Implement basic ExEx in `src/main.rs`
3. Add Uniswap V3 ABI decoding
4. Test with single pool on testnet

**Coming Soon (Phase 2)**:
1. Add gRPC server implementation
2. Create Python consumer
3. Stream events in real-time

---

**Status**: 🏗️ Phase 1 - Project Structure Complete
**Next**: Implement minimal ExEx with event decoding
**Target**: Real-time liquidity tracking at 10-100x performance improvement
