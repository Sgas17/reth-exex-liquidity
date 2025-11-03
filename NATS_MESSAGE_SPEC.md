# NATS Message Specification for Pool Whitelist

## Design Decision: Dual-Topic Architecture

We publish to **two separate topics** to optimize for different subscriber needs:

1. **Minimal topic** (`whitelist.pools.{chain}.minimal`) - For ExEx
2. **Full topic** (`whitelist.pools.{chain}.full`) - For poolStateArena

This allows:
- ExEx to receive small messages (3x smaller)
- poolStateArena to get full metadata without Redis lookup
- Each subscriber chooses what they need

## Message Size Comparison

| Format | Size/Pool | Max Pools @ 100KB | Max Pools @ 1MB |
|--------|-----------|-------------------|-----------------|
| Minimal (addresses only) | ~50 bytes | 2,000 | 20,000 |
| Full (with metadata) | ~350 bytes | 285 | 2,900 |

## Topic: `whitelist.pools.{chain}.minimal`

**Purpose**: Fast updates for event decoders (ExEx)
**Size**: ~50 bytes per pool (just addresses!)
**Subscribers**: ExEx

### Message Format

```json
{
  "pools": [
    "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
    "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed",
    "0x4e68ccd3e89f51c3074ca5072bbac773960dfa36"
  ],
  "chain": "ethereum",
  "timestamp": "2025-10-30T15:30:00Z"
}
```

**Why so minimal?**
- ExEx `decode_log()` tries ALL event types (V2/V3/V4) automatically
- No need to specify protocol - the event signature reveals it
- No need for factory - only used for external validation
- Just need to know: "Is this address whitelisted?"

### What ExEx Does With This

```rust
// ExEx only needs the addresses to filter logs
for pool_address in message.pools {
    pool_tracker.add_address(pool_address);
}

// When log arrives:
if pool_tracker.is_tracked(log.address) {
    // Try to decode - this automatically detects V2/V3/V4
    if let Some(event) = decode_log(log) {
        // Send to orderbook engine
        socket_tx.send(PoolUpdate::from(event));
    }
}
```

## Topic: `whitelist.pools.{chain}.full`

**Purpose**: Complete metadata for price normalization (poolStateArena)
**Size**: ~350 bytes per pool
**Subscribers**: poolStateArena, monitoring tools, analytics

### Message Format

```json
{
  "pools": [
    {
      "id": "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
      "token0": {
        "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        "decimals": 6,
        "symbol": "USDC",
        "name": "USD Coin"
      },
      "token1": {
        "address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        "decimals": 18,
        "symbol": "WETH",
        "name": "Wrapped Ether"
      },
      "protocol": "UniswapV3",
      "factory": "0x1F98431c8aD98523631AE4a59f267346ea31F984",
      "fee": 500,
      "tick_spacing": 10
    }
  ],
  "chain": "ethereum",
  "timestamp": "2025-10-30T15:30:00Z"
}
```

### What poolStateArena Does With This

```python
# poolStateArena needs token metadata to:
# 1. Normalize amounts (decimals)
# 2. Calculate prices
# 3. Track reserves in human-readable units

for pool in message.pools:
    arena.register_pool(
        pool_id=pool.id,
        token0=Token(pool.token0.address, pool.token0.decimals, pool.token0.symbol),
        token1=Token(pool.token1.address, pool.token1.decimals, pool.token1.symbol),
        protocol=pool.protocol,
        fee=pool.fee
    )
```

## Publishing Strategy (dynamicWhitelist)

```python
async def publish_whitelist_update(self, pools: List[PoolMetadata]):
    """
    Publish to both minimal and full topics.
    Cost: 2 NATS publishes, but messages are independent.
    """

    # Minimal message for ExEx (just addresses!)
    minimal = {
        "pools": [pool.address for pool in pools],
        "chain": self.chain,
        "timestamp": datetime.utcnow().isoformat()
    }

    # Full message for poolStateArena
    full = {
        "pools": [
            {
                "id": pool.address,
                "token0": {
                    "address": pool.token0.address,
                    "decimals": pool.token0.decimals,
                    "symbol": pool.token0.symbol,
                    "name": pool.token0.name
                },
                "token1": {
                    "address": pool.token1.address,
                    "decimals": pool.token1.decimals,
                    "symbol": pool.token1.symbol,
                    "name": pool.token1.name
                },
                "protocol": pool.protocol,
                "factory": pool.factory,
                "fee": pool.fee,
                "tick_spacing": pool.tick_spacing,
                "stable": pool.stable
            }
            for pool in pools
        ],
        "chain": self.chain,
        "timestamp": datetime.utcnow().isoformat()
    }

    # Publish to both topics in parallel
    await asyncio.gather(
        self.nats.publish(f"whitelist.pools.{self.chain}.minimal", json.dumps(minimal)),
        self.nats.publish(f"whitelist.pools.{self.chain}.full", json.dumps(full))
    )

    # Also update Redis for persistence
    await self._update_redis(pools)
```

