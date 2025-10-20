# Deploying the Liquidity ExEx to Your Reth Node

This guide explains how to add the ExEx to your Reth node and run it.

## Prerequisites

1. **Reth Node Running**: You need a Reth node (v1.8.2) running locally or remotely
2. **ExEx Binary Built**: The ExEx must be compiled successfully
3. **System Requirements**:
   - libclang-dev installed (`sudo apt install libclang-dev`)
   - Build tools installed (already have build-essential)

## Build Issues (Current)

The build is failing due to missing `libclang-dev`. To fix:

```bash
# Install the missing dependency
sudo apt install libclang-dev

# Retry the build
cd /home/sam-sullivan/reth-exex-liquidity
cargo build --release
```

## Deployment Methods

There are **3 ways** to deploy an ExEx to a Reth node:

### Method 1: Standalone Binary (Recommended for Production)

This method compiles the ExEx as a **standalone binary** that includes both the Reth node and your ExEx code.

**How it works:**
- The ExEx binary IS the Reth node
- When you run it, you get a full Reth node with your ExEx installed
- Use this for production deployments

**Build and Run:**

```bash
# Build the release binary
cd /home/sam-sullivan/reth-exex-liquidity
cargo build --release

# The binary will be at: target/release/exex

# Run it (starts a Reth node with ExEx installed)
./target/release/exex node \
  --chain mainnet \
  --datadir /path/to/reth/data \
  --http \
  --http.addr 0.0.0.0 \
  --http.port 8545 \
  --authrpc.addr 0.0.0.0 \
  --authrpc.port 8551 \
  --authrpc.jwtsecret /path/to/jwt.hex
```

**What happens:**
1. Reth node starts syncing/reading the chain
2. Your ExEx receives notifications for every block
3. Events are decoded and logged (Phase 1)
4. Later: Events streamed via gRPC (Phase 2)

### Method 2: Remote ExEx (For Development/Testing)

This method keeps the ExEx separate from the Reth node and connects over a socket.

**How it works:**
- Reth node runs separately
- ExEx runs as a separate process
- They communicate via Unix socket or network socket
- Use this for development/testing

**Setup:**

```bash
# 1. Start Reth node with remote ExEx support
reth node \
  --chain mainnet \
  --datadir /path/to/reth/data \
  --exex.remote \
  --exex.remote.path /tmp/exex.sock

# 2. In another terminal, run your ExEx
cd /home/sam-sullivan/reth-exex-liquidity
cargo run --release -- \
  --remote \
  --socket /tmp/exex.sock
```

**Pros:**
- Restart ExEx without restarting Reth
- Easier debugging (separate logs)
- Good for iterating during development

**Cons:**
- More complex setup
- Requires socket configuration

### Method 3: Dynamic Loading (Advanced)

This method compiles the ExEx as a **shared library** (.so file) that Reth loads at runtime.

**How it works:**
- ExEx compiled as dynamic library
- Reth loads it from a directory
- Can load multiple ExExs

**Setup:**

```bash
# 1. Modify Cargo.toml to build a dynamic library
# Add this to Cargo.toml:
[lib]
crate-type = ["cdylib"]

# 2. Build the library
cargo build --release

# 3. Copy to ExEx directory
mkdir -p /path/to/exex-dir
cp target/release/libreth_exex_liquidity.so /path/to/exex-dir/

# 4. Run Reth with ExEx directory
reth node \
  --chain mainnet \
  --datadir /path/to/reth/data \
  --exex.dir /path/to/exex-dir
```

## Recommended Approach

For this project, **Method 1 (Standalone Binary)** is recommended:

### Why?

1. **Simplicity**: Single binary, no complex configuration
2. **Performance**: No IPC overhead
3. **Production Ready**: How official examples are deployed
4. **Easier Deployment**: Just copy the binary and run

### Our Current Setup

Our [src/main.rs](src/main.rs) is already configured for Method 1:

```rust
fn main() -> eyre::Result<()> {
    reth::cli::Cli::parse_args().run(|builder, _| async move {
        let handle = builder
            .node(EthereumNode::default())
            .install_exex("Liquidity", liquidity_exex)  // ← ExEx installed here
            .launch()
            .await?;

        handle.wait_for_node_exit().await
    })
}
```

This creates a **complete Reth node** with the Liquidity ExEx installed.

## Configuration Options

When running the ExEx binary, you have all standard Reth options:

### Essential Options

