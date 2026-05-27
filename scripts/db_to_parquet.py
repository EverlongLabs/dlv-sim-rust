#!/usr/bin/env python3
"""Convert a DLV simulator SQLite .db file to the .parquet + .pool-config.json
format expected by the Rust EventReader.

Usage:
    python3 scripts/db_to_parquet.py <path-to-db>

Outputs alongside the .db file:
    <base>.parquet
    <base>.pool-config.json
"""
import json
import sqlite3
import sys

import pyarrow as pa
import pyarrow.parquet as pq


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <path-to-db>")
        sys.exit(1)

    db_path = sys.argv[1]
    base = db_path.removesuffix(".db")
    parquet_path = f"{base}.parquet"
    config_path = f"{base}.pool-config.json"

    db = sqlite3.connect(db_path)

    # ── Pool config ──────────────────────────────────────────────────────────
    row = db.execute(
        "SELECT pool_config_id, token0, token1, fee, tick_spacing, initial_sqrt_price_X96 "
        "FROM pool_config LIMIT 1"
    ).fetchone()
    if not row:
        print("ERROR: pool_config table is empty")
        sys.exit(1)

    pool_config = {
        "id": row[0],
        "token0": row[1],
        "token1": row[2],
        "fee": row[3],
        "tickSpacing": row[4],
        "initialSqrtPriceX96": row[5],
    }
    with open(config_path, "w") as f:
        json.dump(pool_config, f, indent=2)
    print(f"[OK] Wrote {config_path}")

    # ── Events ───────────────────────────────────────────────────────────────
    # Liquidity events: type=1 → MINT, type=2 → BURN (type=0 ignored by TS)
    liq_rows = db.execute(
        "SELECT type, id, msg_sender, recipient, amount0, amount1, "
        "       tick_lower, tick_upper, liquidity, "
        "       NULL AS amount_specified, NULL AS sqrt_price_x96, NULL AS tick_swap, "
        "       block_number, transaction_hash, log_index, date, verified "
        "FROM liquidity_events "
        "WHERE type IN (1, 2) "
        "ORDER BY block_number, log_index"
    ).fetchall()
    print(f"[INFO] {len(liq_rows)} liquidity events (mint+burn)")

    # Swap events
    swap_rows = db.execute(
        "SELECT 3 AS type, id, msg_sender, recipient, amount0, amount1, "
        "       NULL AS tick_lower, NULL AS tick_upper, liquidity, "
        "       amount_specified, sqrt_price_x96, tick AS tick_swap, "
        "       block_number, transaction_hash, log_index, date, verified "
        "FROM swap_events "
        "ORDER BY block_number, log_index"
    ).fetchall()
    print(f"[INFO] {len(swap_rows)} swap events")

    all_rows = liq_rows + swap_rows
    all_rows.sort(key=lambda r: (r[12], r[14]))  # block_number, log_index
    print(f"[INFO] {len(all_rows)} total events (sorted by block, log_index)")

    if not all_rows:
        print("WARNING: no events found")
        db.close()
        return

    # Build Arrow arrays
    types = []
    ids = []
    senders = []
    recipients = []
    amount0s = []
    amount1s = []
    tick_lowers = []
    tick_uppers = []
    liquidities = []
    amt_specifieds = []
    sqrt_prices = []
    ticks = []
    blocks = []
    tx_hashes = []
    log_indices = []
    dates = []
    verifieds = []

    for row in all_rows:
        (typ, eid, sender, recip, a0, a1, tl, tu, liq,
         amt_spec, sqrt_p, tick_s, blk, tx_hash, log_idx, date, verified) = row

        types.append(typ)
        ids.append(eid)
        senders.append(sender or "")
        recipients.append(recip or "")
        amount0s.append(str(a0) if a0 is not None else "0")
        amount1s.append(str(a1) if a1 is not None else "0")
        tick_lowers.append(tl)
        tick_uppers.append(tu)
        liquidities.append(str(liq) if liq is not None else "0")
        amt_specifieds.append(str(amt_spec) if amt_spec is not None else None)
        sqrt_prices.append(str(sqrt_p) if sqrt_p is not None else None)
        ticks.append(tick_s)
        blocks.append(blk)
        tx_hashes.append(str(tx_hash) if tx_hash else "")
        log_indices.append(log_idx)
        dates.append(date)
        verifieds.append(bool(verified) if verified is not None else False)

    schema = pa.schema([
        ("type", pa.int32()),
        ("id", pa.int32()),
        ("msg_sender", pa.string()),
        ("recipient", pa.string()),
        ("amount0", pa.string()),
        ("amount1", pa.string()),
        ("tick_lower", pa.int32()),
        ("tick_upper", pa.int32()),
        ("liquidity", pa.string()),
        ("amount_specified", pa.string()),
        ("sqrt_price_x96", pa.string()),
        ("tick", pa.int32()),
        ("block_number", pa.int32()),
        ("transaction_hash", pa.string()),
        ("log_index", pa.int32()),
        ("date", pa.string()),
        ("verified", pa.bool_()),
    ])

    table = pa.table({
        "type": pa.array(types, type=pa.int32()),
        "id": pa.array(ids, type=pa.int32()),
        "msg_sender": pa.array(senders, type=pa.string()),
        "recipient": pa.array(recipients, type=pa.string()),
        "amount0": pa.array(amount0s, type=pa.string()),
        "amount1": pa.array(amount1s, type=pa.string()),
        "tick_lower": pa.array(tick_lowers, type=pa.int32()),
        "tick_upper": pa.array(tick_uppers, type=pa.int32()),
        "liquidity": pa.array(liquidities, type=pa.string()),
        "amount_specified": pa.array(amt_specifieds, type=pa.string()),
        "sqrt_price_x96": pa.array(sqrt_prices, type=pa.string()),
        "tick": pa.array(ticks, type=pa.int32()),
        "block_number": pa.array(blocks, type=pa.int32()),
        "transaction_hash": pa.array(tx_hashes, type=pa.string()),
        "log_index": pa.array(log_indices, type=pa.int32()),
        "date": pa.array(dates, type=pa.string()),
        "verified": pa.array(verifieds, type=pa.bool_()),
    }, schema=schema)

    pq.write_table(table, parquet_path)
    print(f"[OK] Wrote {parquet_path} ({len(all_rows)} rows)")
    db.close()


if __name__ == "__main__":
    main()