## ExEx Subscription Update

```rust
// In main.rs
let nats_client = WhitelistNatsClient::connect(&nats_url).await?;

// Subscribe to MINIMAL topic (not full)
let subject = format!("whitelist.pools.{}.minimal", chain);
let mut subscriber = nats_client.subscribe(&subject).await?;

// Spawn background task to handle updates
let pool_tracker_clone = exex.pool_tracker.clone();
tokio::spawn(async move {
    while let Some(message) = subscriber.next().await {
        match parse_minimal_whitelist(&message.payload) {
            Ok(pools) => {
                let mut tracker = pool_tracker_clone.write().await;
                for pool in pools {
                    tracker.add_pool(pool.id, pool.protocol, pool.factory);
                }
            }
            Err(e) => warn!("Failed to parse whitelist: {}", e),
        }
    }
});
```

## Scaling Analysis

### Scenario 1: Small Operation (100 pools)
- Minimal: 12 KB
- Full: 35 KB
- **Recommendation**: Use single full topic, overhead is negligible

### Scenario 2: Medium Operation (500 pools)
- Minimal: 60 KB
- Full: 175 KB
- **Recommendation**: Use dual topics for efficiency

### Scenario 3: Large Operation (2,000 pools)
- Minimal: 240 KB
- Full: 700 KB
- **Recommendation**: Use dual topics + consider pagination:

```python
# Publish in batches of 500
BATCH_SIZE = 500
for i in range(0, len(pools), BATCH_SIZE):
    batch = pools[i:i+BATCH_SIZE]
    await publish_batch(batch, batch_num=i//BATCH_SIZE)
```

### Scenario 4: Extreme (10,000 pools)
- Minimal: 1.2 MB (exceeds default NATS limit)
- Full: 3.5 MB (exceeds default NATS limit)
- **Recommendation**: Use incremental updates:

```json
{
  "added": [...],      // Pools added to whitelist
  "removed": ["0x..."], // Just addresses
  "chain": "ethereum",
  "timestamp": "..."
}
```

## Performance Impact

| Pools | Minimal Size | Full Size | Network (1Gbps) | Latency |
|-------|-------------|-----------|-----------------|---------|
| 100 | 5 KB | 35 KB | 0.04 ms | Negligible |
| 500 | 25 KB | 175 KB | 0.2 ms | Negligible |
| 1,000 | 50 KB | 350 KB | 0.4 ms | Negligible |
| 2,000 | 100 KB | 700 KB | 0.8 ms | Very low |
| 5,000 | 250 KB | 1.75 MB | 2 ms | Low |
| 10,000 | 500 KB | 3.5 MB | 4 ms | Acceptable |
| 20,000 | 1 MB | 7 MB | 8 ms | Use batching for full |

## Recommendations by Scale

1. **< 1,000 pools**: Use dual topics (minimal + full) - best practice
2. **1,000-10,000 pools**: Use dual topics - still excellent performance
3. **10,000-20,000 pools**: Use dual topics - minimal still fits in 1MB
4. **> 20,000 pools**: Consider incremental updates for full topic only

## Redis Role

Redis should be used for:
1. **Persistence**: Whitelist survives NATS restarts
2. **Bootstrap**: When ExEx/poolStateArena starts, load initial state from Redis
3. **Queries**: HTTP API to view current whitelist
4. **Fallback**: If NATS is down, poll Redis

```python
# Bootstrap flow
async def start_service():
    # 1. Load initial whitelist from Redis
    pools = await redis.get_whitelist(chain)

    # 2. Subscribe to NATS for updates
    subscriber = await nats.subscribe(f"whitelist.pools.{chain}.minimal")

    # 3. Process updates
    async for msg in subscriber:
        update_pools(msg)
```

## Final Recommendation

For your use case (likely 100-1,000 active pools):

**Use the dual-topic approach:**
- ExEx subscribes to `whitelist.pools.ethereum.minimal`
- poolStateArena subscribes to `whitelist.pools.ethereum.full`
- Both get exactly what they need, no waste

This gives you:
- ✅ 3x smaller messages for ExEx (faster)
- ✅ No Redis dependency for real-time updates
- ✅ Scales to 8,500 pools on minimal topic
- ✅ Each subscriber pays only for what they need
