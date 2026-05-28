#!/bin/bash
# Bit-exact KATANA_5BP arb-mode parity run (Rust ↔ TS), 2 years.
#
# Wrap the whole config in a script so the JSON env-vars are guaranteed to
# survive shell delivery (zsh's history expansion, paste-mode wrapping, or
# stray exports). See README.md "Running long-form parity configs" for why.
#
# Usage:
#   bash scripts/run-katana-5bp-2y.sh
#
# Diagnostic (prints parsed config and exits without running):
#   CONFIG_ONLY=1 bash scripts/run-katana-5bp-2y.sh
#
# Override the binary path:
#   DLV_SIM_BIN=/path/to/dlv-sim bash scripts/run-katana-5bp-2y.sh
#
# Override the date window (defaults to the full 2-year span below):
#   BF_END_DATE=2024-10-21 bash scripts/run-katana-5bp-2y.sh   # 6-month run
#   BF_END_DATE=2025-04-21 bash scripts/run-katana-5bp-2y.sh   # 1-year run

set -euo pipefail

export TZ=UTC
export BF_POOL=WBTC_USDC_KATANA_5BP
export BF_POOL_FEE=10000
export BF_START_DATE="${BF_START_DATE:-2024-04-21}"
export BF_END_DATE="${BF_END_DATE:-2026-04-21}"
export LOOKUP_PERIOD=12
export TARGET_CR_PCT=200
export IS_REGULATE_DEBT=false
export ARB_STRATEGY=true
export ARB_MODE=close_gap
export ACTIVE_REBALANCE_MODE=passive
export USE_DYNAMIC_WIDTH=true
export USE_ASYMMETRIC_DELEVERAGE=false
export USE_LEV_AMM=true
export NO_ARB_DONATION=true

export BF_CHARM_JSON='{"wideRangeWeight":0,"wideThreshold":44000,"baseThreshold":13200,"limitThreshold":600,"period":8640000000000}'
export BF_DLV_JSON='{"deviationThresholdAbove":0.01,"deviationThresholdBelow":0.01,"debtToVolatileSwapFee":0.0015,"almSwapPriceSource":"binance"}'
export BF_ARB_JSON='{"priceFeedDir":"data/binance/BTCUSDT","mode":"close_gap","startDate":"'"$BF_START_DATE"'","endDate":"'"$BF_END_DATE"'"}'
export BF_SLOW_RECENTER_JSON='{"enabled":true,"minDeviation":0.03,"maxShiftPerStep":2,"accelerationThreshold":0.3,"accelerationMultiplier":2,"emergencyThreshold":0.6,"triggerIntervalSeconds":3600,"onlyShiftOOR":false,"redeployLimitAtCurrentTick":false}'
export BF_LEV_AMM_JSON='{"enabled":true,"swapFee":0.005,"maxArbPerTickFrac":1}'
export BF_RD_TUNING_JSON='{"mintFraction":1,"burnFraction":1}'

BIN="${DLV_SIM_BIN:-$(dirname "$0")/../target/release/dlv-sim}"
if [ ! -x "$BIN" ]; then
  echo "[run] binary not found or not executable: $BIN" >&2
  echo "[run] build with: cargo build --release" >&2
  exit 1
fi

exec "$BIN" "$@"
