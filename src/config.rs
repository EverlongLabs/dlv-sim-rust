use chrono::NaiveDate;

use crate::enums::LookUpPeriod;
use crate::pool_config::{self, PoolConfig};

#[derive(Debug, Clone)]
pub struct VaultParams {
    pub wide_range_weight: u64,
    pub wide_threshold: i32,
    pub base_threshold: i32,
    pub limit_threshold: i32,
    pub period: u64,
}

#[derive(Debug, Clone)]
pub struct DlvParams {
    pub period: Option<u64>,
    pub deviation_threshold_above: Option<f64>,
    pub deviation_threshold_below: Option<f64>,
    pub debt_to_volatile_swap_fee: f64,
}

#[derive(Debug, Clone)]
pub struct ArbParams {
    pub price_feed_dir: String,
    pub mode: ArbMode,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbMode {
    CloseGap,
    Optimal,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub pool_selection: String,
    pub pool_config: PoolConfig,
    pub lookup_period: u32,
    pub is_regulate_debt: bool,
    pub is_alm_only: bool,
    pub is_arb_strategy: bool,
    pub target_cr_pct: u32,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub charm: VaultParams,
    pub dlv: DlvParams,
    pub arb: ArbParams,
    pub active_rebalance_ratio_deviation_bps: u32,
    pub max_ticks: Option<u64>,
}

impl Config {
    pub fn from_env() -> Self {
        let pool_selection = std::env::var("BF_POOL")
            .unwrap_or_else(|_| "CBBTC_USDC_BASE".into());
        let pool_config = pool_config::pool_by_name(&pool_selection);

        let lookup_period: u32 = std::env::var("LOOKUP_PERIOD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(LookUpPeriod::FiveSeconds.as_secs());

        let is_regulate_debt = std::env::var("IS_REGULATE_DEBT")
            .map(|s| s.to_lowercase() != "false")
            .unwrap_or(true);

        let is_alm_only = std::env::var("ALM_ONLY")
            .map(|s| s.to_lowercase() == "true")
            .unwrap_or(false);

        let is_arb_strategy = std::env::var("ARB_STRATEGY")
            .map(|s| s == "true")
            .unwrap_or(false);

        let target_cr_pct: u32 = std::env::var("TARGET_CR_PCT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(200);

        let start_date = std::env::var("BF_START_DATE")
            .ok()
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(2025, 9, 9).unwrap());

        let end_date = std::env::var("BF_END_DATE")
            .ok()
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 3, 6).unwrap());

        let charm = parse_charm_env();
        let dlv = parse_dlv_env();

        let price_feed_dir = std::env::var("BF_ARB_PRICE_DIR")
            .unwrap_or_else(|_| "data/binance/BTCUSDT".into());
        let arb_mode = match std::env::var("ARB_MODE").unwrap_or_default().as_str() {
            "optimal" => ArbMode::Optimal,
            _ => ArbMode::CloseGap,
        };
        let arb = ArbParams {
            price_feed_dir,
            mode: arb_mode,
            start_date,
            end_date,
        };

        let active_rebalance_ratio_deviation_bps: u32 = std::env::var("ACTIVE_REBALANCE_RATIO_DEVIATION_BPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);

        let max_ticks: Option<u64> = std::env::var("BF_MAX_TICKS")
            .ok()
            .and_then(|s| s.parse().ok());

        Config {
            pool_selection,
            pool_config,
            lookup_period,
            is_regulate_debt,
            is_alm_only,
            is_arb_strategy,
            target_cr_pct,
            start_date,
            end_date,
            charm,
            dlv,
            arb,
            active_rebalance_ratio_deviation_bps,
            max_ticks,
        }
    }

    pub fn target_cr_wad(&self) -> u128 {
        (self.target_cr_pct as u128) * 1_000_000_000_000_000_000 / 100
    }
}

fn parse_charm_env() -> VaultParams {
    if let Ok(json) = std::env::var("BF_CHARM_JSON") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            return VaultParams {
                wide_range_weight: v["wideRangeWeight"].as_u64().unwrap_or(0),
                wide_threshold: v["wideThreshold"].as_i64().unwrap_or(18000) as i32,
                base_threshold: v["baseThreshold"].as_i64().unwrap_or(1020) as i32,
                limit_threshold: v["limitThreshold"].as_i64().unwrap_or(300) as i32,
                period: v["period"].as_u64().unwrap_or(43200 * 200000),
            };
        }
    }
    VaultParams {
        wide_range_weight: 0,
        wide_threshold: 18000,
        base_threshold: 1020,
        limit_threshold: 300,
        period: 43200 * 200000,
    }
}

fn parse_dlv_env() -> DlvParams {
    if let Ok(json) = std::env::var("BF_DLV_JSON") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            return DlvParams {
                period: v["period"].as_u64(),
                deviation_threshold_above: v["deviationThresholdAbove"].as_f64(),
                deviation_threshold_below: v["deviationThresholdBelow"].as_f64(),
                debt_to_volatile_swap_fee: v["debtToVolatileSwapFee"].as_f64().unwrap_or(0.0015),
            };
        }
    }
    DlvParams {
        period: None,
        deviation_threshold_above: Some(0.01),
        deviation_threshold_below: Some(0.01),
        debt_to_volatile_swap_fee: 0.0015,
    }
}
