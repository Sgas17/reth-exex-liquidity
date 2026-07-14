# reth-exex-liquidity

A Reth-based execution client binary that installs multiple ExExes into the node, with the main data path focused on low-latency pool state extraction for downstream consumers.

## What this binary does

This crate builds a single `exex` binary that launches a Reth node and installs:

- `Liquidity` — decodes whitelisted pool activity and emits normalized updates over a Unix socket
- `BalanceMonitor` — balance monitoring ExEx
- `PoolCreations` — pool creation monitoring ExEx
- `Transfers` — present in the codebase but currently not installed in `main.rs`

The important production path in this repo is:

- **control plane:** NATS whitelist updates
- **execution plane:** Reth `ExExNotification`s
- **data plane:** framed bincode messages over a Unix socket

---

## Actual runtime architecture

```text
                        NATS
     whitelist snapshot + incremental updates
                          │
                          ▼
                   ┌──────────────┐
                   │ PoolTracker  │
                   │ in-memory    │
                   │ whitelist    │
                   └──────┬───────┘
                          │
                          │ filters what to decode
                          │
┌───────────────────────────────────────────────────────────────┐
│                    reth node + installed ExExes              │
│                                                               │
│  ChainCommitted / ChainReorged / ChainReverted notifications  │
│                           │                                   │
│                           ▼                                   │
│                  ┌───────────────────┐                        │
│                  │ Liquidity ExEx    │                        │
│                  │ - log scan        │                        │
│                  │ - protocol decode │                        │
│                  │ - storage enrich  │                        │
│                  └─────────┬─────────┘                        │
└────────────────────────────┼──────────────────────────────────┘
                             │
                             ▼
               /tmp/reth_exex_pool_updates.sock
                             │
                             ▼
                downstream arena / orderbook engine
```

---

## Startup flow

The Liquidity ExEx uses a hard startup barrier before block processing starts.

```text
1. start Unix socket server
2. connect to NATS
3. subscribe to whitelist.pools.{chain}.minimal
4. request whitelist.snapshot.request.{chain}
5. require a non-empty full snapshot
6. load snapshot into PoolTracker
7. begin consuming Reth notifications
```

Sequence view:

```text
Liquidity ExEx          NATS                 PoolTracker            Reth notifications
     │                  │                         │                        │
     │ start socket     │                         │                        │
     │────────────────────────────────────────────────────────────────────>│
     │ connect          │                         │                        │
     │─────────────────>│                         │                        │
     │ subscribe        │                         │                        │
     │─────────────────>│                         │                        │
     │ snapshot request │                         │                        │
     │─────────────────>│                         │                        │
     │ full snapshot    │                         │                        │
     │<─────────────────│                         │                        │
     │ queue/apply full whitelist                 │                        │
     │───────────────────────────────────────────>│                        │
     │ ready                                                          start
     │────────────────────────────────────────────────────────────────────>│
```

If the startup snapshot is missing, malformed, or empty, the process keeps retrying. It does **not** silently proceed with an empty whitelist.

---

## Per-block flow

For committed blocks, the Liquidity ExEx processes notifications block-by-block.

```text
ChainCommitted
   │
   ├─ begin_block() on PoolTracker
   ├─ send BeginBlock
   ├─ iterate receipts/logs
   │   ├─ fast address filter
   │   ├─ protocol decode
   │   ├─ whitelist check by address or pool_id
   │   └─ send PoolUpdate for matching events
   ├─ Fluid only: decode touched pools from storage after log scan
   ├─ apply queued whitelist changes atomically at the block boundary
   │   ├─ remove dropped pools from the shared-arena topology
   │   └─ hydrate newly-added pools into the shared arena when possible
   └─ send EndBlock + signal the arena block
```

Block envelope:

```text
BeginBlock
  ├─ PoolUpdate
  ├─ PoolUpdate
  ├─ PoolUpdate
  └─ ...
EndBlock
```

This ordering is intentional: downstream consumers can treat each block as an atomic batch, and any reader that wakes on `EndBlock` or the arena block signal sees the post-block whitelist topology rather than stale active slots.

---

## Reorg flow

Reorgs are explicit in the socket protocol.

```text
ReorgStart
  ├─ old-chain blocks replayed as is_revert = true
  ├─ new-chain blocks replayed as normal updates
  ├─ ReorgEpilogue messages for canonical final state
  │    - slot0-style final state for V3/V4/Ekubo
  │    - final Fluid reserve state when needed
  └─ ReorgComplete
```

Sequence view:

```text
ChainReorged
   │
   ├─ send ReorgStart
   ├─ revert old blocks
   │    ├─ BeginBlock(is_revert=true)
   │    ├─ PoolUpdate(... is_revert=true)
   │    └─ EndBlock
   ├─ apply new blocks
   │    ├─ BeginBlock(is_revert=false)
   │    ├─ PoolUpdate(... is_revert=false)
   │    └─ EndBlock
   ├─ send ReorgEpilogue final-state corrections
   └─ send ReorgComplete
```

A downstream consumer should not guess reorg semantics from missing data; it should follow the explicit control messages.

---

## Protocol handling

### Direct event decode

- Uniswap V2
- Uniswap V3
- Uniswap V4
- Ekubo
- Curve Stable
- Curve TwoCrypto
- Curve Tricrypto
- Balancer V2 weighted

### Special singleton emitters

Some protocols emit from singleton contracts instead of pool addresses. `PoolTracker` automatically tracks those emitters so logs are not missed.

- Uniswap V4 PoolManager
- Ekubo Core
- Balancer V2 Vault
- Fluid Liquidity Layer

