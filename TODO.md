# TODO - Next Steps

## Immediate (Next Session)

### 1. poolStateArena Integration 🎯
**Priority: HIGH**

Implement the consumer side that receives messages from ExEx:

- [ ] Create block buffering system
  - Buffer messages between BeginBlock and EndBlock
  - Validate `num_updates` matches received updates
  - Handle out-of-order messages gracefully

- [ ] Implement snapshot cache (in-memory ring buffer)
  - Store only swap state (52 bytes per pool)
  - 128 block depth (~25 min history)
  - Efficient lookup by (pool_id, block_number)

- [ ] Build revert handler
  - Restore swap states from snapshots
  - Invert mint/burn operations
  - Handle missing snapshots (deep reorg fallback)

- [ ] Add path recalculation trigger
  - After each EndBlock message
  - Only for affected pools
  - Update orderbook engine

**Estimated Time:** 4-6 hours

---

### 2. Testing & Validation 🧪
**Priority: HIGH**

- [ ] Unit tests for snapshot cache
  - Add/retrieve snapshots
  - Ring buffer wraparound
  - Memory limits

- [ ] Integration tests for reorg handling
  - Simulate 1-3 block reorgs
  - Verify pool states match expected
  - Test with real swap data

- [ ] End-to-end test with live Reth
  - Deploy to testnet (Sepolia recommended)
  - Monitor for natural reorgs
  - Verify state consistency

**Estimated Time:** 3-4 hours

---

## Near Term (This Week)

### 3. Performance Optimization ⚡
**Priority: MEDIUM**

- [ ] Profile memory usage with 1000 pools
- [ ] Benchmark snapshot operations
- [ ] Optimize pool state serialization
- [ ] Add metrics/monitoring
  - Reorg frequency
  - Snapshot hit rate
  - Processing latency per block

**Estimated Time:** 2-3 hours

---

### 4. V2 Reserve Handling 📊
**Priority: MEDIUM**

Current implementation sends deltas, not actual reserves. Options:

- [ ] **Option A:** Query reserves from poolStateArena on swap events
- [ ] **Option B:** Track reserves in ExEx (requires state queries)
- [ ] **Option C:** poolStateArena reconstructs from Sync events

**Decision needed:** Which approach fits your architecture?

**Estimated Time:** 2-4 hours (depending on approach)

---

### 5. Error Handling & Recovery 🛡️
**Priority: MEDIUM**

- [ ] Handle Unix socket disconnections
- [ ] Implement message retry logic
- [ ] Add health checks
  - NATS connection
  - Socket connection
  - Pool whitelist age
- [ ] Graceful shutdown on errors

**Estimated Time:** 2-3 hours

---

## Future (Next Week+)

### 6. Deep Reorg Fallback 🔄
**Priority: LOW**

When reorg exceeds 128 block buffer:

- [ ] Detect missing snapshots
- [ ] Trigger blockchain resync for affected pools
- [ ] Pause trading until state is consistent
- [ ] Alert monitoring system

**Estimated Time:** 3-4 hours

---

### 7. Advanced Features 🚀
**Priority: LOW**

- [ ] **State Compression:** Use delta encoding for snapshots
- [ ] **Disk Spillover:** Move old snapshots to RocksDB
- [ ] **Multi-chain Support:** Handle multiple chains in parallel
- [ ] **State Verification:** Periodic checks against on-chain state

**Estimated Time:** 8-12 hours total

---

### 8. Monitoring & Observability 📈
**Priority: LOW**

- [ ] Prometheus metrics export
- [ ] Grafana dashboards
  - Block processing rate
  - Reorg frequency/depth
  - Snapshot cache stats
  - Pool update latency
- [ ] Alert rules for anomalies

**Estimated Time:** 4-6 hours

---

## Technical Debt

### Code Quality
- [ ] Remove unused methods (warnings in build)
  - `is_tracked_pool_id`
  - `get_by_address`
  - `as_address` / `as_pool_id`
  - `MessageBroadcaster` struct

- [ ] Add more inline documentation
- [ ] Create architecture diagram
- [ ] Write contribution guide

---

## Questions to Resolve

1. **V2 Reserves:** How should we handle V2 pool reserves? (See #4 above)

2. **Snapshot Storage:** Is 128 blocks enough? Or should we implement disk spillover sooner?

3. **Testing Strategy:** Do you have access to a testnet node, or should we use mainnet with careful rollout?

4. **Monitoring:** What monitoring infrastructure do you already have? (Prometheus, Datadog, custom?)

5. **Whitelist Updates:** How often does dynamicWhitelist publish? Should we add debouncing?

---

## Blockers

**None currently** ✅

All dependencies are working:
- ✅ Reth ExEx compiling and functional
- ✅ NATS integration tested
- ✅ Reorg handling implemented
- ✅ Block batching implemented

---

## Notes from Today's Session

- Discovered that swap events can't be individually reverted (need previous state)
- Solution: Block-level snapshots on consumer side (poolStateArena)
- Only need 52 bytes per pool per block for swap state
- With ~1000 watched pools, snapshot cache is only 200 KB - 1.3 MB
- BeginBlock/EndBlock messages ensure poolStateArena has complete blocks before recalculating paths
- Memory usage is very manageable even with deep reorg protection

---

## Quick Wins (Can do anytime)

- [ ] Clean up compiler warnings
- [ ] Add example poolStateArena skeleton
- [ ] Create sequence diagram for message flow
- [ ] Write poolStateArena API documentation

---

**Next Session Focus:** Start implementing poolStateArena block buffering and snapshot cache (Task #1)
