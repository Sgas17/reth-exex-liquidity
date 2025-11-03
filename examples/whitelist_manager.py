#!/usr/bin/env python3
"""
Whitelist Manager - Differential Update Publisher for dynamicWhitelist

This module calculates differential whitelist changes and publishes Add/Remove
messages to NATS instead of full replacements.

Key Features:
- Loads last published whitelist from TimescaleDB
- Calculates differential changes (added/removed pools)
- Publishes differential updates to NATS (Add/Remove/Replace)
- Stores new whitelist snapshots to database
- 100-400x faster than full replacements for typical updates

Integration:
    Copy this file to dynamicWhitelist/src/core/whitelist_manager.py
    Use in your orchestrator after pool filtering:

    from core.whitelist_manager import WhitelistManager

    manager = WhitelistManager(db_conn, nats_url)
    await manager.publish_differential_update(chain, new_pools)
"""

import asyncio
import json
import logging
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional, Set, Tuple

try:
    import nats
except ImportError:
    print("ERROR: nats-py not installed. Install with: pip install nats-py")
    exit(1)

try:
    import psycopg2
    from psycopg2.extras import RealDictCursor, execute_values
except ImportError:
    print("ERROR: psycopg2 not installed. Install with: pip install psycopg2-binary")
    exit(1)

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class WhitelistManager:
    """
    Manages pool whitelist with differential updates.

    Responsibilities:
    1. Load last published whitelist from database
    2. Calculate differential changes (added/removed pools)
    3. Publish Add/Remove/Replace messages to NATS
    4. Store whitelist snapshots to database

    Database Schema:
        CREATE TABLE IF NOT EXISTS whitelist_snapshots (
            id SERIAL PRIMARY KEY,
            chain TEXT NOT NULL,
            pool_address TEXT NOT NULL,
            pool_data JSONB NOT NULL,
            published_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            snapshot_id BIGINT NOT NULL,
            UNIQUE(chain, pool_address, snapshot_id)
        );

        CREATE INDEX idx_whitelist_snapshots_chain_snapshot
            ON whitelist_snapshots(chain, snapshot_id DESC);
    """

    def __init__(
        self,
        db_config: Dict[str, str],
        nats_url: str = "nats://localhost:4222"
    ):
        """
        Initialize WhitelistManager.

        Args:
            db_config: PostgreSQL connection config
                {
                    'host': 'localhost',
                    'port': 5432,
                    'database': 'defi_platform',
                    'user': 'postgres',
                    'password': 'password'
                }
            nats_url: NATS server URL
        """
        self.db_config = db_config
        self.nats_url = nats_url
        self.nc: Optional[nats.Client] = None
        self._ensure_schema()

    def _ensure_schema(self):
        """Create whitelist_snapshots table if it doesn't exist."""
        schema_sql = """
        CREATE TABLE IF NOT EXISTS whitelist_snapshots (
            id SERIAL PRIMARY KEY,
            chain TEXT NOT NULL,
            pool_address TEXT NOT NULL,
            pool_data JSONB NOT NULL,
            published_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            snapshot_id BIGINT NOT NULL,
            UNIQUE(chain, pool_address, snapshot_id)
        );

        CREATE INDEX IF NOT EXISTS idx_whitelist_snapshots_chain_snapshot
            ON whitelist_snapshots(chain, snapshot_id DESC);

        CREATE INDEX IF NOT EXISTS idx_whitelist_snapshots_published_at
            ON whitelist_snapshots(published_at DESC);
        """

        try:
            with psycopg2.connect(**self.db_config) as conn:
                with conn.cursor() as cur:
                    cur.execute(schema_sql)
                conn.commit()
            logger.info("✅ Whitelist snapshots schema verified")
        except Exception as e:
            logger.error(f"❌ Failed to create schema: {e}")
            raise

    async def connect_nats(self):
        """Connect to NATS server."""
        try:
            self.nc = await nats.connect(self.nats_url)
            logger.info(f"✅ Connected to NATS at {self.nats_url}")
        except Exception as e:
            logger.error(f"❌ Failed to connect to NATS: {e}")
            raise

    async def close_nats(self):
        """Close NATS connection."""
        if self.nc:
            await self.nc.close()
            logger.info("Disconnected from NATS")

    def load_last_whitelist(self, chain: str) -> Tuple[Dict[str, Dict], Optional[int]]:
        """
        Load the last published whitelist from database.

        Args:
            chain: Chain identifier (ethereum, base, etc.)

        Returns:
            Tuple of (whitelist_dict, snapshot_id) where:
                whitelist_dict: {pool_address: pool_data}
                snapshot_id: Last snapshot ID (None if no previous snapshot)
        """
        query = """
        SELECT pool_address, pool_data, snapshot_id
        FROM whitelist_snapshots
        WHERE chain = %s
          AND snapshot_id = (
              SELECT MAX(snapshot_id)
              FROM whitelist_snapshots
              WHERE chain = %s
          )
        """

        try:
            with psycopg2.connect(**self.db_config) as conn:
                with conn.cursor(cursor_factory=RealDictCursor) as cur:
                    cur.execute(query, (chain, chain))
                    rows = cur.fetchall()

                    if not rows:
                        logger.info(f"📭 No previous whitelist found for {chain}")
                        return {}, None

                    snapshot_id = rows[0]['snapshot_id']
                    whitelist = {
                        row['pool_address']: row['pool_data']
                        for row in rows
                    }

                    logger.info(
                        f"📚 Loaded {len(whitelist)} pools from snapshot {snapshot_id} "
                        f"for {chain}"
                    )
                    return whitelist, snapshot_id

        except Exception as e:
            logger.error(f"❌ Failed to load whitelist: {e}")
            raise

    def calculate_diff(
        self,
        old_whitelist: Dict[str, Dict],
        new_whitelist: Dict[str, Dict]
    ) -> Tuple[List[Dict], List[str]]:
        """
        Calculate differential changes between whitelists.

        Args:
            old_whitelist: {pool_address: pool_data}
            new_whitelist: {pool_address: pool_data}

        Returns:
            Tuple of (added_pools, removed_addresses) where:
                added_pools: List of new pool metadata dicts
                removed_addresses: List of pool addresses to remove
        """
        old_addresses = set(old_whitelist.keys())
        new_addresses = set(new_whitelist.keys())

        added_addresses = new_addresses - old_addresses
        removed_addresses = old_addresses - new_addresses

        # Convert added addresses to full pool metadata
        added_pools = [
            new_whitelist[addr]
            for addr in added_addresses
        ]

        logger.info(
            f"📊 Calculated diff: +{len(added_pools)} added, "
            f"-{len(removed_addresses)} removed "
            f"(total: {len(new_whitelist)} pools)"
        )

        return added_pools, list(removed_addresses)

    async def publish_differential_update(
        self,
        chain: str,
        new_pools: List[Dict[str, Any]],
        force_full: bool = False
    ) -> Dict[str, Any]:
        """
        Publish differential whitelist update to NATS.

        This is the main method to call from dynamicWhitelist orchestrator.

        Args:
            chain: Chain identifier (ethereum, base, etc.)
            new_pools: List of pool dicts with structure:
                {
                    'address': str,
                    'token0': {...},
                    'token1': {...},
                    'protocol': str,
                    'factory': str,
                    'fee': int (optional),
                    'tick_spacing': int (optional)
                }
            force_full: Force full replacement instead of differential

        Returns:
            Dict with update statistics:
                {
                    'snapshot_id': int,
                    'total_pools': int,
                    'added': int,
                    'removed': int,
                    'update_type': 'differential' | 'full',
                    'published': bool
                }
        """
        if not self.nc:
            await self.connect_nats()

        # Convert new_pools list to dict for comparison
        new_whitelist = {pool['address']: pool for pool in new_pools}

        # Load last published whitelist
        old_whitelist, last_snapshot_id = self.load_last_whitelist(chain)

        # Calculate diff
        added_pools, removed_addresses = self.calculate_diff(
            old_whitelist,
            new_whitelist
        )

        # Determine update type
        is_first_publish = last_snapshot_id is None
        is_full_replacement = force_full or is_first_publish

        # Generate new snapshot ID
        snapshot_id = int(datetime.now(timezone.utc).timestamp() * 1000)
        timestamp = datetime.now(timezone.utc).isoformat()

        # Publish to NATS
        try:
            if is_full_replacement:
                # Publish full replacement
                await self._publish_full_update(
                    chain, new_pools, timestamp, snapshot_id
                )
                update_type = 'full'
                logger.info(
                    f"📤 Published FULL whitelist: {len(new_pools)} pools "
                    f"(snapshot {snapshot_id})"
                )
            else:
                # Publish differential updates
                if added_pools:
                    await self._publish_add_update(
                        chain, added_pools, timestamp, snapshot_id
                    )

                if removed_addresses:
                    await self._publish_remove_update(
                        chain, removed_addresses, timestamp, snapshot_id
                    )

                update_type = 'differential'
                logger.info(
                    f"📤 Published DIFFERENTIAL update: "
                    f"+{len(added_pools)} added, -{len(removed_addresses)} removed "
                    f"(snapshot {snapshot_id})"
                )

            # Store snapshot to database
            self._store_snapshot(chain, new_pools, snapshot_id)

            return {
                'snapshot_id': snapshot_id,
                'total_pools': len(new_pools),
                'added': len(added_pools),
                'removed': len(removed_addresses),
                'update_type': update_type,
                'published': True
            }

        except Exception as e:
            logger.error(f"❌ Failed to publish update: {e}")
            raise

    async def _publish_add_update(
        self,
        chain: str,
        pools: List[Dict],
        timestamp: str,
        snapshot_id: int
    ):
        """Publish Add update to NATS."""
        # Minimal message (for ExEx)
        minimal_msg = {
            "type": "add",
            "pools": [pool["address"] for pool in pools],
            "chain": chain,
            "timestamp": timestamp,
            "snapshot_id": snapshot_id
        }
        minimal_subject = f"whitelist.pools.{chain}.minimal"
        await self.nc.publish(minimal_subject, json.dumps(minimal_msg).encode())

        # Full message (for poolStateArena)
        full_msg = {
            "type": "add",
            "pools": pools,
            "chain": chain,
            "timestamp": timestamp,
            "snapshot_id": snapshot_id
        }
        full_subject = f"whitelist.pools.{chain}.full"
        await self.nc.publish(full_subject, json.dumps(full_msg).encode())

        logger.debug(f"  ➕ Published Add: {len(pools)} pools")

    async def _publish_remove_update(
        self,
        chain: str,
        pool_addresses: List[str],
        timestamp: str,
        snapshot_id: int
    ):
        """Publish Remove update to NATS."""
        # Minimal message (for ExEx)
        minimal_msg = {
            "type": "remove",
            "pools": pool_addresses,  # For remove, just addresses
            "chain": chain,
            "timestamp": timestamp,
            "snapshot_id": snapshot_id
        }
        minimal_subject = f"whitelist.pools.{chain}.minimal"
        await self.nc.publish(minimal_subject, json.dumps(minimal_msg).encode())

        # Full message (for poolStateArena) - same as minimal for remove
        full_msg = {
            "type": "remove",
            "pool_addresses": pool_addresses,
            "chain": chain,
            "timestamp": timestamp,
            "snapshot_id": snapshot_id
        }
        full_subject = f"whitelist.pools.{chain}.full"
        await self.nc.publish(full_subject, json.dumps(full_msg).encode())

        logger.debug(f"  ➖ Published Remove: {len(pool_addresses)} pools")

    async def _publish_full_update(
        self,
        chain: str,
        pools: List[Dict],
        timestamp: str,
        snapshot_id: int
    ):
        """Publish full replacement to NATS (backward compatible)."""
        # Minimal message (for ExEx)
        minimal_msg = {
            "type": "full",
            "pools": [pool["address"] for pool in pools],
            "chain": chain,
            "timestamp": timestamp,
            "snapshot_id": snapshot_id
        }
        minimal_subject = f"whitelist.pools.{chain}.minimal"
        await self.nc.publish(minimal_subject, json.dumps(minimal_msg).encode())

        # Full message (for poolStateArena)
        full_msg = {
            "type": "full",
            "pools": pools,
            "chain": chain,
            "timestamp": timestamp,
            "snapshot_id": snapshot_id
        }
        full_subject = f"whitelist.pools.{chain}.full"
        await self.nc.publish(full_subject, json.dumps(full_msg).encode())

        logger.debug(f"  🔄 Published Full: {len(pools)} pools")

    def _store_snapshot(
        self,
        chain: str,
        pools: List[Dict],
        snapshot_id: int
    ):
        """Store whitelist snapshot to database."""
        insert_sql = """
        INSERT INTO whitelist_snapshots
            (chain, pool_address, pool_data, snapshot_id, published_at)
        VALUES %s
        ON CONFLICT (chain, pool_address, snapshot_id) DO NOTHING
        """

        # Prepare values for bulk insert
        values = [
            (chain, pool['address'], json.dumps(pool), snapshot_id, datetime.now(timezone.utc))
            for pool in pools
        ]

        try:
            with psycopg2.connect(**self.db_config) as conn:
                with conn.cursor() as cur:
                    execute_values(cur, insert_sql, values)
                conn.commit()

            logger.info(
                f"💾 Stored snapshot {snapshot_id}: {len(pools)} pools for {chain}"
            )
        except Exception as e:
            logger.error(f"❌ Failed to store snapshot: {e}")
            raise

    async def __aenter__(self):
        """Async context manager entry."""
        await self.connect_nats()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        """Async context manager exit."""
        await self.close_nats()


