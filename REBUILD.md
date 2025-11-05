# Rebuilding and Deploying the ExEx

## Overview

The ExEx binary is bind-mounted from the host filesystem into the Reth container:
- **Host path:** `/home/sam-sullivan/reth-exex-liquidity/target/release/exex`
- **Container path:** `/usr/local/bin/reth-exex`

This means you only need to rebuild the binary and restart the container - no manual file copying required.

## Build Requirements

The ExEx must be built in Ubuntu 22.04 to match Reth's GLIBC version (2.35). Building on a newer system will cause `GLIBC_2.38/2.39 not found` errors.

## Step-by-Step Process

### 1. Build the Binary

Build in an Ubuntu 22.04 container to ensure GLIBC compatibility:

```bash
docker run --rm \
  -v /home/sam-sullivan/reth-exex-liquidity:/workspace \
  -w /workspace \
  ubuntu:22.04 \
  bash -c "
    apt-get update -qq &&
    apt-get install -y -qq curl build-essential > /dev/null 2>&1 &&
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y > /dev/null 2>&1 &&
    . \$HOME/.cargo/env &&
    cargo build --release
  "
```

This creates the binary at: `/home/sam-sullivan/reth-exex-liquidity/target/release/exex`

### 2. Restart the Execution Container

Restart the Reth container to load the updated binary:

```bash
cd /home/sam-sullivan/eth-docker && ./ethd restart execution
```

### 3. Send Whitelist Update

After the container restarts, send the pool whitelist:

```bash
python3 /home/sam-sullivan/reth-exex-liquidity/test_whitelist.py
```

## One-Line Command

Combine all steps into a single command:

```bash
docker run --rm -v /home/sam-sullivan/reth-exex-liquidity:/workspace -w /workspace ubuntu:22.04 bash -c "apt-get update -qq && apt-get install -y -qq curl build-essential > /dev/null 2>&1 && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y > /dev/null 2>&1 && . \$HOME/.cargo/env && cargo build --release" && cd /home/sam-sullivan/eth-docker && ./ethd restart execution && sleep 15 && python3 /home/sam-sullivan/reth-exex-liquidity/test_whitelist.py
```

## Verifying the Deployment

Check the ExEx logs to confirm it's running with your changes:

```bash
docker logs eth-docker-execution-1 --tail 100 | grep -E "exex|ExEx|Liquidity"
```

Look for:
- `🚀 Liquidity ExEx starting`
- `✅ NATS connected successfully`
- `📥 Received ADD update: +5 pools`
- `Whitelist now tracking: X V2, Y V3, Z V4 pools`
- `🔍 Block XXXXX: checked N logs, M matched address, K decoded, J events`

## Common Issues

### GLIBC Version Mismatch
**Error:** `version GLIBC_2.38 not found`
**Solution:** Always build in Ubuntu 22.04 container (not directly on host)

### Binary Not Updating
**Error:** Changes not reflected after restart
**Solution:** Check that the build completed successfully and the file timestamp updated:
```bash
ls -lh /home/sam-sullivan/reth-exex-liquidity/target/release/exex
```

### Container Won't Start
**Error:** Container crashes on startup
**Solution:** Check Reth logs for errors:
```bash
docker logs eth-docker-execution-1 --tail 50
```
