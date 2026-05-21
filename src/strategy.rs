use chrono::NaiveDate;
use std::collections::HashMap;

use v3_pool::core_pool::CorePool;
use v3_pool::full_math;
use v3_pool::tick_manager::TickManager;
use v3_pool::position_manager::PositionManager;
use v3_pool::types::*;

use crate::arb;
use crate::config::Config;
use crate::event_reader::{self, EventReader};
use crate::output::{JsonlWriter, RebalanceLogRow};
use crate::price_feed;
use crate::vault::Vault;

pub struct BacktestResult {
    pub row_count: usize,
    pub apy: f64,
    pub total_return: f64,
}

pub fn run_backtest(cfg: &Config) -> BacktestResult {
    let mut event_reader = EventReader::new(&cfg.pool_config.db_path);
    if !event_reader.exists() {
        panic!(
            "Parquet file not found for {}. Run the TS simulator first to generate it.",
            cfg.pool_config.db_path
        );
    }

    let (_pool_id, fee, tick_spacing) = event_reader.get_pool_config();
    let initial_sqrt = event_reader.get_initial_sqrt_price_x96();

    let mut pool = CorePool::new(
        cfg.pool_config.token0.address.clone(),
        cfg.pool_config.token1.address.clone(),
        fee,
        tick_spacing,
        U256::ZERO,
        U256::ZERO,
        U256::ZERO,
        U256::ZERO,
        0,
        U256::ZERO,
        U256::ZERO,
        TickManager::new(),
        PositionManager::new(),
    );
    pool.initialize(initial_sqrt);

    let start_ms = date_to_ms(cfg.start_date);
    let end_ms = date_to_ms(cfg.end_date);

    let sampling_ms = (cfg.lookup_period as i64) * 1000;
    let mut price_feed = price_feed::load_price_feed(
        &cfg.arb.price_feed_dir,
        start_ms,
        end_ms,
        sampling_ms,
        &cfg.pool_config,
    );

    // Position pool at start-date price from external feed
    {
        let (start_price, start_sqrt) = price_feed.get_price_at(start_ms);
        let current_sqrt = pool.sqrt_price_x96();
        if !start_sqrt.is_zero() && start_sqrt != current_sqrt {
            let zero_for_one = start_sqrt < current_sqrt;
            let valid = if zero_for_one {
                start_sqrt > MIN_SQRT_RATIO
            } else {
                start_sqrt < MAX_SQRT_RATIO
            };
            if valid {
                let large = I256(U256::new(u128::MAX >> 1, 0));
                pool.swap(zero_for_one, large, Some(start_sqrt));
            }
            println!(
                "[POSITION] Pool moved to tick={} sqrtPrice={} (ext ${:.2})",
                pool.tick_current(),
                pool.sqrt_price_x96().to_dec_string(),
                start_price,
            );
        }
    }

    // Initialize vault
    let mut vault = Vault::new(cfg.charm.clone(), cfg.pool_config.clone(), tick_spacing);

    // Initial deposit: compute from pool price
    let price_wad = Vault::pool_price(pool.sqrt_price_x96());
    let decimal_adj = 10f64.powi(
        cfg.pool_config.volatile_decimals() as i32 - cfg.pool_config.stable_decimals() as i32,
    );
    let price_raw = price_wad.lo as f64 / 1e18;
    let price_human = if cfg.pool_config.is_volatile_token0() {
        price_raw * decimal_adj
    } else {
        if price_raw > 0.0 {
            decimal_adj / price_raw
        } else {
            0.0
        }
    };
    let target_usd = 100.0;
    let volatile_tokens = if price_human > 0.0 {
        target_usd / price_human
    } else {
        0.0
    };
    let volatile_raw = if volatile_tokens.is_finite() {
        (volatile_tokens * 10f64.powi(cfg.pool_config.volatile_decimals() as i32)) as u128
    } else {
        0
    };

    let (amount0, amount1) = if cfg.pool_config.is_volatile_token0() {
        (U256::from_u128(volatile_raw), U256::ZERO)
    } else {
        (U256::ZERO, U256::from_u128(volatile_raw))
    };
    let stable_raw = (target_usd * 10f64.powi(cfg.pool_config.stable_decimals() as i32)) as u128;
    let (a0, a1) = if cfg.pool_config.is_volatile_token0() {
        (amount0, U256::from_u128(stable_raw))
    } else {
        (U256::from_u128(stable_raw), amount1)
    };

    vault.deposit(&mut pool, a0, a1);
    println!(
        "[INIT] Deposited {}={} {}={} at price ${:.2}",
        cfg.pool_config.volatile_symbol(),
        volatile_raw,
        cfg.pool_config.stable_symbol(),
        stable_raw,
        price_human,
    );

    // Initialize debt (matching TS initializeAccountVault → rebalanceDebt)
    let target_cr_wad = U256::from_u128(cfg.target_cr_wad());
    if !cfg.is_alm_only && cfg.is_regulate_debt {
        vault.rebalance_debt(&mut pool, target_cr_wad);
        let (t0, t1) = vault.total_amounts(&pool);
        let init_gav = vault.total_value_in_stable(&pool, None);
        let init_nav = vault.total_pool_value(&pool, None);
        println!(
            "[INIT-DEBUG] debt={} GAV={} NAV={} total0={} total1={} idle0={} idle1={} CR={:.2}%",
            vault.virtual_debt.to_dec_string(), init_gav.to_dec_string(), init_nav.to_dec_string(),
            t0.to_dec_string(), t1.to_dec_string(),
            vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
            vault.collateral_ratio_pct(&pool, None),
        );
        println!(
            "[INIT] Debt initialized: {} {}, CR: {:.2}%",
            vault.virtual_debt.to_dec_string(),
            cfg.pool_config.stable_symbol(),
            vault.collateral_ratio_pct(&pool, None),
        );
    }

    // Output writer
    let output_path = format!("output/output_{}.jsonl", cfg.pool_selection.to_lowercase());
    let mut writer = JsonlWriter::new(&output_path);

    // Counters (matching TS output)
    let mut tick_count: u64 = 0;
    let mut alm_calls: u64 = 0;
    let mut dlv_calls: u64 = 0;
    let mut regulate_debt_calls: u64 = 0;
    let mut arb_calls: u64 = 0;
    let w_const = U256::from_u128(1_000_000_000_000_000_000);

    // APY tracking (uses volatile_price_wad, matching TS calculateAPY)
    let mut first_apy_value: Option<(f64, f64)> = None;
    let mut last_apy_value: Option<(f64, f64)> = None;

    // Risk metrics tracking (uses external price, matching TS computeRiskMetrics)
    let mut btc_values_for_risk: Vec<f64> = Vec::new();
    let mut min_cr: f64 = f64::INFINITY;
    let mut peak_btc_value: f64 = 0.0;
    let mut max_drawdown_pct: f64 = 0.0;
    let mut monthly_btc_values: HashMap<String, (f64, f64)> = HashMap::new();

    // Main backtest loop
    let step_ms = (cfg.lookup_period as i64) * 1000;
    let mut curr_ms = start_ms;

    while curr_ms < end_ms {
        tick_count += 1;

        // Get external price
        let (ext_price, ext_sqrt) = price_feed.get_price_at_monotonic(curr_ms);

        // Arb: position pool at external price
        let mut arb_profit = U256::ZERO;
        let mut arb_dev_bps = 0.0f64;

        if ext_price > 0.0 {
            let detection = arb::detect_arb(pool.sqrt_price_x96(), ext_sqrt, fee);
            arb_dev_bps = detection.deviation_bps;
            if detection.is_arbitrable {
                let current = pool.sqrt_price_x96();
                let target = detection.target_sqrt_price_x96;
                let valid = if detection.zero_for_one {
                    target < current && target > MIN_SQRT_RATIO
                } else {
                    target > current && target < MAX_SQRT_RATIO
                };
                if valid {
                    let result = arb::execute_arb_close_gap(&mut pool, &detection, cfg.pool_config.is_volatile_token0());
                    if cfg.is_arb_strategy {
                        arb_profit = result.profit_stable.abs();
                        // Donate arb profit back to vault idle (matching TS)
                        if result.profit_stable.is_positive() && !arb_profit.is_zero() {
                            if cfg.pool_config.is_volatile_token0() {
                                vault.idle1 = vault.idle1 + arb_profit;
                            } else {
                                vault.idle0 = vault.idle0 + arb_profit;
                            }
                        }
                    }
                    arb_calls += 1;
                }
            }
        }

        // Collect fees
        vault.collect_fees(&mut pool);

        // Regulate debt every period (matching TS: called unconditionally each tick)
        if cfg.is_regulate_debt && !vault.virtual_debt.is_zero() {
            let debt_before = vault.virtual_debt;
            let rd_mode = vault.regulate_debt(&pool, Some(ext_sqrt), target_cr_wad);
            if rd_mode == "mint" {
                vault.deploy_idle_to_lp(&mut pool);
            }
            regulate_debt_calls += 1;
            if tick_count <= 5 {
                let (t0, t1) = vault.total_amounts(&pool);
                let d_delta = if vault.virtual_debt > debt_before { vault.virtual_debt - debt_before } else { debt_before - vault.virtual_debt };
                let mode = if vault.virtual_debt > debt_before { "MINT" } else if vault.virtual_debt < debt_before { "BURN" } else { "NOOP" };
                println!(
                    "[RD-DEBUG t={}] {} delta={} debt={} GAV={} idle0={} idle1={} t0={} t1={} CR={:.2}%",
                    tick_count, mode, d_delta.to_dec_string(),
                    vault.virtual_debt.to_dec_string(),
                    vault.total_value_in_stable(&pool, Some(ext_sqrt)).to_dec_string(),
                    vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                    t0.to_dec_string(), t1.to_dec_string(),
                    vault.collateral_ratio_pct(&pool, Some(ext_sqrt)),
                );
            }
        }

        // ALM rebalance: time-based OR ratio-deviation (matching TS shouldForceActiveRebalance)
        let time_since_last = curr_ms - vault.last_rebalance_ms;
        let time_trigger = vault.last_rebalance_ms == 0
            || time_since_last >= (vault.params.period as i64 * 1000);
        let ratio_trigger = if cfg.active_rebalance_ratio_deviation_bps > 0 {
            let dev_bps = vault.share_deviation_bps(&pool, Some(ext_sqrt));
            dev_bps >= cfg.active_rebalance_ratio_deviation_bps as u64
        } else {
            false
        };
        if time_trigger || ratio_trigger {
            vault.withdraw_all(&mut pool);
            if ratio_trigger {
                vault.active_rebalance_swap(Some(ext_sqrt), &pool, cfg.dlv.debt_to_volatile_swap_fee);
            }
            if cfg.is_regulate_debt && !vault.virtual_debt.is_zero() {
                vault.regulate_debt(&pool, Some(ext_sqrt), target_cr_wad);
            }
            vault.rebalance_from_idle(&mut pool);
            vault.last_rebalance_ms = curr_ms;
            alm_calls += 1;
        }

        // DLV rebalance trigger: when CR deviates beyond thresholds, do full
        // withdrawal + debt regulation + redeploy (matching TS DLV_CALLS).
        // Cooldown: only trigger if enough time has passed since last DLV.
        if cfg.is_regulate_debt && !vault.virtual_debt.is_zero() {
            let dlv_cooldown_ms = cfg.dlv.period.unwrap_or(vault.params.period) as i64 * 1000;
            let dlv_time_since = curr_ms - vault.last_debt_rebalance_ms;
            if vault.last_debt_rebalance_ms == 0 || dlv_time_since >= dlv_cooldown_ms {
                let cr = vault.collateral_ratio_wad(&pool, Some(ext_sqrt));
                let mut needs_dlv = false;
                if let Some(above) = cfg.dlv.deviation_threshold_above {
                    let above_wad = U256::from_u128((above * 1e18) as u128);
                    let threshold =
                        target_cr_wad + full_math::mul_div(target_cr_wad, above_wad, w_const);
                    if cr > threshold {
                        needs_dlv = true;
                    }
                }
                if !needs_dlv {
                    if let Some(below) = cfg.dlv.deviation_threshold_below {
                        let below_wad = U256::from_u128((below * 1e18) as u128);
                        let threshold =
                            target_cr_wad - full_math::mul_div(target_cr_wad, below_wad, w_const);
                        if cr < threshold {
                            needs_dlv = true;
                        }
                    }
                }
                if needs_dlv {
                    vault.withdraw_all(&mut pool);
                    vault.regulate_debt(&pool, Some(ext_sqrt), target_cr_wad);
                    vault.rebalance_from_idle(&mut pool);
                    vault.last_rebalance_ms = curr_ms;
                    vault.last_debt_rebalance_ms = curr_ms;
                    dlv_calls += 1;
                }
            }
        }

        // Snapshot values
        let nav = vault.total_pool_value(&pool, Some(ext_sqrt));
        let value_stable = vault.total_value_in_stable(&pool, Some(ext_sqrt));
        let vol_price_wad =
            Vault::volatile_price_wad(ext_sqrt, cfg.pool_config.is_volatile_token0());
        let cr_pct = vault.collateral_ratio_pct(&pool, Some(ext_sqrt));
        let snap_price_wad = Vault::pool_price(pool.sqrt_price_x96());
        let cr_wad = vault.collateral_ratio_wad(&pool, Some(ext_sqrt));
        let lp_ratio = vault.lp_ratio(&pool, Some(ext_sqrt));

        let vault_value_f =
            nav.lo as f64 / 10f64.powi(cfg.pool_config.stable_decimals() as i32);
        let price_f = vol_price_wad.lo as f64 / 1e18;

        // APY tracking (pool-derived price, matching TS calculateAPY)
        if price_f > 0.0 {
            let btc_value = vault_value_f / price_f;
            if first_apy_value.is_none() {
                first_apy_value = Some((btc_value, curr_ms as f64));
            }
            last_apy_value = Some((btc_value, curr_ms as f64));
        }

        // Risk metrics tracking (external price, matching TS registerLogSummary)
        if ext_price > 0.0 {
            let btc_value = vault_value_f / ext_price;

            if btc_value > 0.0 && btc_value.is_finite() {
                btc_values_for_risk.push(btc_value);

                if btc_value > peak_btc_value {
                    peak_btc_value = btc_value;
                }
                if peak_btc_value > 0.0 {
                    let dd = ((btc_value - peak_btc_value) / peak_btc_value) * 100.0;
                    if dd < max_drawdown_pct {
                        max_drawdown_pct = dd;
                    }
                }

                let date_str = event_reader::fmt_utc_date(curr_ms);
                let month_key = date_str[..7].to_string();
                monthly_btc_values
                    .entry(month_key)
                    .and_modify(|e| e.1 = btc_value)
                    .or_insert((btc_value, btc_value));
            }
        }

        if cr_pct.is_finite() && cr_pct > 0.0 && cr_pct < min_cr {
            min_cr = cr_pct;
        }

        let date_str = event_reader::fmt_utc_date(curr_ms);

        // Hourly progress
        let secs_in_day = (curr_ms / 1000) % 86400;
        if secs_in_day % 3600 == 0 {
            println!("[BACKTEST] {}", date_str);
        }

        let (base0, base1) = vault
            .base
            .as_ref()
            .map(|p| (p.tick_lower, p.tick_upper))
            .unwrap_or((0, 0));
        let (wide0, wide1) = vault
            .wide
            .as_ref()
            .map(|p| (p.tick_lower, p.tick_upper))
            .unwrap_or((0, 0));
        let (lim0, lim1) = vault
            .limit
            .as_ref()
            .map(|p| (p.tick_lower, p.tick_upper))
            .unwrap_or((0, 0));

        writer.write_row(&RebalanceLogRow {
            timestamp_ms: curr_ms,
            date: date_str,
            rebalance_type: "SNAPSHOT",
            price_wad: snap_price_wad,
            external_price: ext_price,
            total_value_stable: value_stable,
            collateral_ratio_wad: cr_wad,
            lp_ratio_wad: lp_ratio,
            virtual_debt: vault.virtual_debt,
            idle0: vault.idle0,
            idle1: vault.idle1,
            accumulated_fees0: vault.accumulated_fees0,
            accumulated_fees1: vault.accumulated_fees1,
            arb_profit_stable: arb_profit,
            arb_deviation_bps: arb_dev_bps,
            wide0,
            wide1,
            base0,
            base1,
            limit0: lim0,
            limit1: lim1,
        });

        curr_ms += step_ms;
    }

    writer.flush();
    let row_count = writer.row_count();

    // ── Calculate APY (matching TS calculateAPY) ──
    let (apy, total_return) =
        if let (Some((first_btc, first_t)), Some((last_btc, last_t))) =
            (first_apy_value, last_apy_value)
        {
            let days = (last_t - first_t) / (1000.0 * 86400.0);
            if days > 0.0 && first_btc > 0.0 {
                let vault_return = last_btc / first_btc - 1.0;
                let apy = ((1.0 + vault_return).powf(365.0 / days) - 1.0) * 100.0;
                (apy, vault_return * 100.0)
            } else {
                (0.0, 0.0)
            }
        } else {
            (0.0, 0.0)
        };

    // ── Monthly metrics (matching TS calculateAPY monthly section) ──
    let (sharpe_ratio, sortino_ratio, downside_deviation, monthly_return_stdev, worst_month_return) =
        compute_monthly_metrics(&monthly_btc_values, apy);

    // ── Per-period risk metrics (matching TS computeRiskMetrics) ──
    let (sigma, sortino_hf, downside_std) =
        compute_risk_metrics(&btc_values_for_risk, cfg.lookup_period);

    let min_cr_val = if min_cr.is_finite() {
        (min_cr * 100.0).round() / 100.0
    } else {
        0.0
    };
    let max_dd = (max_drawdown_pct * 100.0).round() / 100.0;
    let liquidated = min_cr_val > 0.0 && min_cr_val < 110.0;

    // ── Final summary (matching TS evaluate output) ──
    let (total0, total1) = vault.total_amounts(&pool);
    let final_nav = vault.total_pool_value(&pool, None);
    let final_lp_ratio = vault.lp_ratio(&pool, None);
    let final_cr_pct = vault.collateral_ratio_pct(&pool, None);
    let final_vol_price_wad =
        Vault::volatile_price_wad(pool.sqrt_price_x96(), cfg.pool_config.is_volatile_token0());

    println!("success!");
    println!("periods processed: {}", tick_count);
    println!("ALM calls: {}", alm_calls);
    println!("DLV calls: {}", dlv_calls);
    println!("Regulate Debt calls: {}", regulate_debt_calls);
    println!("Arb calls: {}", arb_calls);
    println!("LevAMM calls: 0");
    println!("Slow Recenter calls: 0");
    println!(
        "position {} value: {}",
        cfg.pool_config.stable_symbol(),
        final_nav.to_dec_string()
    );
    println!("lpRatio (WAD): {}", final_lp_ratio.to_dec_string());
    println!(
        "collateral ratio (%): {}",
        if final_cr_pct.is_finite() {
            format!("{}", final_cr_pct)
        } else {
            "infinite".to_string()
        }
    );
    println!(
        "total amounts: {} {}, {} {}",
        total0.to_dec_string(),
        cfg.pool_config.token0.symbol,
        total1.to_dec_string(),
        cfg.pool_config.token1.symbol,
    );
    println!(
        "current price (Volatile, WAD): {}",
        final_vol_price_wad.to_dec_string()
    );
    println!(
        "virtual debt ({}): {}",
        cfg.pool_config.stable_symbol(),
        vault.virtual_debt.to_dec_string()
    );

    let result_json = format!(
        concat!(
            "{{",
            "\"apy\":{:.3},",
            "\"totalReturn\":{:.3},",
            "\"minCR\":{:.2},",
            "\"maxDrawdown\":{:.2},",
            "\"worstMonthReturn\":{:.2},",
            "\"liquidated\":{},",
            "\"sortinoRatio\":{:.3},",
            "\"sharpeRatio\":{:.3},",
            "\"downsideDeviation\":{:.3},",
            "\"monthlyReturnStdev\":{:.3},",
            "\"sigma\":{:.2},",
            "\"sortino\":{:.2},",
            "\"downsideStd\":{:.2}",
            "}}"
        ),
        apy,
        total_return,
        min_cr_val,
        max_dd,
        worst_month_return,
        liquidated,
        sortino_ratio,
        sharpe_ratio,
        downside_deviation,
        monthly_return_stdev,
        sigma,
        sortino_hf,
        downside_std,
    );
    println!("RESULT_JSON: {}", result_json);
    println!("[OUTPUT] {} rows written to {}", row_count, output_path);

    BacktestResult {
        row_count,
        apy,
        total_return,
    }
}