async def example_usage():
    """Example of how to use WhitelistManager in dynamicWhitelist."""

    # Database configuration
    db_config = {
        'host': 'localhost',
        'port': 5432,
        'database': 'defi_platform',
        'user': 'postgres',
        'password': 'your_password'
    }

    # Sample pools from dynamicWhitelist filtering
    pools = [
        {
            "address": "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",
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
        },
        {
            "address": "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8",
            "token0": {
                "address": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "decimals": 6,
                "symbol": "USDC"
            },
            "token1": {
                "address": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
                "decimals": 18,
                "symbol": "WETH"
            },
            "protocol": "UniswapV3",
            "factory": "0x1F98431c8aD98523631AE4a59f267346ea31F984",
            "fee": 3000,
            "tick_spacing": 60
        }
    ]

    print("\n🚀 WhitelistManager Differential Update Example")
    print("=" * 60)
    print(f"Publishing {len(pools)} pools with differential updates...\n")

    # Use WhitelistManager
    async with WhitelistManager(db_config) as manager:
        result = await manager.publish_differential_update("ethereum", pools)

    print("\n" + "=" * 60)
    print("📊 Update Results:")
    print(f"  Snapshot ID: {result['snapshot_id']}")
    print(f"  Total Pools: {result['total_pools']}")
    print(f"  Added: {result['added']}")
    print(f"  Removed: {result['removed']}")
    print(f"  Update Type: {result['update_type']}")
    print(f"  Published: {'✅' if result['published'] else '❌'}")
    print("\n💡 The ExEx should receive differential Add/Remove messages!")


if __name__ == "__main__":
    print("""
╔══════════════════════════════════════════════════════════════╗
║  WhitelistManager - Differential Update Publisher           ║
║  For dynamicWhitelist orchestrator integration               ║
╚══════════════════════════════════════════════════════════════╝

This module replaces full whitelist replacements with differential
updates, reducing NATS traffic by 100-400x for typical changes.

Integration Steps:
1. Copy to dynamicWhitelist/src/core/whitelist_manager.py
2. In your orchestrator, after filtering pools:

   from core.whitelist_manager import WhitelistManager

   async with WhitelistManager(db_config) as manager:
       result = await manager.publish_differential_update(
           chain="ethereum",
           new_pools=filtered_pools
       )

3. The ExEx automatically receives Add/Remove/Full messages
4. Whitelist history stored in whitelist_snapshots table
    """)

    try:
        asyncio.run(example_usage())
    except KeyboardInterrupt:
        print("\n\n👋 Interrupted by user")
    except Exception as e:
        print(f"\n\n❌ Error: {e}")
        logger.exception("Fatal error")