### Fluid handling

Fluid is handled differently from the other protocols.

```text
LogOperate observed on Liquidity Layer
   │
   ├─ extract touched pool address from indexed topic
   ├─ verify that pool is whitelisted as Fluid
   ├─ defer full decode until after the log loop
   ├─ read cached storage slots using FluidPoolConfig
   └─ emit full FluidState snapshot
```

That means Fluid updates are effectively **storage-derived snapshots triggered by logs**, not just ABI-decoded event payloads.

---

## Socket protocol

The Liquidity ExEx writes framed bincode messages to:

```text
/tmp/reth_exex_pool_updates.sock
```

Each frame is:

```text
[4-byte little-endian length][bincode payload]
```

Current control messages include:

- `BeginBlock`
- `PoolUpdate`
- `EndBlock`
- `ReorgStart`
- `ReorgEpilogue`
- `ReorgComplete`

Socket message envelope examples:

```text
Normal committed block

  [len][BeginBlock {
           stream_seq,
           block_number,
           block_timestamp,
           base_fee_per_gas,
           is_revert: false
         }]
  [len][PoolUpdate {
           stream_seq,
           event: PoolUpdateMessage {
             pool_id,
             protocol,
             update_type,
             block_number,
             block_timestamp,
             tx_index,
             log_index,
             is_revert: false,
             update: ...
           }
         }]
  [len][PoolUpdate {...}]
  [len][EndBlock {
           stream_seq,
           block_number,
           num_updates
         }]
```

```text
Reorg batch

  [len][ReorgStart {
           stream_seq,
           old_range,
           new_range
         }]

  [len][BeginBlock { is_revert: true,  ... }]
  [len][PoolUpdate  { event.is_revert: true,  ... }]
  [len][EndBlock    { ... }]

  [len][BeginBlock { is_revert: false, ... }]
  [len][PoolUpdate  { event.is_revert: false, ... }]
  [len][EndBlock    { ... }]

  [len][ReorgEpilogue {
           stream_seq,
           final_tip_block,
           final_tip_timestamp,
           update: Slot0Final | FluidStateFinal
         }]
  [len][ReorgComplete {
           stream_seq,
           final_tip_block
         }]
```

The consumer contract is simple:

- read 4-byte frame length
- decode one `ControlMessage` with bincode
- process messages strictly in stream order
- treat `BeginBlock ... EndBlock` as a block envelope
- treat `ReorgStart ... ReorgComplete` as a reorg envelope

Legacy v1 compatibility was removed. This repo uses a hard cutover model.

---

## Whitelist update model

The whitelist is maintained in memory by `PoolTracker`.

Supported update types:

- `Replace` — live full-snapshot replacement; computes add/remove topology deltas and refreshes retained metadata
- `Add` — incremental additions
- `Remove` — incremental removals

Startup uses a separate full-snapshot install path because the shared arena is reset and hydrated from scratch at the startup anchor.

Property that matters operationally:

- updates arriving during block processing are queued
- queued updates are applied at the block boundary before `EndBlock` and before the arena block signal
- whitelist membership therefore changes **between blocks**, not mid-block
- dropped pools are removed from the shared-arena topology before readers are notified for that block

This avoids inconsistent filtering inside a single block and prevents stale active slots for de-whitelisted pools.

---

## Repository map

```text
src/main.rs            entrypoint, ExEx installation, Liquidity flow
src/pool_tracker.rs    whitelist state + deferred update application
src/nats_client.rs     NATS subscription + snapshot handling
src/socket.rs          Unix socket server + framed broadcast
src/types.rs           wire protocol and update enums
src/events.rs          log decoding across supported protocols
src/fluid_decoder.rs   Fluid storage-based reserve decoding
src/balance_monitor/   balance monitor ExEx
src/pool_creations/    pool creation ExEx
src/transfers/         transfers ExEx implementation (not installed now)
REBUILD.md             rebuild + deploy instructions
docs/benchmarks.md     performance notes and benchmark guidance
```

---

## Build and run

Current dependency target:

- Reth `v2.4.0` (tag commit `943af245c4d69c6c1df241df016c278ffb5d15df`)
- Rust `1.95` (pinned by `rust-toolchain.toml`; Reth v2.4.0 declares `rust-version = "1.95"`)
- Alloy consensus `2.1.1`, `alloy-primitives`/`alloy-sol-types` `1.6.0` (kept aligned with Reth's Alloy 2 graph)
- `roaring` pinned by `Cargo.lock` to `0.11.4` for the Reth DB bitmap implementation

Reth v2.4.0 added `jit` (the experimental revmc JIT) and `gmp` to the `reth`
crate's default features. This build deliberately disables `jit` and keeps every
other default (see `Cargo.toml`). `gmp` needs `m4` at build time.

Build locally:

```bash
cargo build --release
```

Run as a Reth execution client binary:

```bash
./target/release/exex node [reth flags...]
```

For the actual deployment flow used with your environment, see:

- [`REBUILD.md`](REBUILD.md)

---

## Operational assumptions

This binary expects:

- a working Reth environment
- reachable NATS
- a valid startup whitelist snapshot
- a downstream consumer connected to the Unix socket if you want to use the liquidity feed

Useful environment variables:

- `NATS_URL` — defaults to `nats://localhost:4222`
- `CHAIN` — defaults to `ethereum`
- `RPC_URL` — used for resolving Fluid configs, defaults to `http://localhost:8545`

---

## Non-goals of this README

This README documents the **current** flow in the codebase. It does not describe the earlier gRPC/Python-consumer plan, because that is not the live architecture anymore.
