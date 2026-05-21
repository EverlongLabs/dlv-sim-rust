use std::fs;
use std::path::Path;

use v3_pool::types::U256;
use crate::pool_config::PoolConfig;

#[derive(Debug, Clone)]
pub struct PriceEntry {
    pub timestamp_ms: i64,
    pub price: f64,
}

pub struct PriceFeed {
    entries: Vec<PriceEntry>,
    cursor: usize,
    pool_config: PoolConfig,
}

impl PriceFeed {
    pub fn start_ms(&self) -> i64 {
        self.entries.first().map(|e| e.timestamp_ms).unwrap_or(0)
    }

    pub fn end_ms(&self) -> i64 {
        self.entries.last().map(|e| e.timestamp_ms).unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get_price(&self, timestamp_ms: i64) -> f64 {
        self.lookup(timestamp_ms).price
    }

    pub fn get_price_at(&self, timestamp_ms: i64) -> (f64, U256) {
        let price = self.lookup(timestamp_ms).price;
        let sqrt = price_to_sqrt_price_x96(price, &self.pool_config);
        (price, sqrt)
    }

    pub fn get_price_at_monotonic(&mut self, timestamp_ms: i64) -> (f64, U256) {
        let price = self.lookup_monotonic(timestamp_ms).price;
        let sqrt = price_to_sqrt_price_x96(price, &self.pool_config);
        (price, sqrt)
    }

    pub fn reset_cursor(&mut self) {
        self.cursor = 0;
    }

    fn lookup(&self, target_ms: i64) -> &PriceEntry {
        if self.entries.is_empty() {
            panic!("Empty price feed");
        }
        if target_ms <= self.entries[0].timestamp_ms {
            return &self.entries[0];
        }
        let last = self.entries.len() - 1;
        if target_ms >= self.entries[last].timestamp_ms {
            return &self.entries[last];
        }
        let idx = self.entries.partition_point(|e| e.timestamp_ms <= target_ms);
        &self.entries[if idx > 0 { idx - 1 } else { 0 }]
    }

    fn lookup_monotonic(&mut self, target_ms: i64) -> &PriceEntry {
        let last = self.entries.len() - 1;
        if target_ms <= self.entries[0].timestamp_ms {
            return &self.entries[0];
        }
        if target_ms >= self.entries[last].timestamp_ms {
            return &self.entries[last];
        }
        if self.cursor > last || target_ms < self.entries[self.cursor].timestamp_ms {
            let idx = self.entries.partition_point(|e| e.timestamp_ms <= target_ms);
            self.cursor = if idx > 0 { idx - 1 } else { 0 };
            return &self.entries[self.cursor];
        }
        while self.cursor < last && self.entries[self.cursor + 1].timestamp_ms <= target_ms {
            self.cursor += 1;
        }
        &self.entries[self.cursor]
    }
}

pub fn load_price_feed(
    directory: &str,
    start_ms: i64,
    end_ms: i64,
    sampling_interval_ms: i64,
    pool_config: &PoolConfig,
) -> PriceFeed {
    let dir = Path::new(directory);
    if !dir.exists() {
        panic!("Price feed directory not found: {}", directory);
    }

    let mut csv_files: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Cannot read {}: {}", directory, e))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            if name.ends_with(".csv") { Some(name) } else { None }
        })
        .collect();
    csv_files.sort();

    if csv_files.is_empty() {
        panic!("No CSV files found in {}. Run binance_downloader first.", directory);
    }

    let mut entries: Vec<PriceEntry> = Vec::new();

    for csv_file in &csv_files {
        let file_path = dir.join(csv_file);
        let content = fs::read_to_string(&file_path)
            .unwrap_or_else(|e| panic!("Cannot read {}: {}", file_path.display(), e));

        for line in content.lines() {
            if line.is_empty() { continue; }
            let cols: Vec<&str> = line.splitn(6, ',').collect();
            if cols.len() < 5 { continue; }

            let mut timestamp_ms: i64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Binance uses microsecond timestamps for some pairs since 2025
            if timestamp_ms > 1_000_000_000_000_000 {
                timestamp_ms /= 1000;
            }

            if timestamp_ms < start_ms || timestamp_ms > end_ms { continue; }

            let close: f64 = match cols[4].parse() {
                Ok(v) if v > 0.0 => v,
                _ => continue,
            };

            if sampling_interval_ms > 0 && !entries.is_empty() {
                let dt = timestamp_ms - entries.last().unwrap().timestamp_ms;
                if dt < sampling_interval_ms { continue; }
            }

            entries.push(PriceEntry { timestamp_ms, price: close });
        }
    }

    if entries.is_empty() {
        panic!(
            "No price data found in {} for range {}..{}",
            directory, start_ms, end_ms,
        );
    }

    entries.sort_by_key(|e| e.timestamp_ms);

    println!(
        "[PriceFeed] Loaded {} entries from {} to {}",
        entries.len(),
        entries.first().unwrap().timestamp_ms,
        entries.last().unwrap().timestamp_ms,
    );

    PriceFeed {
        entries,
        cursor: 0,
        pool_config: pool_config.clone(),
    }
}

pub fn price_to_sqrt_price_x96(usd_price: f64, pool_config: &PoolConfig) -> U256 {
    if usd_price <= 0.0 {
        return U256::ZERO;
    }

    let volatile_decimals = pool_config.volatile_decimals() as i32;
    let stable_decimals = pool_config.stable_decimals() as i32;

    // price in token1-per-token0 raw units
    // If volatile is token1: priceRaw = 1/usd_price * 10^(token1_dec - token0_dec)
    // If volatile is token0: priceRaw = usd_price * 10^(token1_dec - token0_dec)
    let price_raw_t1_per_t0: f64 = if pool_config.is_volatile_token0() {
        // token0=volatile(BTC), token1=stable(USDC)
        // price is USDC per BTC, raw = price * 10^(stable_dec - volatile_dec)
        usd_price * 10f64.powi(stable_decimals - volatile_decimals)
    } else {
        // token0=stable(USDC), token1=volatile(BTC)
        // price is USDC per BTC = 1/rate, but v3 price is token1/token0
        // so sqrtPrice corresponds to price = token1_raw / token0_raw
        // which is (1/usd_price) * 10^(volatile_dec - stable_dec)
        (1.0 / usd_price) * 10f64.powi(volatile_decimals - stable_decimals)
    };

    // sqrtPriceX96 = sqrt(priceRawT1PerT0) * 2^96
    let q96 = 2.0f64.powi(96);
    let sqrt_val = price_raw_t1_per_t0.sqrt() * q96;
    let rounded = sqrt_val.round() as u128;
    U256::from_u128(rounded)
}
