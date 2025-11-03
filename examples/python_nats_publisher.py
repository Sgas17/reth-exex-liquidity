#!/usr/bin/env python3
"""
Pool Whitelist NATS Publisher - Example for dynamicWhitelist integration.

This script demonstrates how to publish pool whitelist updates to NATS
in the format expected by the ExEx and poolStateArena.

Publishes to two topics:
1. whitelist.pools.{chain}.minimal - For ExEx (addresses only, ~50 bytes/pool)
2. whitelist.pools.{chain}.full - For poolStateArena (with metadata, ~350 bytes/pool)

Usage:
    # Install dependencies first
    pip install nats-py

    # Run the publisher
    python examples/python_nats_publisher.py

Integration with dynamicWhitelist:
    Copy the PoolWhitelistPublisher class to your dynamicWhitelist project
    and call it from your orchestrator after filtering pools.
"""

import asyncio
import json
import logging
from datetime import datetime, timezone
from typing import Any, Dict, List, Optional

try:
    import nats
except ImportError:
    print("ERROR: nats-py not installed. Install with: pip install nats-py")
    exit(1)

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


class PoolWhitelistPublisher:
    """
    Publisher for pool whitelist updates to NATS.

    Publishes to two topics optimized for different consumers:
    - Minimal: Just pool addresses (for ExEx event filtering)
    - Full: Complete metadata (for poolStateArena price calculations)
    """

    def __init__(self, nats_url: str = "nats://localhost:4222"):
        """
        Initialize the pool whitelist publisher.

        Args:
            nats_url: NATS server URL (default: localhost:4222)
        """
        self.nats_url = nats_url
        self.nc: Optional[nats.Client] = None

    async def __aenter__(self):
        """Async context manager entry."""
        await self.connect()
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb):
        """Async context manager exit."""
        await self.close()

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

    async def publish_pool_whitelist(
        self,
        chain: str,
        pools: List[Dict[str, Any]],
        publish_minimal: bool = True,
        publish_full: bool = True
    ) -> Dict[str, bool]:
        """
        Publish pool whitelist to NATS topics.

        Args:
            chain: Chain identifier (ethereum, base, etc.)
            pools: List of pool dicts with structure:
                {
                    'address': str,              # Pool address (required)
                    'token0': {                  # Required for full topic
                        'address': str,
                        'decimals': int,
                        'symbol': str,
                        'name': str (optional)
                    },
                    'token1': {...},             # Same as token0
                    'protocol': str,             # "UniswapV2", "UniswapV3", "UniswapV4"
                    'factory': str,
                    'fee': int (optional),       # For V3/V4
                    'tick_spacing': int (optional),  # For V3/V4
                    'stable': bool (optional)    # For V2 stable pools
                }
            publish_minimal: Whether to publish to minimal topic
            publish_full: Whether to publish to full topic

        Returns:
            Dict mapping topic type to success status
        """
        if not self.nc:
            logger.error("❌ Not connected to NATS")
            return {"minimal": False, "full": False}

        if not pools:
            logger.warning(f"⚠️  No pools to publish for {chain}")
            return {"minimal": False, "full": False}

        results = {}
        timestamp = datetime.now(timezone.utc).isoformat()

        # Publish minimal message (for ExEx)
        if publish_minimal:
            try:
                minimal_msg = {
                    "pools": [pool["address"] for pool in pools],
                    "chain": chain,
                    "timestamp": timestamp
                }
                minimal_subject = f"whitelist.pools.{chain}.minimal"
                payload = json.dumps(minimal_msg).encode()

                await self.nc.publish(minimal_subject, payload)

                results["minimal"] = True
                logger.info(
                    f"📤 Published {len(pools)} pools to {minimal_subject} "
                    f"({len(payload)} bytes)"
                )
            except Exception as e:
                logger.error(f"❌ Failed to publish minimal message: {e}")
                results["minimal"] = False

        # Publish full message (for poolStateArena)
        if publish_full:
            try:
                full_msg = {
                    "pools": pools,
                    "chain": chain,
                    "timestamp": timestamp
                }
                full_subject = f"whitelist.pools.{chain}.full"
                payload = json.dumps(full_msg).encode()

                await self.nc.publish(full_subject, payload)

                results["full"] = True
                logger.info(
                    f"📤 Published {len(pools)} pools to {full_subject} "
                    f"({len(payload)} bytes)"
                )
            except Exception as e:
                logger.error(f"❌ Failed to publish full message: {e}")
                results["full"] = False

        return results


async def example_usage():
    """Example of how to use the publisher."""

    # Sample pool data (what you'd get from dynamicWhitelist filtering)
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
        },
        {
            "address": "0xcbcdf9626bc03e24f779434178a73a0b4bad62ed",
            "token0": {
                "address": "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
                "decimals": 8,
                "symbol": "WBTC"
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

    print("\n🚀 Pool Whitelist NATS Publisher Example")
    print("=" * 50)
    print(f"Publishing {len(pools)} pools to NATS...\n")

    # Publish to NATS
    async with PoolWhitelistPublisher() as publisher:
        results = await publisher.publish_pool_whitelist("ethereum", pools)

    print("\n" + "=" * 50)
    print("📊 Results:")
    print(f"  Minimal topic: {'✅ Success' if results.get('minimal') else '❌ Failed'}")
    print(f"  Full topic: {'✅ Success' if results.get('full') else '❌ Failed'}")
    print("\n💡 The ExEx should now receive these pool addresses!")
    print("   Check the ExEx logs for: 'Parsed whitelist message'")


if __name__ == "__main__":
    print("""
╔══════════════════════════════════════════════════════════════╗
║  Pool Whitelist NATS Publisher                               ║
║  For dynamicWhitelist → ExEx & poolStateArena integration    ║
╚══════════════════════════════════════════════════════════════╝

This example shows how to publish pool whitelists to NATS.

Integration Steps:
1. Copy PoolWhitelistPublisher class to dynamicWhitelist
2. After filtering pools in orchestrator, call:

   async with PoolWhitelistPublisher() as publisher:
       await publisher.publish_pool_whitelist(chain, filtered_pools)

3. The ExEx will automatically receive and track these pools
4. poolStateArena will get full metadata for price calculations
    """)

    try:
        asyncio.run(example_usage())
    except KeyboardInterrupt:
        print("\n\n👋 Interrupted by user")
    except Exception as e:
        print(f"\n\n❌ Error: {e}")
        logger.exception("Fatal error")
