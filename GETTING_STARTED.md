# Getting Started with Reth ExEx Liquidity Tracker

## Step-by-Step Setup Guide

### Step 1: Verify Prerequisites

```bash
# Check Rust is installed
rustc --version  # Should be 1.70+

# Check Python is installed
python3 --version  # Should be 3.10+

# Check you have access to a Reth node
# (Either running locally or know the IPC endpoint)
```

### Step 2: Start with Reth ExEx Examples

Before building our custom ExEx, study the official examples:

```bash
# Clone the examples repository
cd /home/sam-sullivan
git clone https://github.com/paradigmxyz/reth-exex-examples.git
cd reth-exex-examples

# Look at the "remote" example (closest to our use case)
cd remote
cat README.md

# Try building it
cargo build

# Study the code structure
cat src/main.rs  # See how ExEx subscribes to notifications
cat src/server.rs  # See how gRPC server is set up
```

**Key Takeaways:**
- ExExs receive `ExExNotification` objects from Reth
- Notifications include `ChainCommitted`, `ChainReorged`, `ChainReverted`
- Each notification has full block data (transactions, receipts, logs)
- Use Alloy's `sol!` macro for type-safe event decoding

### Step 3: Understand the Liquidity Event Structure

Uniswap V3 emits two key events we care about:

**Mint Event** (when liquidity is added):
```solidity
event Mint(
    address indexed sender,
    address indexed owner,
    int24 indexed tickLower,
    int24 indexed tickUpper,
    uint128 amount,
    uint256 amount0,
    uint256 amount1
);
```

**Burn Event** (when liquidity is removed):
```solidity
event Burn(
    address indexed owner,
    int24 indexed tickLower,
    int24 indexed tickUpper,
    uint128 amount,
    uint256 amount0,
    uint256 amount1
);
```

### Step 4: Implement Minimal ExEx (Week 1 Goal)

Create a minimal ExEx that:
1. Subscribes to Reth notifications
2. Filters logs from known Uniswap pools
3. Decodes Mint/Burn events
4. Prints them to console (no gRPC yet, no database yet)

**File to create**: `src/main.rs`

**Template structure:**
```rust
use reth_exex::{ExExContext, ExExNotification};
use alloy::sol;

// Define Uniswap V3 events
sol! {
    interface IUniswapV3Pool {
        event Mint(...);
        event Burn(...);
    }
}

async fn main() {
    // Get ExEx context from Reth
    let mut ctx = ExExContext::new()?;

    // Main loop: receive notifications
    while let Some(notification) = ctx.notifications.recv().await {
        match notification {
            ExExNotification::ChainCommitted { new } => {
                // Process new blocks
                for block in new.blocks() {
                    for receipt in block.receipts() {
                        for log in receipt.logs() {
                            // Try to decode as Mint or Burn
                            if let Ok(mint) = IUniswapV3Pool::Mint::decode_log(&log) {
                                println!("Mint: {:?}", mint);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
```

**Test with**:
```bash
cargo build
# Run with Reth (exact command depends on your Reth setup)
```

**Success**: You see Mint/Burn events printed to console!

### Step 5: Add Pool Filtering (Week 1)

Hard-code a few high-volume Uniswap V3 pools to track:

```rust
const TRACKED_POOLS: &[&str] = &[
    "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640", // USDC/WETH 0.05%
    "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8", // USDC/WETH 0.3%
];

// In your loop:
if TRACKED_POOLS.contains(&log.address.to_string().as_str()) {
    // Process this event
}
```

**Test**: You only see events from your tracked pools (much less noise!)

### Step 6: Add gRPC Streaming (Week 2)

Add gRPC server to stream events to Python:

1. **Generate Rust gRPC code**:
```bash
cargo build  # build.rs will generate proto code
```

2. **Create gRPC server** (`src/grpc_server.rs`):
```rust
use tonic::{transport::Server, Request, Response, Status};

// Implement gRPC service
impl LiquidityEventStream for MyService {
    async fn subscribe(&self, request: Request<SubscribeRequest>)
        -> Result<Response<EventStream>, Status> {
        // Return stream of events
    }
}
```

3. **Start server in ExEx**:
```rust
tokio::spawn(async move {
    Server::builder()
        .add_service(LiquidityEventStreamServer::new(service))
        .serve(addr)
        .await
});
```

