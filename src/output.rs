use std::fs;
use std::io::{BufWriter, Write};

use v3_pool::types::U256;

#[derive(Debug)]
pub struct RebalanceLogRow {
    pub timestamp_ms: i64,
    pub date: String,
    pub rebalance_type: &'static str,
    pub price_wad: U256,
    pub external_price: f64,
    pub total_value_stable: U256,
    pub collateral_ratio_wad: U256,
    pub lp_ratio_wad: U256,
    pub virtual_debt: U256,
    pub idle0: U256,
    pub idle1: U256,
    pub accumulated_fees0: U256,
    pub accumulated_fees1: U256,
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
                "\"price\":\"{}\",",
                "\"externalPrice\":{},",
                "\"totalValueStable\":\"{}\",",
                "\"collateralRatio\":\"{}\",",
                "\"lpRatio\":\"{}\",",
                "\"debt\":\"{}\",",
                "\"idle0\":\"{}\",",
                "\"idle1\":\"{}\",",
                "\"accumulatedFees0\":\"{}\",",
                "\"accumulatedFees1\":\"{}\",",
                "\"arbProfitStable\":\"{}\",",
                "\"arbDeviationBps\":{},",
                "\"wide0\":{},\"wide1\":{},",
                "\"base0\":{},\"base1\":{},",
                "\"limit0\":{},\"limit1\":{}",
                "}}\n"
            ),
            row.date,
            row.rebalance_type,
            row.price_wad.to_dec_string(),
            row.external_price,
            row.total_value_stable.to_dec_string(),
            row.collateral_ratio_wad.to_dec_string(),
            row.lp_ratio_wad.to_dec_string(),
            row.virtual_debt.to_dec_string(),
            row.idle0.to_dec_string(),
            row.idle1.to_dec_string(),
            row.accumulated_fees0.to_dec_string(),
            row.accumulated_fees1.to_dec_string(),
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
