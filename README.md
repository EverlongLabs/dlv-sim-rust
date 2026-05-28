# DLV Simulator (Rust)

Rust port of the TypeScript DLV vault simulator at `../dlv-sim`. Produces
bit-identical RESULT_JSON given the same inputs.

## Quick start — Docker Compose

Build the image once (or after source changes), then run any preset:

```bash
docker compose build
```

### KATANA_5BP arb-mode parity preset (LevAMM + slow recenter + dynamic width)

Full 2-year window (2024-04-21 → 2026-04-21, ~5.3M periods @ 12s), 8 GB RAM:

```bash
docker compose run --rm katana-5bp-2y
```

Parse-only diagnostic (prints parsed config and exits, no simulation):

```bash
docker compose run --rm -e CONFIG_ONLY=1 katana-5bp-2y
```

Shorter window:

```bash
docker compose run --rm -e BF_END_DATE=2024-06-21 katana-5bp-2y   # 2 months
docker compose run --rm -e BF_END_DATE=2024-10-21 katana-5bp-2y   # 6 months
docker compose run --rm -e BF_END_DATE=2025-04-21 katana-5bp-2y   # 1 year
```

All env vars come from `scripts/katana-5bp-2y.docker.env` via the compose
service's `env_file:` directive, so the ~3.5 KB of `BF_*_JSON` survives any
shell-delivery quirks.

### Simple event-replay presets (WBTC/USDC, cbBTC/USDC)

These use the generic `dlv-sim` service with env vars passed inline:

```bash
# WBTC/USDC, 6-month event-replay
docker compose run --rm -e BF_POOL=wbtc_usdc -e ARB_STRATEGY=false dlv-sim

# cbBTC/USDC, 6-month event-replay
docker compose run --rm -e BF_POOL=cbbtc_usdc_base -e ARB_STRATEGY=false dlv-sim
```

## Prerequisites

- Rust ≥ 1.87 (or use the Dockerfile)
- TS data files in `../dlv-sim/data/` — parquet for event-replay configs,
  Binance CSV feed for arb-mode configs
- For the TS comparison side: a working `../dlv-sim` checkout (`yarn install` etc.)

Local build (skip if using Docker):

```bash
cargo build --release
```

After any source change, rebuild — `cargo run --release` doesn't always pick up
changes if the binary is invoked through a wrapper script.

## Verified parity configs

The following configs have been validated bit-for-bit (RESULT_JSON byte-identical,
every trigger count and every final vault-state field matches TS).

### Event-replay mode (no arb, ALM-driven)

WBTC/USDC, 2025-09-09 → 2025-11-09 (2 months, 5s step, ~1M periods):

```bash
BF_POOL=wbtc_usdc \
BF_START_DATE=2025-09-09 BF_END_DATE=2025-11-09 \
ARB_STRATEGY=false \
./target/release/dlv-sim
```

Result (matches TS exactly, all 13 RESULT_JSON fields):

```
total amounts:    173026 WBTC, 170739267 USDC
virtual debt:     174709565
position USDC:    172142688
CR %:             198.53077477469537
RESULT_JSON: {"apy":-26.372830598230145,...,"sigma":106.63,"sortino":-67.52,"downsideStd":169.48}
```

### Arb mode (LevAMM + slow recenter + dynamic width)

KATANA_5BP, 2024-04-21 → 2026-04-21 (2 years, 12s step, ~5.3M periods):

```bash
bash scripts/run-katana-5bp-2y.sh
```

Or pick a shorter window via `BF_END_DATE` (any of 2-month / 6-month / 1-year /
2-year all match TS bit-for-bit):

```bash
BF_END_DATE=2024-06-21 bash scripts/run-katana-5bp-2y.sh   # 2 months
BF_END_DATE=2024-10-21 bash scripts/run-katana-5bp-2y.sh   # 6 months
BF_END_DATE=2025-04-21 bash scripts/run-katana-5bp-2y.sh   # 1 year
bash scripts/run-katana-5bp-2y.sh                          # 2 years (default)
```

2-year result (matches TS exactly):

```
periods processed: 5256000
DLV calls:         3176812
Arb calls:         2353133
LevAMM calls:      5256000
Slow Recenter:     567
total amounts:     82826 WBTC, 141750175 vbUSDC
virtual debt:      102931131
position vbUSDC:   100885719
CR %:              198.01283442615627
RESULT_JSON: {"apy":-6.495793539060701,...,"sigma":43.02,"sortino":-0.01,"downsideStd":1003.85}
```

TS side, same window:

```bash
cd ../dlv-sim
bash ../dlv-sim-rust/scripts/run-katana-5bp-2y.sh yarn test
```

## Running long-form parity configs

The KATANA_5BP / arb-mode parity configs export ~20 `BF_*_JSON` env vars
totalling ~3.5 KB. Pasting them into a terminal as a single inline `KEY=val cmd`
chain is fragile — zsh has bitten us in three different ways:

