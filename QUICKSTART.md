# Quick Start Guide

## What We've Built

A Reth Execution Extension (ExEx) that:
1. **Receives pool whitelist updates** from dynamicWhitelist via NATS
2. **Decodes Uniswap V2/V3/V4 events** from tracked pools in real-time
3. **Sends pool state updates** to orderbook engine via Unix Domain Socket

## Critical Discovery

After extensive debugging, we found the correct pattern for `install_exex` with Reth v1.8.2:

```rust
// ✅ CORRECT
.install_exex("Liquidity", async move |ctx| Ok(liquidity_exex(ctx)))

// ❌ WRONG (causes "() is not a future" error)
.install_exex("Liquidity", liquidity_exex)
```

## Build Status

✅ **Compilation successful!**
- Binary: `target/release/exex` (86MB)
- All modules implemented and integrated
- NATS integration complete
- Unix socket server ready

## Architecture Overview

```
┌─────────────────┐         NATS          ┌──────────────────┐
│ dynamicWhitelist├────────────────────────>│  Reth ExEx       │
│  (Python)       │  Pool whitelist updates│  (Rust)          │
└─────────────────┘                        │                  │
                                           │  - NATS Client   │
                                           │  - Pool Tracker  │
┌─────────────────┐         Reth           │  - Event Decoder │
│  Ethereum       ├────────────────────────>│                  │
│  Blockchain     │   Block notifications  │                  │
└─────────────────┘                        └──────────────────┘
                                                     │
                                                     │ Unix Socket
                                                     │ (bincode)
                                                     ▼
                                           ┌──────────────────┐
                                           │  Orderbook Engine│
                                           │  (Consumer)      │
                                           └──────────────────┘
```

## Running the ExEx

### Prerequisites
1. **NATS Server** running on `localhost:4222` (or set `NATS_URL`)
2. **Reth Node** (the ExEx runs as part of Reth)

### Environment Variables
```bash
export NATS_URL=nats://localhost:4222  # Optional, defaults to localhost
export CHAIN=ethereum                   # Optional, defaults to ethereum
```

### Integration with Reth

The ExEx runs as part of your Reth node. Configure Reth to load the ExEx:

```bash
# Add to your Reth configuration or command line
reth node \
  --exex.liquidity /path/to/reth-exex-liquidity/target/release/exex \
  # ... other Reth flags
```

## NATS Integration

### Starting NATS Server
```bash
# Using Docker
docker run -p 4222:4222 -p 8222:8222 nats:latest

# Or install locally
# See https://docs.nats.io/running-a-nats-service/introduction/installation
```

### Test NATS Publisher
We've included a test publisher to simulate dynamicWhitelist:

```bash
cargo run --example test_nats_publisher
```

This will publish sample pool whitelist messages to NATS that the ExEx can receive.

## Unix Socket Consumer

The ExEx sends pool updates via Unix Domain Socket at `/tmp/reth_exex_liquidity.sock`.

### Message Format
Binary format using `bincode` serialization:

```rust
enum ControlMessage {
    PoolUpdate(PoolUpdateMessage),
    Shutdown,
}

struct PoolUpdateMessage {
    pool_id: PoolIdentifier,      // Address or bytes32 (for V4)
    protocol: Protocol,            // V2, V3, or V4
    update_type: UpdateType,       // Swap, Mint, Burn, ModifyLiquidity
    block_number: u64,
    block_timestamp: u64,
    tx_index: u64,
    log_index: u64,
    update: PoolUpdate,            // V2Reserves, V3Liquidity, or V4Liquidity
}
```

### Creating a Consumer

Example consumer in Python (using `msgpack` or similar):

```python
import socket
import bincode  # or use msgpack with schema

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect('/tmp/reth_exex_liquidity.sock')

while True:
    data = sock.recv(4096)
    if not data:
        break

    # Decode bincode message
    message = bincode.decode(data)
    print(f"Pool update: {message}")
```

Or in Rust:

```rust
use tokio::net::UnixStream;
use reth_exex_liquidity::types::ControlMessage;

let stream = UnixStream::connect("/tmp/reth_exex_liquidity.sock").await?;
let mut reader = BufReader::new(stream);

loop {
    let msg: ControlMessage = bincode::deserialize_from(&mut reader)?;
    match msg {
        ControlMessage::PoolUpdate(update) => {
            println!("Pool update: {:?}", update);
        }
        ControlMessage::Shutdown => break,
    }
}
```

## What's Implemented

### ✅ Complete
1. **NATS Client** - Subscribes to pool whitelist updates
2. **Pool Tracker** - Thread-safe pool storage with runtime updates
3. **Event Decoder** - Decodes V2/V3/V4 Swap/Mint/Burn events
4. **Unix Socket Server** - High-performance IPC with bincode serialization
5. **Main ExEx Logic** - Integrates all components
6. **Compilation** - Successfully builds with Reth v1.8.2

### ⏳ Pending
1. **Live Testing** - Test with actual Reth node
2. **Mock Consumer** - Create test consumer for Unix socket
3. **Performance Profiling** - Measure latency and throughput
4. **Error Recovery** - Handle NATS/socket disconnections gracefully

## Troubleshooting

### "() is not a future" error
Make sure you're using the correct `install_exex` pattern:
```rust
.install_exex("Liquidity", async move |ctx| Ok(liquidity_exex(ctx)))
```

### "no field 'number' on type RecoveredBlock"
Import the required traits:
```rust
use alloy_consensus::{BlockHeader, TxReceipt};
```

### NATS connection fails
Check that NATS server is running:
```bash
# Test connection
curl http://localhost:8222/varz
```

## Next Steps

1. **Deploy to production Reth node**
2. **Connect dynamicWhitelist NATS publisher**
3. **Create Unix socket consumer in orderbook engine**
4. **Monitor performance metrics**
5. **Add health checks and monitoring**

## Files Overview

- `src/main.rs` - Main ExEx logic and integration
- `src/nats_client.rs` - NATS subscription and message parsing
- `src/pool_tracker.rs` - Pool storage and management
- `src/events.rs` - Event type definitions and decoding
- `src/socket.rs` - Unix socket server
- `src/types.rs` - Shared data types
- `examples/test_nats_publisher.rs` - Test NATS message publisher

## Performance Targets

- **Block Processing**: < 100μs per block
- **Event Decoding**: < 10μs per event
- **Socket Latency**: 1-5μs (Unix domain socket)
- **NATS Latency**: < 1ms (local network)
- **Total E2E Latency**: < 10ms (blockchain event → orderbook engine)