**Test**:
```bash
# In one terminal: Run ExEx
cargo run

# In another terminal: Test with grpcurl
grpcurl -plaintext localhost:10000 liquidity.LiquidityEventStream/Subscribe
```

**Success**: You see events streaming via gRPC!

### Step 7: Create Python Consumer (Week 2)

**Generate Python gRPC code**:
```bash
cd python-consumer
python -m grpc_tools.protoc -I../proto --python_out=. --grpc_python_out=. ../proto/liquidity.proto
```

**Create consumer** (`consumer.py`):
```python
import grpc
import liquidity_pb2
import liquidity_pb2_grpc

def main():
    channel = grpc.insecure_channel('localhost:10000')
    stub = liquidity_pb2_grpc.LiquidityEventStreamStub(channel)

    # Subscribe to events
    for notification in stub.Subscribe(liquidity_pb2.SubscribeRequest()):
        if notification.HasField('chain_committed'):
            for block in notification.chain_committed.new_chain.blocks:
                for event in block.events:
                    print(f"Event: {event.event_type} at block {block.number}")

if __name__ == '__main__':
    main()
```

**Test**:
```bash
python consumer.py
```

**Success**: Python receives events in real-time!

### Step 8: Connect to Database (Week 3)

Import storage functions from main project:

```python
import sys
sys.path.append('/home/sam-sullivan/dynamicWhitelist')

from src.core.storage.timescaledb_liquidity import store_liquidity_updates_batch

# In your consumer loop:
events_buffer = []
for event in stream:
    events_buffer.append({
        'pool_address': event.pool_address,
        'block_number': block.number,
        # ... other fields
    })

    if len(events_buffer) >= 1000:
        store_liquidity_updates_batch(events_buffer, chain_id=1)
        events_buffer.clear()
```

**Test**: Events appear in TimescaleDB!

### Step 9: Compare with Parquet System (Week 3)

Run both systems in parallel and compare:

```python
# Compare snapshots
exex_snapshot = load_liquidity_snapshot(pool_address, chain_id=1)
parquet_snapshot = load_from_legacy_system(pool_address)

assert exex_snapshot['tick_data'] == parquet_snapshot['tick_data']
```

**Success**: Data matches perfectly!

### Step 10: Production Deployment (Week 4+)

1. Handle reorgs properly
2. Add monitoring and alerting
3. Load pool list from database
4. Deploy alongside existing system
5. Gradually switch production traffic

## Quick Reference

### Useful Commands

```bash
# Build ExEx
cd /home/sam-sullivan/reth-exex-liquidity
cargo build --release

# Run tests
cargo test

# Generate Python protobuf code
cd python-consumer
python -m grpc_tools.protoc -I../proto --python_out=. --grpc_python_out=. ../proto/liquidity.proto

# Run Python consumer
python consumer.py

# Check ExEx is running
ps aux | grep exex

# View logs
tail -f /var/log/exex.log
```

### Helpful Resources

- **Reth Docs**: https://reth.rs/exex/overview
- **Alloy Docs**: https://alloy.rs/
- **gRPC Python**: https://grpc.io/docs/languages/python/quickstart/
- **Examples Repo**: https://github.com/paradigmxyz/reth-exex-examples

### Common Issues

**Q: Cargo build fails with dependency errors**
A: Update Reth dependencies - the project evolves fast
```bash
cargo update
```

**Q: ExEx won't connect to Reth**
A: Check Reth IPC path and permissions
```bash
ls -la /path/to/reth.ipc
```

**Q: No events coming through**
A: Verify Reth is synced and blocks are being produced
```bash
reth node status
```

## Next Steps

1. ✅ Review this guide
2. ⏳ Study `reth-exex-examples/remote`
3. ⏳ Implement minimal ExEx (print events)
4. ⏳ Add gRPC streaming
5. ⏳ Create Python consumer
6. ⏳ Connect to database
7. ⏳ Validate against parquet system
8. ⏳ Deploy to production

---

**Current Status**: Phase 1 - Ready to implement minimal ExEx
**Goal**: Real-time liquidity tracking at 10-100x performance improvement
**Timeline**: 4-5 weeks to production-ready system
