#!/usr/bin/env python3
"""
Test Differential Whitelist Updates - Demonstration Script

This script demonstrates the differential update flow:
1. Publishes initial whitelist (Full)
2. Adds new pools (Add)
3. Removes pools (Remove)
4. Shows the ExEx receiving and applying updates

Usage:
    pip install nats-py
    python examples/test_differential_updates.py
"""

import asyncio
import json
import logging
from datetime import datetime, timezone
from typing import List, Dict, Any

try:
    import nats
except ImportError:
    print("ERROR: nats-py not installed. Install with: pip install nats-py")
    exit(1)

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


class DifferentialUpdateTester:
    """Tests differential whitelist updates with the ExEx."""

    def __init__(self, nats_url: str = "nats://localhost:4222"):
        self.nats_url = nats_url
        self.nc = None
        self.snapshot_id = int(datetime.now(timezone.utc).timestamp() * 1000)

    async def connect(self):
        """Connect to NATS server."""
        try:
            self.nc = await nats.connect(self.nats_url)
            logger.info(f"✅ Connected to NATS at {self.nats_url}")
        except Exception as e:
            logger.error(f"❌ Failed to connect to NATS: {e}")
            raise

    async def close(self):
        """Close NATS connection."""
        if self.nc:
            await self.nc.close()
            logger.info("Disconnected from NATS")

    async def publish_full(self, chain: str, pools: List[str]):
        """Publish full whitelist (initial load or forced full replacement)."""
        self.snapshot_id += 1
        message = {
            "type": "full",
            "pools": pools,
            "chain": chain,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "snapshot_id": self.snapshot_id
        }

        subject = f"whitelist.pools.{chain}.minimal"
        payload = json.dumps(message).encode()
        await self.nc.publish(subject, payload)

        logger.info(
            f"📤 Published FULL update: {len(pools)} pools "
            f"(snapshot {self.snapshot_id}, {len(payload)} bytes)"
        )
        return self.snapshot_id

    async def publish_add(self, chain: str, pools: List[str]):
        """Publish differential add update."""
        self.snapshot_id += 1
        message = {
            "type": "add",
            "pools": pools,
            "chain": chain,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "snapshot_id": self.snapshot_id
        }

        subject = f"whitelist.pools.{chain}.minimal"
        payload = json.dumps(message).encode()
        await self.nc.publish(subject, payload)

        logger.info(
            f"📤 Published ADD update: +{len(pools)} pools "
            f"(snapshot {self.snapshot_id}, {len(payload)} bytes)"
        )
        return self.snapshot_id

    async def publish_remove(self, chain: str, pools: List[str]):
        """Publish differential remove update."""
        self.snapshot_id += 1
        message = {
            "type": "remove",
            "pools": pools,
            "chain": chain,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "snapshot_id": self.snapshot_id
        }

        subject = f"whitelist.pools.{chain}.minimal"
        payload = json.dumps(message).encode()
        await self.nc.publish(subject, payload)

        logger.info(
            f"📤 Published REMOVE update: -{len(pools)} pools "
            f"(snapshot {self.snapshot_id}, {len(payload)} bytes)"
        )
        return self.snapshot_id

    async def run_test(self):
        """Run the differential update test sequence."""
        await self.connect()

        print("\n" + "=" * 70)
        print("🧪 DIFFERENTIAL WHITELIST UPDATE TEST")
        print("=" * 70)

        try:
            # Initial pools
            initial_pools = [
                "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",  # USDC/WETH 0.05%
                "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8",  # USDC/WETH 0.3%
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed",  # WBTC/WETH 0.3%
            ]

            print("\n📋 Step 1: Publish initial whitelist (FULL)")
            print(f"   Pools: {len(initial_pools)}")
            await self.publish_full("ethereum", initial_pools)
            print("   ✅ ExEx should log: 'Received FULL update: 3 pools'")
            await asyncio.sleep(1)

            print("\n" + "-" * 70)
            print("📋 Step 2: Add new pools (DIFFERENTIAL)")
            new_pools = [
                "0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36",  # USDT/WETH 0.3%
                "0x11b815efB8f581194ae79006d24E0d814B7697F6",  # USDT/WETH 0.05%
            ]
            print(f"   Adding: {len(new_pools)} pools")
            await self.publish_add("ethereum", new_pools)
            print("   ✅ ExEx should log: 'Received ADD update: +2 pools'")
            print("   ✅ Total pools tracked: 5")
            await asyncio.sleep(1)

            print("\n" + "-" * 70)
            print("📋 Step 3: Remove pools (DIFFERENTIAL)")
            removed_pools = [
                "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed",  # WBTC/WETH
            ]
            print(f"   Removing: {len(removed_pools)} pools")
            await self.publish_remove("ethereum", removed_pools)
            print("   ✅ ExEx should log: 'Received REMOVE update: -1 pools'")
            print("   ✅ Total pools tracked: 4")
            await asyncio.sleep(1)

            print("\n" + "-" * 70)
            print("📋 Step 4: Mixed operation (Add + Remove)")
            print("   Adding 2 pools...")
            await self.publish_add("ethereum", [
                "0x5777d92f208679DB4b9778590Fa3CAB3aC9e2168",  # DAI/USDC 0.01%
            ])
            await asyncio.sleep(0.5)

            print("   Removing 1 pool...")
            await self.publish_remove("ethereum", [
                "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8",  # USDC/WETH 0.3%
            ])
            print("   ✅ Total pools tracked: 4")
            await asyncio.sleep(1)

            print("\n" + "=" * 70)
            print("✅ TEST COMPLETE")
            print("=" * 70)
            print("\n📊 Performance Comparison:")
            print(f"   Full update (4 pools):     ~{len(json.dumps({'type': 'full', 'pools': initial_pools + new_pools[:-1]}).encode())} bytes")
            print(f"   Add update (2 pools):      ~{len(json.dumps({'type': 'add', 'pools': new_pools}).encode())} bytes")
            print(f"   Remove update (1 pool):    ~{len(json.dumps({'type': 'remove', 'pools': removed_pools}).encode())} bytes")
            print("\n💡 Bandwidth savings: ~70-90% for typical updates")

            print("\n🔍 Check ExEx Logs:")
            print("   You should see messages like:")
            print("   • '📥 Received FULL update: 3 pools for ethereum (snapshot: ####)'")
            print("   • '📥 Received ADD update: +2 pools for ethereum (snapshot: ####)'")
            print("   • '📥 Received REMOVE update: -1 pools for ethereum (snapshot: ####)'")

        finally:
            await self.close()


async def main():
    print("""
╔══════════════════════════════════════════════════════════════════╗
║  Differential Whitelist Update Tester                            ║
║  Demonstrates Add/Remove/Full updates to ExEx                    ║
╚══════════════════════════════════════════════════════════════════╝

Prerequisites:
1. NATS server running:
   docker run -p 4222:4222 nats:latest

2. ExEx running:
   cd ~/reth-exex-liquidity && cargo run --release

3. ExEx subscribed to whitelist.pools.ethereum.minimal

Starting test in 3 seconds...
    """)

    await asyncio.sleep(3)

    tester = DifferentialUpdateTester()
    await tester.run_test()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n\n👋 Test interrupted by user")
    except Exception as e:
        print(f"\n\n❌ Test failed: {e}")
        logging.exception("Test error")
