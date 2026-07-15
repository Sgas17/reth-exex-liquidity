# Rebuilding and Deploying the ExEx

## Current target

This deployment currently targets **Reth `v2.4.0`** (tag commit
`943af245c4d69c6c1df241df016c278ffb5d15df`) on **Rust `1.95`**. Keep all `reth*`
git dependencies on the same tag and keep direct Alloy dependencies aligned with
Reth's Alloy 2 dependency graph (`alloy-consensus` `2.1.1`, `alloy-primitives` /
`alloy-sol-types` `1.6.0`).

Two v2.4.0 changes affect how this binary is built:

- The `reth` crate's default features gained **`jit`** (the experimental revmc
  JIT) and **`gmp`**. `jit` is deliberately **disabled** here — revmc needs LLVM
  dev headers to compile and is not part of this baseline. Cargo cannot subtract
  one default feature, so `Cargo.toml` sets `default-features = false` and
  re-lists the remaining defaults (plus `reth-revm/portable` via a direct
  `reth-revm` dependency).
- `gmp` pulls in `gmp-mpfr-sys`, which requires **`m4`** at build time. The build
  container package list below includes it.

Rust `1.95` is pinned by `rust-toolchain.toml`, so a rustup-based build container
installs the right toolchain automatically.

## Overview

The ExEx binary is bind-mounted from the host filesystem into the Reth container:
- **Host path:** `/home/sam-sullivan/reth-exex-liquidity/target/release/exex`
- **Production container path:** `/usr/local/bin/reth`

The build recipes stage container-owned artifacts under `target-user/`, then
atomically promote the completed binary to the exact host path mounted by the
live override. A failed build leaves the deployed binary untouched.

The binary runs ExExes in a single Reth node. The production deployment currently installs:
- **Liquidity** — Decodes Uniswap V2/V3/V4 Swap/Mint/Burn events from whitelisted pools, sends updates via Unix socket
- **BalanceMonitor** — Publishes executor token balance snapshots to NATS

Additional ExEx modules are still present in the repository but are not installed by the current `main.rs` deployment path.

## Build Requirements

The ExEx must be built in Ubuntu 22.04 to match Reth's GLIBC version (2.35). Building on a newer system will cause `GLIBC_2.38/2.39 not found` errors.

**Required packages in the build container:**
- `build-essential` - C compiler and basic build tools
- `pkg-config` - Package configuration tool
- `libssl-dev` - OpenSSL development headers
- `libclang-dev` - Required by bindgen for MDBX bindings
- `m4` - Required by `gmp-mpfr-sys` (Reth v2.4.0 `gmp` default feature). Missing
  `m4` fails the build with `configure: error: No usable m4 in $PATH`
- `git` - For fetching Reth from GitHub
- `curl` - For installing Rust

## Step-by-Step Process

The owned deployment wrapper lives in `defi_arb_rust`; it pins the upstream
eth-docker revision and refuses dirty vendor checkouts. Complete the migration
in `deployment/eth-docker/README.md` before this cutover.

### 1. Preflight and preserve rollback artifacts

Do this before any build can replace the mounted binary:

