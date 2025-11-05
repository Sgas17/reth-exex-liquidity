#!/usr/bin/env python3
"""
Simple test script to send pool whitelist to ExEx via NATS
"""

import asyncio
import json
from datetime import datetime, timezone
from nats.aio.client import Client as NATS

async def main():
    # Connect to NATS
    nc = NATS()
    await nc.connect("nats://localhost:4222")
    print("✅ Connected to NATS at localhost:4222")

    # Test pools - high volume Uniswap pools (V2, V3, and V4)
    test_pools = [
        # V3 pools (highest volume)
        "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640",  # USDC/WETH 0.05% V3
        "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8",  # USDC/WETH 0.3% V3
        # V2 pools (highest volume)
        "0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852",  # WETH/USDT V2
        "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc",  # USDC/WETH V2
        # V4 pool (32-byte pool ID)
        "0xdce6394339af00981949f5f3baf27e3610c76326a700af57e4b3e3ae4977f78d",  # V4 pool
    ]

    # Specify protocol for each pool (parallel to pools array)
    test_protocols = [
        "v3",  # USDC/WETH 0.05%
        "v3",  # USDC/WETH 0.3%
        "v2",  # WETH/USDT
        "v2",  # USDC/WETH
        "v4",  # V4 pool
    ]

    # Create whitelist message with protocols
    message = {
        "type": "add",
        "pools": test_pools,
        "protocols": test_protocols,
        "chain": "ethereum",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "snapshot_id": 1
    }

    subject = "whitelist.pools.ethereum.minimal"

    print(f"\n📤 Publishing whitelist to subject: {subject}")
    print(f"   Message type: {message['type']}")
    print(f"   Pool count: {len(message['pools'])}")
    print(f"   Pools (with protocols):")
    for pool, protocol in zip(message['pools'], message['protocols']):
        print(f"     - {pool} ({protocol})")

    # Publish message
    await nc.publish(subject, json.dumps(message).encode())
    print("\n✅ Message published successfully!")

    # Cleanup
    await nc.close()
    print("✅ NATS connection closed")

if __name__ == "__main__":
    asyncio.run(main())