```bash
./target/release/exex node \
  --chain mainnet \                    # Chain to sync
  --datadir /mnt/nvme/reth \          # Where to store blockchain data
  --http \                             # Enable HTTP RPC
  --http.addr 0.0.0.0 \               # RPC listen address
  --http.port 8545 \                   # RPC port
  --authrpc.addr 0.0.0.0 \            # Engine API address
  --authrpc.port 8551 \                # Engine API port
  --authrpc.jwtsecret /path/jwt.hex    # JWT secret for Engine API
```

### Syncing Options

```bash
# Full archive node (recommended for ExEx)
--full

# Start from specific block (for testing)
--debug.tip <block-hash>

# Pruning (NOT recommended for ExExs)
# ExExs need full history, so use --full
```

### Performance Options

```bash
# Increase database cache (if you have RAM)
--db.max-read-concurrent 512
--db.max-write-concurrent 512

# Network options
--peers.max-outbound 100
--peers.max-inbound 30
```

## Monitoring Your ExEx

### Logs

The ExEx will log to stdout/stderr. Our Phase 1 logs look like:

```
INFO Liquidity ExEx started
INFO Tracking 4 pools
INFO   - 0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640
INFO Processing committed chain with 1 blocks
INFO 🟢 MINT | Block 12345678 | Pool 0x88e6... | Owner 0xabc... | Ticks [-887220, -887210] | Amount 123456 | Amount0 1000 | Amount1 2000
INFO 📊 Block 12345678 summary: 5 Mints, 3 Burns (timestamp: 1234567890)
```

### Filtering Logs

```bash
# Show only ExEx logs
./target/release/exex node ... 2>&1 | grep -E "MINT|BURN|Liquidity"

# Save to file
./target/release/exex node ... 2>&1 | tee exex.log
```

### Systemd Service (Production)

Create `/etc/systemd/system/reth-exex.service`:

```ini
[Unit]
Description=Reth Node with Liquidity ExEx
After=network.target

[Service]
Type=simple
User=reth
WorkingDirectory=/home/reth
ExecStart=/home/reth/reth-exex-liquidity/target/release/exex node \
  --chain mainnet \
  --datadir /mnt/nvme/reth \
  --http \
  --http.addr 0.0.0.0 \
  --http.port 8545 \
  --authrpc.addr 0.0.0.0 \
  --authrpc.port 8551 \
  --authrpc.jwtsecret /home/reth/jwt.hex \
  --full

Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable reth-exex
sudo systemctl start reth-exex

# View logs
sudo journalctl -u reth-exex -f
```

## Testing Before Production

### Test with Recent Blocks

```bash
# Find a recent block hash
cast block latest --rpc-url https://eth.llamarpc.com | grep hash

# Start from that block (syncs much faster)
./target/release/exex node \
  --chain mainnet \
  --datadir /tmp/reth-test \
  --debug.tip <block-hash> \
  --http
```

This will:
1. Start Reth from that block
2. Sync forward (minutes, not days)
3. Process recent liquidity events
4. Verify your ExEx works

### Monitor Performance

```bash
# CPU/Memory
htop

# Network
iftop

# Disk I/O
iotop

# ExEx-specific metrics (Phase 4)
# Will add Prometheus metrics
```

## Troubleshooting

### Issue: "ExEx is too slow"

**Symptom**: Reth pauses and waits for ExEx

**Cause**: ExEx takes too long to process notifications

**Fix**:
- Optimize event decoding (Phase 1: should be fast)
- Add batching (Phase 2)
- Profile with `flamegraph`

### Issue: "Events missing"

**Symptom**: Not all events appear in logs

**Cause**: Pool filtering is wrong

**Fix**:
- Check pool addresses (must be lowercase in HashSet)
- Verify events are actually in those pools
- Use `cast logs` to verify events exist on-chain

### Issue: "Build fails with mdbx error"

**Symptom**: Cannot build due to libmdbx-sys error

**Fix**:
```bash
sudo apt install libclang-dev
cargo clean
cargo build --release
```

## Next Steps After Deployment

Once Phase 1 is working:

1. **Phase 2: Add gRPC Streaming**
   - Stream events to Python
   - Integrate with existing storage layers

2. **Phase 3: Database Integration**
   - Replace parquet-based system
   - Validate against existing data

3. **Phase 4: Production Hardening**
   - Add proper reorg handling
   - Implement monitoring/alerting
   - Load pools from database

## Resources

- [Reth CLI Documentation](https://reth.rs/cli/reth/)
- [ExEx Examples](https://github.com/paradigmxyz/reth-exex-examples)
- [Systemd Guide](https://www.freedesktop.org/software/systemd/man/systemd.service.html)

---

**Summary**: Build the binary, run it like a normal Reth node. The ExEx is embedded and automatically processes every block.
