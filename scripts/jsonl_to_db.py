#!/usr/bin/env python3
"""
jsonl_to_db.py — convert a Rust dlv-sim output JSONL into the SQLite .db that
the TypeScript dashboard (`scripts/rebalance_plotting.ts` in ../dlv-sim) reads.

The dashboard reads a `rebalanceLog` table (the 60-column schema defined in
../dlv-sim/test/LogDBManager.ts) plus a `simulationConfig` key/value table. This
script reproduces that schema exactly so `yarn dashboard` / the other dashboards
can render the Rust run identically to `yarn simulate`.

Field mapping is tolerant: it accepts both the dashboard's canonical key names
and the Rust output's legacy names (e.g. `rawPoolPrice` OR `price`), so it keeps
working as the Rust output is migrated toward full field parity. Columns the
Rust sim does not emit yet default to the same values the TS sim writes ("0" for
numeric strings, "" for the yb-passive telemetry columns).

Usage:
    python3 scripts/jsonl_to_db.py output/output_wbtc_usdc_katana_5bp.jsonl [out.db]

simulationConfig is populated from BF_* / known env vars (the same ones the run
scripts export); BF_POOL and BF_POOL_FEE drive the dashboard's decimals/symbols.
"""

import json
import os
import sqlite3
import sys

# Column order matches ../dlv-sim/test/LogDBManager.ts createTable("rebalanceLog").
COLUMNS = [
    "wide0", "wide1", "base0", "base1", "limit0", "limit1",
    "total0", "total1",
    "nonVolatileAssetPrice", "prevTotalPoolValue", "afterTotalPoolValue",
    "lpRatio", "swapFeeStable", "prevCollateralRatio", "afterCollateralRatio",
    "accumulatedSwapFees0", "accumulatedSwapFees1", "debt", "rebalanceType",
    "almSwapFeeStable", "volatileHoldValueStable", "realizedIL",
    "swapFeesGainedThisPeriod", "regulateDebtAmount", "prevLpRatio",
    "currentPPS", "fundamentalPPS", "equilibriumPriceWad",
    "arbProfitStable", "arbDeviationBps", "externalPrice", "rawPoolPrice",
    "idle0", "idle1",
    "wideAmount0", "wideAmount1", "baseAmount0", "baseAmount1",
    "limitAmount0", "limitAmount1",
    "levAmmCollateral", "levAmmNotional", "levAmmDebt", "levAmmFeeRevenue",
    "bidLimit0", "bidLimit1", "askLimit0", "askLimit1",
    "bidLimitAmount0", "bidLimitAmount1", "askLimitAmount0", "askLimitAmount1",
    "date",
    "ybpGammaEff", "ybpKappaBid", "ybpKappaAsk", "ybpAlphaBps", "ybpToxicityBps",
    "ybpDynamicTargetShareBps", "ybpVolRegimeScore", "ybpJumpRiskScore",
    "ybpBaseWidthTicks", "ybpWideWidthTicks", "ybpWideRangeWeight",
]

CREATE_REBALANCE_LOG = (
    "CREATE TABLE `rebalanceLog` ("
    "`id` integer not null primary key autoincrement, "
    + ", ".join(
        f"`{c}` {'text' if c == 'date' else ('varchar(64)' if c.startswith('ybp') else 'varchar(255)')}"
        for c in COLUMNS
    )
    + ")"
)

# yb-passive telemetry columns default to "" (TS writes empty when the mode is off).
YBP_COLUMNS = {c for c in COLUMNS if c.startswith("ybp")}

# Per-column source: dashboard key first, then Rust legacy alias(es). First key
# present in the row wins; otherwise the default below is used.
ALIASES = {
    "nonVolatileAssetPrice": ["nonVolatileAssetPrice", "price"],
    "afterTotalPoolValue": ["afterTotalPoolValue", "totalValueStable"],
    "afterCollateralRatio": ["afterCollateralRatio", "collateralRatio"],
    "accumulatedSwapFees0": ["accumulatedSwapFees0", "accumulatedFees0"],
    "accumulatedSwapFees1": ["accumulatedSwapFees1", "accumulatedFees1"],
    "rawPoolPrice": ["rawPoolPrice", "price"],
}

