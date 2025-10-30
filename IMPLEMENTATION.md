# Reth ExEx Liquidity Tracker - Implementation Notes

## Critical Fix: install_exex Pattern

After extensive debugging, we discovered the correct pattern for `install_exex` with Reth v1.8.2:

### ❌ WRONG (causes "() is not a future" error):
```rust
.install_exex("Liquidity", liquidity_exex)
// or
.install_exex("Liquidity", |ctx| async move { liquidity_exex(ctx).await })
```

### ✅ CORRECT:
```rust
.install_exex("Liquidity", async move |ctx| Ok(liquidity_exex(ctx)))
```

The key insight: `install_exex` expects a closure that returns `Ok(Future)`, NOT a closure that awaits the future.

## Architecture

### Components

1. **NATS Client** (`src/nats_client.rs`)
   - Subscribes to `whitelist.pools.{chain}.>` subjects
   - Receives JSON messages from dynamicWhitelist service
   - Parses pool metadata (address, tokens, protocol, fee, tick_spacing)
   - Updates pool tracker dynamically

2. **Pool Tracker** (`src/pool_tracker.rs`)
   - Thread-safe storage of tracked pools (Arc<RwLock<PoolTracker>>)
   - Supports V2 (Address), V3 (Address), and V4 (bytes32 poolId)
   - Can be updated at runtime via NATS messages

3. **Event Decoder** (`src/events.rs`)
   - Decodes Uniswap V2/V3/V4 events using alloy-sol-types
   - V2: Swap, Mint, Burn (reserves)
   - V3: Swap, Mint, Burn (liquidity + ticks)
   - V4: Swap, ModifyLiquidity (poolId + liquidity + ticks)

4. **Unix Socket Server** (`src/socket.rs`)
   - High-performance IPC (1-5μs latency)
   - Binary serialization with bincode
   - Sends pool updates to orderbook engine

5. **Main ExEx** (`src/main.rs`)
   - Subscribes to Reth block notifications
   - Processes committed chains
   - Decodes events from tracked pools
   - Sends updates via Unix socket

## NATS Integration

### Message Format
```json
{
  "pools": [
    {
      "address": "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
      "token0": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
      "token1": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
      "protocol": "UniswapV3",
      "factory": "0x1F98431c8aD98523631AE4a59f267346ea31F984",
      "tick_spacing": 10,
      "fee": 500
    }
  ],
  "chain": "ethereum",
  "timestamp": "2025-10-30T14:30:00Z"
}
```

### Environment Variables
- `NATS_URL`: NATS server URL (default: `nats://localhost:4222`)
- `CHAIN`: Chain name for subject filtering (default: `ethereum`)

## Unix Socket Protocol

### Socket Path
- `/tmp/reth_exex_liquidity.sock`

### Message Format (Bincode-serialized)
```rust
enum ControlMessage {
    PoolUpdate(PoolUpdateMessage),
    Shutdown,
}

struct PoolUpdateMessage {
    pool_id: PoolIdentifier,  // Address or bytes32
    protocol: Protocol,        // V2, V3, or V4
    update_type: UpdateType,   // Swap, Mint, Burn, ModifyLiquidity
    block_number: u64,
    block_timestamp: u64,
    tx_index: u64,
    log_index: u64,
    update: PoolUpdate,        // V2Reserves, V3Liquidity, or V4Liquidity
}
```

## Key Dependencies

- `reth v1.8.2` - Ethereum node framework
- `alloy-consensus v1.0.37` - Provides BlockHeader and TxReceipt traits
- `alloy-sol-types v1.3.1` - Event decoding with sol! macro
- `async-nats v0.37` - NATS messaging client
- `tokio` - Async runtime
- `bincode` - Binary serialization for Unix socket

## Building and Running

```bash
# Check compilation
cargo check --bin exex

# Build release binary
cargo build --bin exex --release

# Run (requires Reth node configuration)
NATS_URL=nats://localhost:4222 CHAIN=ethereum ./target/release/exex
```

## Testing

### Test NATS Publisher
```bash
cargo run --example test_nats_publisher
```

### Mock Unix Socket Consumer
```bash
# TODO: Create mock consumer to receive and display pool updates
```

## Next Steps

1. ✅ NATS integration complete
2. ✅ Unix socket server implemented
3. ✅ Event decoding for V2/V3/V4
4. ⏳ Test with live Reth node
5. ⏳ Create mock Unix socket consumer for testing
6. ⏳ Performance profiling and optimization
7. ⏳ Error handling and recovery strategies
