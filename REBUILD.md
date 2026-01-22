# Rebuilding and Deploying the ExEx

## Overview

The ExEx binary is bind-mounted from the host filesystem into the Reth container:
- **Host path:** `/home/sam-sullivan/reth-exex-liquidity/target/release/exex`
- **Container path:** `/usr/local/bin/reth-exex`

This means you only need to rebuild the binary and restart the container - no manual file copying required.

## Build Requirements

The ExEx must be built in Ubuntu 22.04 to match Reth's GLIBC version (2.35). Building on a newer system will cause `GLIBC_2.38/2.39 not found` errors.

**Required packages in the build container:**
- `build-essential` - C compiler and basic build tools
- `pkg-config` - Package configuration tool
- `libssl-dev` - OpenSSL development headers
- `libclang-dev` - Required by bindgen for MDBX bindings
- `git` - For fetching Reth from GitHub
- `curl` - For installing Rust

## Step-by-Step Process

### 1. Build the Binary

Build in an Ubuntu 22.04 container to ensure GLIBC compatibility:

```bash
cd /home/sam-sullivan/reth-exex-liquidity

docker run --rm --network host \
  -v /home/sam-sullivan/reth-exex-liquidity:/workspace \
  -w /workspace \
  ubuntu:22.04 \
  bash -c "
    apt-get update -qq &&
    apt-get install -y -qq curl build-essential pkg-config libssl-dev git libclang-dev &&
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y &&
    . \$HOME/.cargo/env &&
    cargo build --release
  "
```

**Notes:**
- `--network host` ensures the container can resolve DNS for crates.io and GitHub
- Build takes ~5-15 minutes depending on cached dependencies
- Binary created at: `/home/sam-sullivan/reth-exex-liquidity/target/release/exex`

### 2. Restart the Execution Container

```bash
cd /home/sam-sullivan/eth-docker && ./ethd restart execution
```

### 3. Verify the Deployment

Check the ExEx logs:

```bash
./ethd logs execution --tail 50 | grep -E "exex|ExEx|Liquidity|connected_peers"
```

Look for:
- `🚀 Liquidity ExEx starting`
- `✅ NATS connected successfully`
- `connected_peers=X` where X > 0

## Updating Reth Version

When Ethereum hard forks occur, you may see **fork ID mismatch** errors and `connected_peers=0`. This means the node's chain specs are outdated.

### Diagnosing Fork ID Mismatch

Enable debug logging to confirm:
```bash
# In eth-docker/.env, set:
LOG_LEVEL=debug

# Restart and check logs:
./ethd restart execution
./ethd logs execution | grep "fork id mismatch"
```

### Updating Reth

1. Check latest Reth version:
   ```bash
   curl -s https://api.github.com/repos/paradigmxyz/reth/releases/latest | grep tag_name
   ```

2. Update `Cargo.toml` - change all Reth dependencies to the new version:
   ```toml
   reth = { git = "https://github.com/paradigmxyz/reth", tag = "v1.9.3" }
   reth-exex = { git = "https://github.com/paradigmxyz/reth", tag = "v1.9.3", features = ["serde"] }
   # ... update all reth-* dependencies
   ```

3. Clean and rebuild:
   ```bash
   # Clean old artifacts (may need sudo if owned by root from previous Docker builds)
   sudo rm -rf target/release

   # Rebuild using the Docker command above
   ```

4. Restart execution client

## One-Line Build Command

```bash
cd /home/sam-sullivan/reth-exex-liquidity && docker run --rm --network host -v /home/sam-sullivan/reth-exex-liquidity:/workspace -w /workspace ubuntu:22.04 bash -c "apt-get update -qq && apt-get install -y -qq curl build-essential pkg-config libssl-dev git libclang-dev && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && . \$HOME/.cargo/env && cargo build --release" && cd /home/sam-sullivan/eth-docker && ./ethd restart execution
```

## Common Issues

### GLIBC Version Mismatch
**Error:** `version GLIBC_2.38 not found` or `GLIBC_2.39 not found`
**Cause:** Binary was built on host system instead of Ubuntu 22.04 container
**Solution:** Always build using the Docker command above

### Fork ID Mismatch (0 Peers)
**Error:** `fork id mismatch, removing peer` in debug logs, `connected_peers=0`
**Cause:** Reth version doesn't include latest hard fork chain specs
**Solution:** Update Reth version in Cargo.toml and rebuild (see "Updating Reth" above)

### Permission Denied / Target Directory Issues
**Error:** `failed to remove file` or `Is a directory` errors during build
**Cause:** Mixed ownership from host builds and Docker builds (root vs user)
**Solution:**
```bash
sudo rm -rf /home/sam-sullivan/reth-exex-liquidity/target/release
# Then rebuild
```

### libclang Not Found
**Error:** `Unable to find libclang` during build
**Cause:** Missing `libclang-dev` package
**Solution:** Ensure `libclang-dev` is in the apt-get install command

### DNS Resolution Failed in Docker
**Error:** `failed to resolve address for github.com`
**Cause:** Docker container network isolation
**Solution:** Add `--network host` to the docker run command

### Binary Not Updating
**Error:** Changes not reflected after restart
**Solution:** Check that build completed successfully and file timestamp updated:
```bash
ls -lh /home/sam-sullivan/reth-exex-liquidity/target/release/exex
```

### Container Won't Start
**Error:** Container crashes on startup
**Solution:** Check Reth logs for errors:
```bash
docker logs eth-docker-execution-1 --tail 50
```
