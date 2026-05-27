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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveRebalanceMode {
    Active,
    Passive,
    Hybrid,
}

#[derive(Debug, Clone)]
pub struct LevAmmConfig {
    pub enabled: bool,
    pub swap_fee: f64,
    pub max_arb_per_tick_frac: f64,
}

#[derive(Debug, Clone)]
pub struct SlowRecenterConfig {
    pub enabled: bool,
    pub min_deviation: f64,
    pub max_shift_per_step: i32,
    pub acceleration_threshold: f64,
    pub acceleration_multiplier: f64,
    pub emergency_threshold: f64,
    pub trigger_interval_seconds: u64,
    pub only_shift_oor: bool,
    pub redeploy_limit_at_current_tick: bool,
}

#[derive(Debug, Clone)]
pub struct RdTuningConfig {
    pub mint_fraction: f64,
    pub burn_fraction: f64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub pool_selection: String,
    pub pool_config: PoolConfig,
    pub pool_fee_override: Option<u32>,
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
    pub active_rebalance_mode: ActiveRebalanceMode,
    pub max_ticks: Option<u64>,
    pub no_arb_donation: bool,
    pub use_asymmetric_deleverage: bool,
    pub use_dynamic_width: bool,
    pub volatility_window_minutes: u32,
    pub use_fee_recycling: bool,
    pub lev_amm: LevAmmConfig,
    pub slow_recenter: SlowRecenterConfig,
    pub rd_tuning: RdTuningConfig,
}