```bash
cd /home/sam-sullivan/defi_arb_rust
DEPLOY=deployment/eth-docker/compose.sh
$DEPLOY preflight
deployment/eth-docker/validate-config.sh

cd /home/sam-sullivan/reth-exex-liquidity
umask 077
ROLLBACK_ROOT="${HOME}/rollback/ITE-54"
ROLLBACK_DIR="${ROLLBACK_ROOT}/$(date -u +%Y%m%dT%H%M%SZ)"
install -d -m 0700 "$ROLLBACK_DIR"
cp -a target/release/exex "$ROLLBACK_DIR/exex.v2.3.0.rollback"
chmod 0755 "$ROLLBACK_DIR/exex.v2.3.0.rollback"
sha256sum "$ROLLBACK_DIR/exex.v2.3.0.rollback" \
  > "$ROLLBACK_DIR/exex.v2.3.0.sha256"
{
  target/release/exex --version
  stat target/release/exex
  printf 'checkout_head_at_capture=%s\n' "$(git rev-parse HEAD)"
} > "$ROLLBACK_DIR/exex.provenance.txt"
sha256sum "$ROLLBACK_DIR/exex.provenance.txt" \
  > "$ROLLBACK_DIR/exex.provenance.sha256"

install -m 0600 /etc/itrcap/eth-docker.env \
  "$ROLLBACK_DIR/eth-docker.env.pre-v2.4.0"
sha256sum "$ROLLBACK_DIR/eth-docker.env.pre-v2.4.0" \
  > "$ROLLBACK_DIR/eth-docker.env.pre-v2.4.0.sha256"
install -m 0600 /etc/itrcap/eth-docker.env \
  /etc/itrcap/eth-docker.env.pre-v2.4.0
docker image inspect reth:local >/dev/null
docker tag reth:local reth:ite54-pre-v2.4.0
docker image inspect --format '{{.Id}}' reth:ite54-pre-v2.4.0 \
  > "$ROLLBACK_DIR/reth-image-pre-v2.4.0.id"
ln -sfn "$(basename "$ROLLBACK_DIR")" "$ROLLBACK_ROOT/latest"
just verify-rollback
```

Do not continue unless `just verify-rollback` confirms the binary, protected
env, image tag/ID, checksum, and provenance under the external rollback root.

### 2. Build and atomically promote the ExEx/Reth binary

Build in Ubuntu 22.04 for the production GLIBC baseline. The build is isolated
under `target-user`; only a successful build is promoted to the mounted path.
The coupled service crates are mounted from a temporary `git archive HEAD`
snapshot so untracked runtime configuration is never visible in the container.

```bash
cd /home/sam-sullivan/reth-exex-liquidity
just build-exex
target/release/exex --version
```

The version output must identify Reth 2.4.0 and tag commit
`943af245c4d69c6c1df241df016c278ffb5d15df` before restart.

### 3. Build the pinned base image and recreate execution

```bash
cd /home/sam-sullivan/reth-exex-liquidity
just build-deployment-image
just restart-execution
```

The recipes and deployment wrapper both verify the external rollback bundle
before replacing the binary, image, or execution container. The wrapper uses the
reviewed Reth and eth-docker pins from `deployment/eth-docker/versions.env`; it
does not edit the upstream checkout.

### 4. Verify the deployment

```bash
cd /home/sam-sullivan/defi_arb_rust
deployment/eth-docker/compose.sh logs execution --tail 200 \
  | grep -iE 'reth version|Liquidity ExEx starting|Balance Monitor ExEx starting|NATS connected successfully|connected_peers='

curl -s -X POST -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","method":"web3_clientVersion","params":[],"id":1}' \
  http://localhost:8545 | jq -r .result

node <<'JS'
const ws = new WebSocket('ws://127.0.0.1:8546');
const timer = setTimeout(() => process.exit(1), 10_000);
ws.onopen = () => ws.send(JSON.stringify({
  jsonrpc: '2.0', method: 'web3_clientVersion', params: [], id: 1,
}));
ws.onmessage = ({ data }) => {
  const response = JSON.parse(data);
  console.log(response.result);
  clearTimeout(timer);
  ws.close();
  if (!response.result?.startsWith('reth/v2.4.0-')) process.exitCode = 1;
};
ws.onerror = () => process.exit(1);
JS
```

Then confirm Engine/consensus health, peer sync, ExEx socket events, shared-arena
advancement, `arena_verifier`, whitelist publication/direct DB reads, and the
`backtest_scraper`, `evm_execution`, `quoter`, and `hedger` smoke checks.

## Rollback

Reth v2.4.0's release notes do not describe a storage migration. Restore the
complete preserved deployment state and recreate execution. Do not modify or
downgrade the datadir.

