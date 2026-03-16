# ExEx Performance Benchmarks

Tracking latency of each stage in the ExEx → socket → arena pipeline.
When total per-block latency threatens competitiveness, this is the
reference for deciding what to cut.

> **Goal**: new block state available in the arena < 1 ms after ExEx
> receives the `ChainCommitted` notification.

---

## Pipeline Stages

```
ChainCommitted notification
  │
  ├─ 1. Log iteration & address filter
  ├─ 2. Event decoding (decode_log)
  ├─ 3. Pool update construction (create_pool_update)
  │     └─ [Fluid only] Storage slot read + reserve decode
  ├─ 4. Socket send (Unix socket → arena subscriber)
  ├─ 5. Arena write (shared memory update)
  └─ 6. Curve/path invalidation
```

---

## Measured

| Stage | Operation | Time | Notes |
|---|---|---|---|
| 3 | `decode_fluid_reserves` (8 U256 slots → reserves) | **~8 µs** | Criterion, pool 1 wstETH/ETH. BigMath + exchange price calc + 2× quadratic solver. Only called for pools that emitted `LogOperate`. |

## Unmeasured (TODO)

| Stage | Operation | Expected | Notes |
|---|---|---|---|
| 1 | Log iteration + address filter | < 1 µs/log | HashSet lookup on `log.address`. ~200-400 logs/block typical. |
| 2 | `decode_log` (topic match + ABI decode) | < 1 µs/event | alloy-sol-types generated decoder. Branch on first topic. |
| 3 | V2/V3/V4 `create_pool_update` | < 1 µs | Field extraction from decoded event, no math. |
| 3 | Fluid storage slot reads (reth state provider) | ? | `provider.storage(addr, slot)` × 8 per pool. Depends on reth's trie cache. Critical unknown. |
| 4 | Unix socket send (bincode serialize + write) | < 10 µs | Single `write_all` per message. Loopback, no syscall batching yet. |
| 5 | Arena shared-memory write | < 1 µs | Direct `AtomicU64` stores into mmap'd region. |
| 6 | Curve/path invalidation | < 1 µs/pool | Set dirty bit on affected paths. |

## Per-Protocol Cost Estimate

Rough per-block cost assuming typical event counts:

| Protocol | Events/block (p50) | Events/block (p99) | Decode cost | Total (p50) | Total (p99) |
|---|---|---|---|---|---|
| Uniswap V2 | 30 | 150 | ~0.5 µs | 15 µs | 75 µs |
| Uniswap V3 | 40 | 200 | ~0.5 µs | 20 µs | 100 µs |
| Uniswap V4 | 5 | 30 | ~0.5 µs | 2.5 µs | 15 µs |
| Fluid | 2 | 10 | ~8 µs + slot reads | 16 µs + ? | 80 µs + ? |

> **Fluid is ~16× more expensive per event** than V2/V3 due to the
> quadratic solver math, but event volume is low (2-10/block vs 30-200).
> The unknown is storage slot read latency from reth's state provider —
> if that's > 100 µs/slot, it dominates everything else.

## Optimization Levers

If latency becomes a problem, in priority order:

1. **Batch socket writes**: Coalesce all updates into a single
   `write_all` per block instead of per-event.
2. **Drop low-value protocols**: If a protocol's pools rarely produce
   profitable arbs, remove it from the whitelist to skip its events entirely.
3. **Parallel decode**: Fluid decodes are independent per pool —
   could use `rayon` if > 10 pools change in one block.
4. **SIMD / branchless BigMath**: The `from_big_number` and `isqrt` hot
   paths could be optimized, but at 8 µs total they're not the bottleneck.

> **Already baked in**:
> - Fluid pools are only decoded when they emit `LogOperate` —
>   unchanged pools cost zero.
> - Fluid `LogOperate` pre-filter: the Liquidity Layer emits
>   `LogOperate` for every protocol (fTokens, Vaults, DEX pools).
>   We check the indexed `user` topic (topics[1] = pool address)
>   against tracked pools *before* ABI decoding — pure byte comparison,
>   no deserialization. Only our ~44 DEX pools proceed to full decode.
> - Slot addresses cached in `FluidPoolConfig` at registration time.

---

## How to Run

```bash
# Fluid decoder benchmark
cargo bench --bench fluid_decoder

# Add new benchmarks in benches/ and register in Cargo.toml
```

## History

| Date | Change | Impact |
|---|---|---|
| 2026-03-13 | Initial `decode_fluid_reserves` benchmark | 8 µs baseline |
| 2026-03-13 | Fixed exchange price bit positions (was extracting wrong fields) | Correctness fix, no perf change |