1. **Line-wrap mid-token**: copying a multi-line block from a chat/email can
   put a newline inside `"false"` → JSON unparseable → Rust silently falls
   back to defaults (`SR enabled=false` → `APY −12.72%` instead of `−6.50%`).
2. **Inline `KEY=val` not delivered**: certain zsh hooks (`preexec`, history
   expansion, `unsetopt EQUALS`-style configs) drop the inline assignments
   before the binary sees them. The process gets only its own defaults plus
   whatever you already `export`-ed in a prior session.
3. **Smart quotes** from copy-paste (`'` `'` instead of `'` `'`) break the
   single-quoted JSON.

The fix: always run via a script file. `scripts/run-katana-5bp-2y.sh` puts each
`export KEY=value` on its own physical line, so JSON survives intact, and
invokes the binary with `exec` so nothing in the calling shell can rewrite the
environment.

## Diagnostic: verify the binary sees the config you intend

The binary prints the parsed config at startup. To check **only** the config
and exit without running the simulation (useful when debugging env delivery):

```bash
CONFIG_ONLY=1 bash scripts/run-katana-5bp-2y.sh
```

Expected output:

```
[CONFIG] pool=WBTC_USDC_KATANA_5BP fee=500 dates=2024-04-21..2026-04-21 step=12s arb=true
[CONFIG] slow_recenter={enabled:true min_dev:0.03 interval_s:3600} lev_amm={enabled:true fee:0.005} alm_swap_price_source=binance
[CONFIG_ONLY] exiting without running backtest
```

(`fee=500` in the print is the *pool's base fee* from `pool-config.json`. With
`BF_POOL_FEE=10000` set, strategy.rs overrides to `fee=10000` at runtime; that
override isn't reflected in this banner.)

If any field reads as a default (e.g. `slow_recenter={enabled:false ...}` or
`alm_swap_price_source=5bp`), your env vars aren't reaching the process —
fix the delivery before troubleshooting the math.

## Fixes that took the Rust port from ~9 ppm drift to bit-exact

| Commit | Scope | Symptom |
|--------|-------|---------|
| [`55e4311`](#) | Sort events by `(block_number, log_index)` to match TS's UNION_SQL cursor | DST-boundary rows broke date-vs-chain ordering → ~9 ppm vault drift |
| [`6c54c41`](#) | Risk metrics: log returns + Bessel correction + EARLY/LATE SNAP entry placement | `sigma`/`sortino`/`downsideStd` drifted ~0.5% from population-vs-sample variance + SNAP-vs-ALM ordering when both fire same period |
| [`a20abc6`](#) | Route LevAMM step through `dlvConfig.almSwapPriceSource` (Binance feed) | Rust used pool sqrt; TS uses external Binance sqrt → LevAMM made different fire/noop decisions, cascading into ~50% APY divergence |

The diagnostic env-vars added during this work (all gated, do nothing unless
set):

| Env var | Effect |
|---------|--------|
| `PARITY_CHECK=1` | per-ALM/DLV state snapshot to stderr |
| `TICK_DUMP_START` / `TICK_DUMP_END` | per-tick state dump in a window |
| `REPLAY_TRACE_TICK` / `REPLAY_TRACE_WIN_START` / `..._END` | per-event-replay inputs/outputs |
| `RISK_DIAG=1` | dump rets length/neg/pos/zero/mean/variance |
| `BTC_DUMP_PATH=<path>` | write the full btc_values series |
| `BTC_SRC_DUMP_PATH=<path>` | write `(idx, source, tick, btc)` per entry |
| `ALM_BTC_DIAG=1` | log `alm_pre_sqrt`/`nav`/`p`/`v`/`btc` per ALM fire |
| `ARB_PARITY_START` / `ARB_PARITY_END` | per-step parity log in arb-mode dispatch |
| `CONFIG_ONLY=1` | print parsed config and exit (no simulation) |
| `DAILY_DIAG=1` | every-7200-ticks state dump |
| `TICK_DIAG=N` | first N ticks per-tick state dump |

## Adding new docker-compose parity presets

To add another arb-mode / long-config preset:

1. Create `scripts/<name>.docker.env` in Docker `--env-file` format
   (no quotes around JSON values, no `export` keyword, one `KEY=VALUE`
   per physical line).
2. Add a new service block to `docker-compose.yml`:

   ```yaml
   <name>:
     build: .
     mem_limit: 8g
     volumes:
       - ../dlv-sim/data:/dlv-sim/data:ro
       - ./output:/app/output
     env_file:
       - scripts/<name>.docker.env
   ```

3. Run it:

   ```bash
   docker compose run --rm <name>
   ```

The data volume must mount at `/dlv-sim/data` (not `/app/data`) so the
binary's hardcoded `../dlv-sim/data/...` paths resolve from `WORKDIR /app`.