# Env keys mirrored into simulationConfig. The dashboard needs BF_POOL and
# BF_POOL_FEE; the rest keep the config panel informative.
CONFIG_ENV_KEYS = [
    "BF_POOL", "BF_POOL_FEE", "BF_START_DATE", "BF_END_DATE", "LOOKUP_PERIOD",
    "TARGET_CR_PCT", "IS_REGULATE_DEBT", "ARB_STRATEGY", "ARB_MODE",
    "ACTIVE_REBALANCE_MODE", "USE_DYNAMIC_WIDTH", "USE_ASYMMETRIC_DELEVERAGE",
    "USE_LEV_AMM", "NO_ARB_DONATION", "BF_CHARM_JSON", "BF_DLV_JSON",
    "BF_ARB_JSON", "BF_SLOW_RECENTER_JSON", "BF_LEV_AMM_JSON", "BF_RD_TUNING_JSON",
]
CONFIG_DEFAULTS = {"BF_POOL": "WBTC_USDC_KATANA_5BP", "BF_POOL_FEE": "10000"}


def normalize_date(s):
    """Match the TS DB format 'YYYY-MM-DD HH:MM:SS.000'."""
    if not s:
        return s
    if "." not in s:
        return s + ".000"
    return s


def value_for(col, row):
    if col == "date":
        return normalize_date(row.get("date", ""))
    if col in YBP_COLUMNS:
        return row.get(col, "")
    if col == "arbDeviationBps":
        # TS stores BigInt(Math.round(...)); emit an integer string to match.
        v = row.get("arbDeviationBps", 0)
        try:
            return str(int(round(float(v))))
        except (TypeError, ValueError):
            return "0"
    if col == "externalPrice":
        v = row.get("externalPrice", 0)
        return str(v)
    for key in ALIASES.get(col, [col]):
        if key in row:
            v = row[key]
            return v if isinstance(v, str) else str(v)
    return "0"


def main():
    if len(sys.argv) < 2:
        sys.exit("usage: jsonl_to_db.py <input.jsonl> [output.db]")
    in_path = sys.argv[1]
    out_path = sys.argv[2] if len(sys.argv) > 2 else (
        in_path[:-6] + ".db" if in_path.endswith(".jsonl") else in_path + ".db"
    )

    if os.path.exists(out_path):
        os.remove(out_path)

    conn = sqlite3.connect(out_path)
    conn.execute("PRAGMA journal_mode = OFF")
    conn.execute("PRAGMA synchronous = OFF")
    conn.execute(CREATE_REBALANCE_LOG)
    conn.execute("CREATE TABLE `simulationConfig` (`key` varchar(255) primary key, `value` text)")

    cfg = dict(CONFIG_DEFAULTS)
    for k in CONFIG_ENV_KEYS:
        if k in os.environ:
            cfg[k] = os.environ[k]
    conn.executemany(
        "INSERT INTO `simulationConfig` (`key`,`value`) VALUES (?,?)", list(cfg.items())
    )

    placeholders = ",".join("?" * len(COLUMNS))
    insert_sql = f"INSERT INTO `rebalanceLog` ({','.join('`'+c+'`' for c in COLUMNS)}) VALUES ({placeholders})"

    def rows():
        with open(in_path, "r") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                r = json.loads(line)
                yield tuple(value_for(c, r) for c in COLUMNS)

    n = 0
    batch = []
    BATCH = 20000
    for tup in rows():
        batch.append(tup)
        if len(batch) >= BATCH:
            conn.executemany(insert_sql, batch)
            n += len(batch)
            batch.clear()
    if batch:
        conn.executemany(insert_sql, batch)
        n += len(batch)

    conn.commit()
    conn.close()
    print(f"[jsonl_to_db] wrote {n} rows + {len(cfg)} config keys to {out_path}")


if __name__ == "__main__":
    main()
