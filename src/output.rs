use std::fs;
use std::io::{BufWriter, Write};

use v3_pool::types::{I256, U256};

/// One dashboard row. Field names mirror the TS `RebalanceLog` (LogDBManager
/// schema) so the output JSONL maps 1:1 onto the columns rebalance_plotting.ts
/// reads. Fields the Rust sim does not yet derive (category ③ metrics) are
/// populated with ZERO by the caller until implemented.
#[derive(Debug)]
pub struct RebalanceLogRow {
    pub timestamp_ms: i64,
    pub date: String,
    pub rebalance_type: &'static str,
    /// Raw pool AMM price (WAD) — always pool.sqrtPriceX96 derived.
    pub raw_pool_price: U256,
    /// Valuation price used for this row (WAD); equals raw_pool_price for snapshots.
    pub non_volatile_asset_price: U256,
    pub external_price: f64,
    /// NAV (GAV − virtualDebt) before this period, stable raw units.
    pub prev_total_pool_value: U256,
    /// NAV after this row's state, stable raw units.
    pub after_total_pool_value: U256,
    /// CR as round(percent × 100), e.g. 200% → 20000. NOT WAD.
    pub prev_collateral_ratio: i64,
    pub after_collateral_ratio: i64,
    pub lp_ratio: U256,
    pub swap_fee_stable: U256,
    pub alm_swap_fee_stable: U256,
    pub accumulated_swap_fees0: U256,
    pub accumulated_swap_fees1: U256,
    pub debt: U256,
    pub idle0: U256,
    pub idle1: U256,
    pub total0: U256,
    pub total1: U256,
    pub wide_amount0: U256,
    pub wide_amount1: U256,
    pub base_amount0: U256,
    pub base_amount1: U256,
    pub limit_amount0: U256,
    pub limit_amount1: U256,
    pub lev_amm_collateral: U256,
    pub lev_amm_notional: U256,
    pub lev_amm_debt: U256,
    pub lev_amm_fee_revenue: U256,
    pub volatile_hold_value_stable: U256,
    /// Realized IL in bps (signed).
    pub realized_il: I256,
    pub swap_fees_gained_this_period: U256,
    /// Stable minted (+) / burned (−) by regulateDebt this row.
    pub regulate_debt_amount: I256,
    pub current_pps: U256,
    pub fundamental_pps: U256,
    pub equilibrium_price_wad: U256,
    pub arb_profit_stable: U256,
    pub arb_deviation_bps: f64,
    pub wide0: i32,
    pub wide1: i32,
    pub base0: i32,
    pub base1: i32,
    pub limit0: i32,
    pub limit1: i32,
}

pub struct JsonlWriter {
    writer: BufWriter<fs::File>,
    row_count: usize,
}

impl JsonlWriter {
    pub fn new(path: &str) -> Self {
        let file = fs::File::create(path)
            .unwrap_or_else(|e| panic!("Cannot create {}: {}", path, e));
        JsonlWriter {
            writer: BufWriter::with_capacity(64 * 1024, file),
            row_count: 0,
        }
    }

    pub fn write_row(&mut self, row: &RebalanceLogRow) {
        let json = format!(
            concat!(
                "{{",
                "\"date\":\"{}\",",
                "\"rebalanceType\":\"{}\",",
                "\"rawPoolPrice\":\"{}\",",
                "\"nonVolatileAssetPrice\":\"{}\",",
                "\"externalPrice\":{},",
                "\"prevTotalPoolValue\":\"{}\",",
                "\"afterTotalPoolValue\":\"{}\",",
                "\"prevCollateralRatio\":{},",
                "\"afterCollateralRatio\":{},",
                "\"lpRatio\":\"{}\",",
                "\"swapFeeStable\":\"{}\",",
                "\"almSwapFeeStable\":\"{}\",",
                "\"accumulatedSwapFees0\":\"{}\",",
                "\"accumulatedSwapFees1\":\"{}\",",
                "\"debt\":\"{}\",",
                "\"idle0\":\"{}\",\"idle1\":\"{}\",",
                "\"total0\":\"{}\",\"total1\":\"{}\",",
                "\"wideAmount0\":\"{}\",\"wideAmount1\":\"{}\",",
                "\"baseAmount0\":\"{}\",\"baseAmount1\":\"{}\",",
                "\"limitAmount0\":\"{}\",\"limitAmount1\":\"{}\",",
                "\"levAmmCollateral\":\"{}\",\"levAmmNotional\":\"{}\",",
                "\"levAmmDebt\":\"{}\",\"levAmmFeeRevenue\":\"{}\",",
                "\"volatileHoldValueStable\":\"{}\",",
                "\"realizedIL\":\"{}\",",
                "\"swapFeesGainedThisPeriod\":\"{}\",",
                "\"regulateDebtAmount\":\"{}\",",
                "\"currentPPS\":\"{}\",",
                "\"fundamentalPPS\":\"{}\",",
                "\"equilibriumPriceWad\":\"{}\",",
                "\"arbProfitStable\":\"{}\",",
                "\"arbDeviationBps\":{},",
                "\"wide0\":{},\"wide1\":{},",
                "\"base0\":{},\"base1\":{},",
                "\"limit0\":{},\"limit1\":{}",
                "}}\n"
            ),
            row.date,
            row.rebalance_type,
            row.raw_pool_price.to_dec_string(),
            row.non_volatile_asset_price.to_dec_string(),
            row.external_price,
            row.prev_total_pool_value.to_dec_string(),
            row.after_total_pool_value.to_dec_string(),
            row.prev_collateral_ratio,
            row.after_collateral_ratio,
            row.lp_ratio.to_dec_string(),
            row.swap_fee_stable.to_dec_string(),
            row.alm_swap_fee_stable.to_dec_string(),
            row.accumulated_swap_fees0.to_dec_string(),
            row.accumulated_swap_fees1.to_dec_string(),
            row.debt.to_dec_string(),
            row.idle0.to_dec_string(),
            row.idle1.to_dec_string(),
            row.total0.to_dec_string(),
            row.total1.to_dec_string(),
            row.wide_amount0.to_dec_string(),
            row.wide_amount1.to_dec_string(),
            row.base_amount0.to_dec_string(),
            row.base_amount1.to_dec_string(),
            row.limit_amount0.to_dec_string(),
            row.limit_amount1.to_dec_string(),
            row.lev_amm_collateral.to_dec_string(),
            row.lev_amm_notional.to_dec_string(),
            row.lev_amm_debt.to_dec_string(),
            row.lev_amm_fee_revenue.to_dec_string(),
            row.volatile_hold_value_stable.to_dec_string(),
            row.realized_il.to_dec_string(),
            row.swap_fees_gained_this_period.to_dec_string(),
            row.regulate_debt_amount.to_dec_string(),
            row.current_pps.to_dec_string(),
            row.fundamental_pps.to_dec_string(),
            row.equilibrium_price_wad.to_dec_string(),
            row.arb_profit_stable.to_dec_string(),
            row.arb_deviation_bps,
            row.wide0, row.wide1,
            row.base0, row.base1,
            row.limit0, row.limit1,
        );
        self.writer.write_all(json.as_bytes()).unwrap();
        self.row_count += 1;
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn flush(&mut self) {
        self.writer.flush().unwrap();
    }
}

impl Drop for JsonlWriter {
    fn drop(&mut self) {
        let _ = self.writer.flush();
    }
}

pub fn write_config(path: &str, config: &serde_json::Value) {
    let data = serde_json::to_string_pretty(config).unwrap();
    fs::write(path, data).unwrap_or_else(|e| panic!("Cannot write {}: {}", path, e));
}
