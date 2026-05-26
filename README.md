# DLV Simulator (Rust)

Rust port of the TypeScript DLV vault simulator. Produces matching output given the same parquet input data.

## Prerequisites

- Docker & Docker Compose
- Pool data files in `../dlv-sim/data/` (shared with the TS simulator)

## Running the Comparison

### cbBTC/USDC (Base)

```bash
# Rust
docker compose run --rm -e BF_POOL=cbbtc_usdc_base -e ARB_STRATEGY=false dlv-sim 2>&1 | grep RESULT_JSON

# TypeScript
cd ../dlv-sim && docker compose run --rm \
  -e BF_POOL=cbbtc_usdc_base \
  -e BENCHMARK=false \
  dlv-sim sh -c "yarn test 2>&1 | grep RESULT_JSON"
```

### WBTC/USDC (Ethereum)

```bash
# Rust
docker compose run --rm -e BF_POOL=wbtc_usdc -e ARB_STRATEGY=false dlv-sim 2>&1 | grep RESULT_JSON

# TypeScript
cd ../dlv-sim && docker compose run --rm \
  -e BF_POOL=wbtc_usdc \
  -e BENCHMARK=false \
  dlv-sim sh -c "yarn test 2>&1 | grep RESULT_JSON"
```

### Quick Local Run (no Docker)

```bash
# Rust
BF_POOL=wbtc_usdc cargo run --release
BF_POOL=cbbtc_usdc_base cargo run --release

# TypeScript
cd ../dlv-sim
BF_POOL=wbtc_usdc yarn test
BF_POOL=cbbtc_usdc_base yarn test
```

## Comparison Matrix

Results from full 6-month backtest (2025-09-09 to 2026-03-06, 5s periods).

### WBTC/USDC

| Metric             | Rust                   | TypeScript             | Match   |
|--------------------|------------------------|------------------------|---------|
| apy                | -22.386262620678497    | -22.386262620678497    | exact   |
| totalReturn        | -11.623095898574364    | -11.623095898574364    | exact   |
| minCR              | 197.99                 | 197.99                 | exact   |
| maxDrawdown        | -12.51                 | -12.51                 | exact   |
| worstMonthReturn   | -6                     | -6                     | exact   |
| liquidated         | false                  | false                  | exact   |
| sortinoRatio       | -2.016                 | -2.016                 | exact   |
| sharpeRatio        | -2.317                 | -2.317                 | exact   |
| downsideDeviation  | 3.206                  | 3.206                  | exact   |
| monthlyReturnStdev | 2.789                  | 2.789                  | exact   |
| sigma              | 91.96                  | 91.96                  | exact   |
| sortino            | -57.38                 | -57.44                 | ~0.1%   |
| downsideStd        | 145.7                  | 145.55                 | ~0.1%   |

### cbBTC/USDC (Base)

| Metric             | Rust                   | TypeScript             | Match   |
|--------------------|------------------------|------------------------|---------|
| apy                | -24.536709815578163    | -24.536709815578163    | exact   |
| totalReturn        | -12.82718107523636     | -12.82718107523636     | exact   |
| minCR              | 197.99                 | 197.99                 | exact   |
| maxDrawdown        | -16.44                 | -16.44                 | exact   |
| worstMonthReturn   | -14.54                 | -14.54                 | exact   |
| liquidated         | false                  | false                  | exact   |
| sortinoRatio       | -1.288                 | -1.288                 | exact   |
| sharpeRatio        | -1.355                 | -1.355                 | exact   |
| downsideDeviation  | 5.501                  | 5.501                  | exact   |
| monthlyReturnStdev | 5.229                  | 5.229                  | exact   |
| sigma              | 75.45                  | 75.45                  | exact   |
| sortino            | -70.63                 | -70.68                 | ~0.1%   |
| downsideStd        | 116.07                 | 115.99                 | ~0.1%   |

11 of 13 metrics match exactly for both pools. The remaining `sortino` and `downsideStd`
differ by ~0.1% due to f64 precision at the boundary between negative and non-negative
log returns (7 returns out of ~9300 flip sign).