impl Config {
    pub fn from_env() -> Self {
        let pool_selection = std::env::var("BF_POOL")
            .unwrap_or_else(|_| "CBBTC_USDC_BASE".into());
        let pool_config = pool_config::pool_by_name(&pool_selection);

        let pool_fee_override: Option<u32> = std::env::var("BF_POOL_FEE")
            .ok()
            .and_then(|s| s.parse().ok());

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
        let arb = parse_arb_env(start_date, end_date);

        let active_rebalance_ratio_deviation_bps: u32 = std::env::var("ACTIVE_REBALANCE_RATIO_DEVIATION_BPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100);

        let active_rebalance_mode = match std::env::var("ACTIVE_REBALANCE_MODE")
            .unwrap_or_default().to_lowercase().as_str()
        {
            "passive" => ActiveRebalanceMode::Passive,
            "hybrid" => ActiveRebalanceMode::Hybrid,
            _ => ActiveRebalanceMode::Active,
        };

        let max_ticks: Option<u64> = std::env::var("BF_MAX_TICKS")
            .ok()
            .and_then(|s| s.parse().ok());

        let no_arb_donation = std::env::var("NO_ARB_DONATION")
            .map(|s| s == "true")
            .unwrap_or(false);

        let use_asymmetric_deleverage = std::env::var("USE_ASYMMETRIC_DELEVERAGE")
            .map(|s| s.to_lowercase() != "false")
            .unwrap_or(true);

        let use_dynamic_width = std::env::var("USE_DYNAMIC_WIDTH")
            .map(|s| s == "true")
            .unwrap_or(false);

        let volatility_window_minutes: u32 = std::env::var("VOLATILITY_WINDOW_MINUTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1440);

        let use_fee_recycling = std::env::var("USE_FEE_RECYCLING")
            .map(|s| s.to_lowercase() != "false")
            .unwrap_or(true);

        let lev_amm = parse_lev_amm_env();
        let slow_recenter = parse_slow_recenter_env();
        let rd_tuning = parse_rd_tuning_env();

        Config {
            pool_selection,
            pool_config,
            pool_fee_override,
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
            active_rebalance_mode,
            max_ticks,
            no_arb_donation,
            use_asymmetric_deleverage,
            use_dynamic_width,
            volatility_window_minutes,
            use_fee_recycling,
            lev_amm,
            slow_recenter,
            rd_tuning,
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

fn resolve_data_path(path: &str) -> String {
    if path.starts_with("data/") {
        format!("../dlv-sim/{}", path)
    } else {
        path.to_string()
    }
}

fn parse_arb_env(default_start: NaiveDate, default_end: NaiveDate) -> ArbParams {
    if let Ok(json) = std::env::var("BF_ARB_JSON") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            let raw_dir = v["priceFeedDir"].as_str()
                .unwrap_or("data/binance/BTCUSDT");
            let price_feed_dir = resolve_data_path(raw_dir);
            let mode = match v["mode"].as_str().unwrap_or("close_gap") {
                "optimal" => ArbMode::Optimal,
                _ => ArbMode::CloseGap,
            };
            let start_date = v["startDate"].as_str()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                .unwrap_or(default_start);
            let end_date = v["endDate"].as_str()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                .unwrap_or(default_end);
            return ArbParams { price_feed_dir, mode, start_date, end_date };
        }
    }
    let price_feed_dir = std::env::var("BF_ARB_PRICE_DIR")
        .unwrap_or_else(|_| "../dlv-sim/data/binance/BTCUSDT".into());
    let mode = match std::env::var("ARB_MODE").unwrap_or_default().as_str() {
        "optimal" => ArbMode::Optimal,
        _ => ArbMode::CloseGap,
    };
    ArbParams { price_feed_dir, mode, start_date: default_start, end_date: default_end }
}

fn parse_lev_amm_env() -> LevAmmConfig {
    let use_lev_amm = std::env::var("USE_LEV_AMM")
        .map(|s| s == "true")
        .unwrap_or(false);
    if let Ok(json) = std::env::var("BF_LEV_AMM_JSON") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            return LevAmmConfig {
                enabled: v["enabled"].as_bool().unwrap_or(false) || use_lev_amm,
                swap_fee: v["swapFee"].as_f64().unwrap_or(0.005),
                max_arb_per_tick_frac: v["maxArbPerTickFrac"].as_f64().unwrap_or(1.0),
            };
        }
    }
    LevAmmConfig { enabled: use_lev_amm, swap_fee: 0.005, max_arb_per_tick_frac: 1.0 }
}

fn parse_slow_recenter_env() -> SlowRecenterConfig {
    if let Ok(json) = std::env::var("BF_SLOW_RECENTER_JSON") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            return SlowRecenterConfig {
                enabled: v["enabled"].as_bool().unwrap_or(false),
                min_deviation: v["minDeviation"].as_f64().unwrap_or(0.05),
                max_shift_per_step: v["maxShiftPerStep"].as_i64().unwrap_or(2) as i32,
                acceleration_threshold: v["accelerationThreshold"].as_f64().unwrap_or(0.3),
                acceleration_multiplier: v["accelerationMultiplier"].as_f64().unwrap_or(2.0),
                emergency_threshold: v["emergencyThreshold"].as_f64().unwrap_or(0.8),
                trigger_interval_seconds: v["triggerIntervalSeconds"].as_u64().unwrap_or(3600),
                only_shift_oor: v["onlyShiftOOR"].as_bool().unwrap_or(false),
                redeploy_limit_at_current_tick: v["redeployLimitAtCurrentTick"].as_bool().unwrap_or(false),
            };
        }
    }
    SlowRecenterConfig {
        enabled: false, min_deviation: 0.05, max_shift_per_step: 2,
        acceleration_threshold: 0.3, acceleration_multiplier: 2.0,
        emergency_threshold: 0.8, trigger_interval_seconds: 3600,
        only_shift_oor: false, redeploy_limit_at_current_tick: false,
    }
}

fn parse_rd_tuning_env() -> RdTuningConfig {
    if let Ok(json) = std::env::var("BF_RD_TUNING_JSON") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
            return RdTuningConfig {
                mint_fraction: v["mintFraction"].as_f64().unwrap_or(1.0),
                burn_fraction: v["burnFraction"].as_f64().unwrap_or(1.0),
            };
        }
    }
    RdTuningConfig { mint_fraction: 1.0, burn_fraction: 1.0 }
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
