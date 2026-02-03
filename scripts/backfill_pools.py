#!/usr/bin/env python3
"""
Backfill pool creations gap between cryo historical data and the ExEx chain tip.

Reads MAX(creation_block) from network_1_dex_pools_cryo, fetches pool creation
events for the gap using cryo, decodes them, and bulk inserts into the same table.

Usage:
    python scripts/backfill_pools.py

Environment variables:
    DATABASE_URL  - PostgreSQL connection string
                    (default: postgres://transfers_user:transfers_pass@localhost:5433/transfers)
    RPC_URL       - Ethereum JSON-RPC endpoint (default: http://localhost:8545)
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

import eth_abi.abi as eth_abi
import polars as pl
import psycopg2
import psycopg2.extras
import requests

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

DATABASE_URL = os.environ.get(
    "DATABASE_URL",
    "postgres://transfers_user:transfers_pass@localhost:5433/transfers",
)
RPC_URL = os.environ.get("RPC_URL", "http://localhost:8545")

TABLE = "network_1_dex_pools_cryo"

# Uniswap V2 PairCreated factories
V2_FACTORIES = [
    "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",  # Uniswap V2
    "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac",  # SushiSwap
    "0x1097053Fd2ea711dad45caCcc45EfF7548fCB362",  # PancakeSwap V2
]
V2_EVENT_HASH = "0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"

# Uniswap V3 PoolCreated factories
V3_FACTORIES = [
    "0x1F98431c8aD98523631AE4a59f267346ea31F984",  # Uniswap V3
    "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865",  # PancakeSwap V3
    "0xbACEB8eC6b9355Dfc0269C18bac9d6E2Bdc29C4F",  # SushiSwap V3
]
V3_EVENT_HASH = "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"

# Uniswap V4 Initialize (pool manager)
V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"
V4_EVENT_HASH = "0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438"

CRYO_INNER_REQUEST_SIZE = 10000


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def get_max_block(conn) -> int:
    """Get MAX(creation_block) from the pool table."""
    with conn.cursor() as cur:
        cur.execute(f"SELECT COALESCE(MAX(creation_block), 0) FROM {TABLE}")
        return cur.fetchone()[0]


def get_latest_block() -> int:
    """Get latest block number from the RPC node."""
    resp = requests.post(
        RPC_URL,
        json={"jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1},
        timeout=10,
    )
    resp.raise_for_status()
    return int(resp.json()["result"], 16)


def run_cryo(start_block: int, end_block: int, contracts: list[str],
             event_hash: str, output_dir: str) -> None:
    """Run cryo logs for the given block range and contracts."""
    cmd = [
        "cryo", "logs",
        "--rpc", RPC_URL,
        "--inner-request-size", str(CRYO_INNER_REQUEST_SIZE),
        "--u256-types", "binary",
        "--blocks", f"{start_block}:{end_block}",
        "--output-dir", output_dir,
        "--contract", *contracts,
        "--event-signature", event_hash,
    ]
    print(f"  Running: {' '.join(cmd[:6])} ... blocks {start_block}:{end_block}")
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"  WARNING: cryo exited {result.returncode}: {result.stderr.strip()}")


def read_parquet_dir(path: str) -> pl.DataFrame:
    """Read all parquet files in a directory, return empty DataFrame if none."""
    import glob
    files = glob.glob(os.path.join(path, "*.parquet"))
    if not files:
        return pl.DataFrame()
    return pl.read_parquet(os.path.join(path, "*.parquet"))


def to_lower_hex_address(raw: bytes) -> str:
    """Decode an ABI-encoded address and return lowercase hex."""
    (addr,) = eth_abi.decode(["address"], raw)
    return addr.lower()


# ---------------------------------------------------------------------------
# Decoders
# ---------------------------------------------------------------------------

def decode_v2_events(df: pl.DataFrame, factory_set: set[str]) -> list[dict]:
    """Decode PairCreated events from cryo parquet output."""
    rows = []
    for event in df.rows(named=True):
        factory = event.get("address")
        if factory is None:
            continue
        # cryo returns address as bytes
        if isinstance(factory, bytes):
            factory_hex = "0x" + factory.hex()
        else:
            factory_hex = factory if isinstance(factory, str) else str(factory)
        if factory_hex.lower() not in factory_set:
            continue

        token0 = to_lower_hex_address(event["topic1"])
        token1 = to_lower_hex_address(event["topic2"])
        # data = pair address (address) + pairIndex (uint256)
        pair_addr, _ = eth_abi.decode(["address", "uint256"], event["data"])
        pair_addr = pair_addr.lower()

        rows.append({
            "address": pair_addr,
            "factory": factory_hex.lower(),
            "asset0": token0,
            "asset1": token1,
            "creation_block": event["block_number"],
            "fee": 3000,
            "tick_spacing": None,
            "additional_data": None,
        })
    return rows


def decode_v3_events(df: pl.DataFrame, factory_set: set[str]) -> list[dict]:
    """Decode PoolCreated events from cryo parquet output."""
    rows = []
    for event in df.rows(named=True):
        factory = event.get("address")
        if factory is None:
            continue
        if isinstance(factory, bytes):
            factory_hex = "0x" + factory.hex()
        else:
            factory_hex = factory if isinstance(factory, str) else str(factory)
        if factory_hex.lower() not in factory_set:
            continue

        token0 = to_lower_hex_address(event["topic1"])
        token1 = to_lower_hex_address(event["topic2"])
        (fee,) = eth_abi.decode(["uint24"], event["topic3"])
        tick_spacing, pool_addr = eth_abi.decode(["int24", "address"], event["data"])
        pool_addr = pool_addr.lower()

        rows.append({
            "address": pool_addr,
            "factory": factory_hex.lower(),
            "asset0": token0,
            "asset1": token1,
            "creation_block": event["block_number"],
            "fee": fee,
            "tick_spacing": tick_spacing,
            "additional_data": None,
        })
    return rows


def decode_v4_events(df: pl.DataFrame, manager_lower: str) -> list[dict]:
    """Decode Initialize events from cryo parquet output."""
    rows = []
    for event in df.rows(named=True):
        factory = event.get("address")
        if factory is None:
            continue
        if isinstance(factory, bytes):
            factory_hex = "0x" + factory.hex()
        else:
            factory_hex = factory if isinstance(factory, str) else str(factory)
        if factory_hex.lower() != manager_lower:
            continue

        # topic1 = pool id (bytes32)
        pool_id = "0x" + event["topic1"].hex() if isinstance(event["topic1"], bytes) else event["topic1"]
        token0 = to_lower_hex_address(event["topic2"])
        token1 = to_lower_hex_address(event["topic3"])
        fee, tick_spacing, hooks_addr = eth_abi.decode(
            ["uint24", "int24", "address"], event["data"]
        )
        hooks_addr = hooks_addr.lower()

        rows.append({
            "address": pool_id.lower(),
            "factory": factory_hex.lower(),
            "asset0": token0,
            "asset1": token1,
            "creation_block": event["block_number"],
            "fee": fee,
            "tick_spacing": tick_spacing,
            "additional_data": json.dumps({"hooks_address": hooks_addr}),
        })
    return rows


# ---------------------------------------------------------------------------
# Database insert
# ---------------------------------------------------------------------------

def bulk_insert(conn, rows: list[dict]) -> int:
    """Bulk insert pool rows with ON CONFLICT DO NOTHING. Returns inserted count."""
    if not rows:
        return 0

    # Deduplicate by address (keep first occurrence)
    seen = set()
    unique = []
    for r in rows:
        if r["address"] not in seen:
            seen.add(r["address"])
            unique.append(r)

    sql = f"""
        INSERT INTO {TABLE} (address, factory, asset0, asset1, creation_block, fee, tick_spacing, additional_data)
        VALUES %s
        ON CONFLICT (address) DO NOTHING
    """
    values = [
        (
            r["address"],
            r["factory"],
            r["asset0"],
            r["asset1"],
            r["creation_block"],
            r["fee"],
            r["tick_spacing"],
            r["additional_data"],
        )
        for r in unique
    ]

    with conn.cursor() as cur:
        psycopg2.extras.execute_values(cur, sql, values, page_size=1000)
        inserted = cur.rowcount
    conn.commit()
    return inserted


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    print("=== Pool Creations Backfill ===")

    conn = psycopg2.connect(DATABASE_URL)
    try:
        max_block = get_max_block(conn)
        latest_block = get_latest_block()
        start_block = max_block + 1

        print(f"DB max block:     {max_block}")
        print(f"Chain tip:        {latest_block}")
        print(f"Gap:              {start_block} -> {latest_block} ({latest_block - start_block + 1} blocks)")

        if start_block > latest_block:
            print("No gap to backfill.")
            return

        tmpdir = tempfile.mkdtemp(prefix="backfill_pools_")
        try:
            all_rows: list[dict] = []

            # --- V2 PairCreated ---
            print("\n[V2] Fetching PairCreated events...")
            v2_dir = os.path.join(tmpdir, "v2")
            run_cryo(start_block, latest_block, V2_FACTORIES, V2_EVENT_HASH, v2_dir)
            v2_df = read_parquet_dir(v2_dir)
            if not v2_df.is_empty():
                v2_set = {a.lower() for a in V2_FACTORIES}
                v2_rows = decode_v2_events(v2_df, v2_set)
                print(f"  Decoded {len(v2_rows)} V2 pools")
                all_rows.extend(v2_rows)
            else:
                print("  No V2 events found")

            # --- V3 PoolCreated ---
            print("\n[V3] Fetching PoolCreated events...")
            v3_dir = os.path.join(tmpdir, "v3")
            run_cryo(start_block, latest_block, V3_FACTORIES, V3_EVENT_HASH, v3_dir)
            v3_df = read_parquet_dir(v3_dir)
            if not v3_df.is_empty():
                v3_set = {a.lower() for a in V3_FACTORIES}
                v3_rows = decode_v3_events(v3_df, v3_set)
                print(f"  Decoded {len(v3_rows)} V3 pools")
                all_rows.extend(v3_rows)
            else:
                print("  No V3 events found")

            # --- V4 Initialize ---
            print("\n[V4] Fetching Initialize events...")
            v4_dir = os.path.join(tmpdir, "v4")
            run_cryo(start_block, latest_block, [V4_POOL_MANAGER], V4_EVENT_HASH, v4_dir)
            v4_df = read_parquet_dir(v4_dir)
            if not v4_df.is_empty():
                v4_rows = decode_v4_events(v4_df, V4_POOL_MANAGER.lower())
                print(f"  Decoded {len(v4_rows)} V4 pools")
                all_rows.extend(v4_rows)
            else:
                print("  No V4 events found")

            # --- Insert ---
            print(f"\nTotal decoded pools: {len(all_rows)}")
            if all_rows:
                inserted = bulk_insert(conn, all_rows)
                print(f"Inserted {inserted} new pools (duplicates skipped via ON CONFLICT)")

        finally:
            shutil.rmtree(tmpdir, ignore_errors=True)

    finally:
        conn.close()

    print("\nDone.")


if __name__ == "__main__":
    main()