fn compute_monthly_metrics(
    monthly_btc: &HashMap<String, (f64, f64)>,
    apy: f64,
) -> (f64, f64, f64, f64, f64) {
    let mut monthly_returns: Vec<f64> = Vec::new();
    let mut worst_month = 0.0f64;

    for (_, (first, last)) in monthly_btc {
        if *first > 0.0 {
            let r = (last / first - 1.0) * 100.0;
            monthly_returns.push(r);
            if r < worst_month {
                worst_month = r;
            }
        }
    }

    let worst_month = (worst_month * 100.0).round() / 100.0;

    if monthly_returns.len() < 2 {
        return (0.0, 0.0, 0.0, 0.0, worst_month);
    }

    let n = monthly_returns.len() as f64;
    let mean = monthly_returns.iter().sum::<f64>() / n;
    let variance = monthly_returns
        .iter()
        .map(|r| (r - mean) * (r - mean))
        .sum::<f64>()
        / n;
    let monthly_stdev = variance.sqrt();

    let downside_sq_sum: f64 = monthly_returns
        .iter()
        .map(|r| if *r < 0.0 { r * r } else { 0.0 })
        .sum();
    let downside_dev = (downside_sq_sum / n).sqrt();

    let ann_std = monthly_stdev * 12.0f64.sqrt();
    let ann_down = downside_dev * 12.0f64.sqrt();

    let sharpe = apy / ann_std.max(0.01);
    let sortino = apy / ann_down.max(0.01);

    let monthly_stdev = (monthly_stdev * 1000.0).round() / 1000.0;
    let downside_dev = (downside_dev * 1000.0).round() / 1000.0;
    let sharpe = (sharpe * 1000.0).round() / 1000.0;
    let sortino = (sortino * 1000.0).round() / 1000.0;

    (sharpe, sortino, downside_dev, monthly_stdev, worst_month)
}