```bash
cd /home/sam-sullivan/reth-exex-liquidity
ROLLBACK_DIR="${HOME}/rollback/ITE-54/latest"
just verify-rollback
install -m 0755 "$ROLLBACK_DIR/exex.v2.3.0.rollback" \
  target/release/.exex.rollback-tmp
mv -f target/release/.exex.rollback-tmp target/release/exex

install -m 0600 "$ROLLBACK_DIR/eth-docker.env.pre-v2.4.0" \
  /etc/itrcap/eth-docker.env
docker tag reth:ite54-pre-v2.4.0 reth:local

cd /home/sam-sullivan/defi_arb_rust
ETH_DOCKER_APPLY=1 deployment/eth-docker/compose.sh apply-execution
deployment/eth-docker/compose.sh logs execution --tail 200 \
  | grep -iE 'reth version|connected_peers'
```

Confirm `web3_clientVersion`, Engine/consensus health, ExEx socket traffic, arena
advancement, and `arena_verifier` before declaring rollback complete. If the
restored node refuses to start on the existing datadir, stop and escalate rather
than deleting or re-syncing the datadir.

## Environment Variables

| Variable | Used by | Default |
|---|---|---|
| `NATS_URL` | Liquidity | tracked in `deployment/eth-docker/versions.env` |
| `SOCKET_PROTOCOL` | Liquidity | tracked in `deployment/eth-docker/versions.env` |
| `DATABASE_URL` | Transfers, PoolCreations | required in `/etc/itrcap/eth-docker.env` |
| `BALANCE_MONITOR_ADDRESS` | Balance monitor | required in `/etc/itrcap/eth-docker.env` |

## Updating Reth Version

When Ethereum hard forks occur, you may see **fork ID mismatch** errors and `connected_peers=0`. This means the node's chain specs are outdated.

### Diagnosing Fork ID Mismatch

Enable debug logging to confirm:
```bash
# In /etc/itrcap/eth-docker.env, set LOG_LEVEL=debug, then:
cd /home/sam-sullivan/defi_arb_rust
ETH_DOCKER_APPLY=1 deployment/eth-docker/compose.sh apply-execution
deployment/eth-docker/compose.sh logs execution | grep "fork id mismatch"
```

### Updating Reth

1. Check latest Reth version:
   ```bash
   curl -s https://api.github.com/repos/paradigmxyz/reth/releases/latest | grep tag_name
   ```

2. Update `Cargo.toml` - change all Reth dependencies to the new version:
   ```toml
   reth = { git = "https://github.com/paradigmxyz/reth", tag = "vX.Y.Z" }
   reth-exex = { git = "https://github.com/paradigmxyz/reth", tag = "vX.Y.Z", features = ["serde"] }
   # ... update all reth-* dependencies to the same tag
   ```

   If the Reth update moves to a new Alloy major, update direct `alloy-*` dependencies to match Reth's dependency graph before rebuilding.

3. Clean and rebuild:
   ```bash
   # Clean old artifacts (may need sudo if owned by root from previous Docker builds)
   sudo rm -rf target-user/release

   # Rebuild using the Docker command above
   ```

4. Rebuild and recreate execution through the guarded recipes after preserving rollback artifacts:
   ```bash
   cd /home/sam-sullivan/reth-exex-liquidity
   just build-deployment-image
   just restart-execution
   ```

## One-Line Build Command (build only)

This intentionally does not restart the node. Complete the rollback-artifact
steps above before recreating the execution container.

```bash
cd /home/sam-sullivan/reth-exex-liquidity && just build-exex
```

## Justfile Automation

A `justfile` is available in the repo root to make cutovers repeatable through
the owned deployment wrapper. Preserve rollback artifacts first.

```bash
# Show latest upstream release metadata
just reth-latest

# Full cutover after preserving the rollback artifacts documented above
just deploy v2.4.0

# Rebuild + restart without changing version
just rebuild-and-restart
```

Available recipes:
- `just set-reth-version vX.Y.Z`
- `just verify-rollback`
- `just build-exex`
- `just build-deployment-image`
- `just restart-execution`
- `just verify-websocket`
- `just verify-exex`
- `just deploy vX.Y.Z`

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
sudo rm -rf /home/sam-sullivan/reth-exex-liquidity/target-user/release
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