fn compute_risk_metrics(btc_values: &[f64], lookup_period_secs: u32) -> (f64, f64, f64) {
    if btc_values.len() < 30 {
        return (0.0, 0.0, 0.0);
    }

    let mut rets: Vec<f64> = Vec::new();
    for i in 1..btc_values.len() {
        let r = (btc_values[i] / btc_values[i - 1]).ln();
        if r.is_finite() {
            rets.push(r);
        }
    }
    if rets.len() < 2 {
        return (0.0, 0.0, 0.0);
    }

    let n = rets.len() as f64;
    let mean = rets.iter().sum::<f64>() / n;
    let variance = rets
        .iter()
        .map(|r| (r - mean) * (r - mean))
        .sum::<f64>()
        / (n - 1.0);

    let mut downside_sq_sum = 0.0;
    let mut downside_n = 0u64;
    for &r in &rets {
        if r < 0.0 {
            downside_sq_sum += r * r;
            downside_n += 1;
        }
    }
    let downside_var = if downside_n > 0 {
        downside_sq_sum / downside_n as f64
    } else {
        0.0
    };
    let downside_std_val = downside_var.sqrt();

    let periods_per_year = (365.0 * 86400.0) / (lookup_period_secs as f64).max(1.0);
    let sigma = variance.sqrt() * periods_per_year.sqrt() * 100.0;
    let downside_std_annual = downside_std_val * periods_per_year.sqrt() * 100.0;
    let mean_annual_pct = mean * periods_per_year * 100.0;
    let sortino = if downside_std_annual > 0.0 {
        mean_annual_pct / downside_std_annual
    } else {
        0.0
    };

    let sigma = (sigma * 100.0).round() / 100.0;
    let sortino = (sortino * 100.0).round() / 100.0;
    let downside_std_annual = (downside_std_annual * 100.0).round() / 100.0;

    (sigma, sortino, downside_std_annual)
}

fn date_to_ms(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}
