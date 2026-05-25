use chrono::NaiveDate;
use std::collections::HashMap;

use v3_pool::core_pool::CorePool;
use v3_pool::full_math;
use v3_pool::tick_manager::TickManager;
use v3_pool::position_manager::PositionManager;
use v3_pool::types::*;

use crate::arb;
use crate::config::Config;
use crate::enums::EventType;
use crate::event_reader::{self, EventReader, PoolEvent};
use crate::output::{JsonlWriter, RebalanceLogRow};
use crate::price_feed;
use crate::vault::Vault;

fn wad() -> U256 { U256::from_u128(1_000_000_000_000_000_000) }

pub struct BacktestResult {
    pub row_count: usize,
    pub apy: f64,
    pub total_return: f64,
}

fn replay_event(pool: &mut CorePool, event: &PoolEvent) {
    match event.event_type {
        EventType::Mint => {
            let tl = event.tick_lower.unwrap_or(0);
            let tu = event.tick_upper.unwrap_or(0);
            let liq = event.liquidity;
            if liq <= I256::ZERO { return; }
            let recipient = if event.recipient.is_empty() { &event.msg_sender } else { &event.recipient };
            let saved = pool.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.mint(recipient, tl, tu, liq);
            }));
            if result.is_err() {
                *pool = saved;
            }
        }
        EventType::Burn => {
            let tl = event.tick_lower.unwrap_or(0);
            let tu = event.tick_upper.unwrap_or(0);
            let liq = event.liquidity;
            if liq <= I256::ZERO { return; }
            let pos = pool.get_position(&event.msg_sender, tl, tu);
            if pos.liquidity < liq.0 {
                if pos.liquidity.is_zero() { return; }
                let saved = pool.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pool.burn(&event.msg_sender, tl, tu, I256(pos.liquidity));
                }));
                if result.is_err() {
                    *pool = saved;
                }
            } else {
                let saved = pool.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pool.burn(&event.msg_sender, tl, tu, liq);
                }));
                if result.is_err() {
                    *pool = saved;
                }
            }
        }
        EventType::Swap => {
            if pool.tick_manager().ticks().is_empty() {
                return;
            }

            let current_sqrt = pool.sqrt_price_x96();

            // Determine direction from event amounts (matching TS behavior)
            let zero_for_one = if !event.amount0.is_zero() {
                event.amount0 > I256::ZERO
            } else {
                event.amount1 < I256::ZERO
            };

            let historical_limit = event.sqrt_price_x96;
            let limit_reachable = match historical_limit {
                Some(h) => {
                    if zero_for_one {
                        h < current_sqrt && h > MIN_SQRT_RATIO
                    } else {
                        h > current_sqrt && h < MAX_SQRT_RATIO
                    }
                }
                None => false,
            };

            let (amount_specified, sqrt_price_limit) = if limit_reachable {
                let big = I256(U256::new((1u128 << 127) - 1, 0));
                (big, historical_limit)
            } else {
                let evt_amt = event.amount_specified
                    .unwrap_or_else(|| {
                        if zero_for_one { event.amount0 } else { event.amount1 }
                    });
                let amt = if !evt_amt.is_zero() {
                    evt_amt
                } else {
                    I256(U256::new((1u128 << 127) - 1, 0))
                };
                let limit = if zero_for_one {
                    Some(MIN_SQRT_RATIO + U256::ONE)
                } else {
                    Some(MAX_SQRT_RATIO - U256::ONE)
                };
                (amt, limit)
            };

            let saved = pool.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pool.swap(zero_for_one, amount_specified, sqrt_price_limit);
            }));
            if result.is_err() {
                *pool = saved;
            }
        }
    }
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

    // Event-replay mode: load main events; warm-up is skipped when parquet
    // lacks mint events (zero external liquidity, vault becomes sole LP).
    let main_events: Vec<PoolEvent>;
    if !cfg.is_arb_strategy {
        let start_date_str = event_reader::fmt_utc_date(start_ms);
        let end_date_str = event_reader::fmt_utc_date(end_ms);

        // Warm-up: replay events to reconstruct pool state
        println!("[WARMUP] Replaying events up to {}", start_date_str);
        let warmup_events = event_reader.load_all_events(None, Some(&start_date_str));
        println!("[WARMUP] {} events to replay", warmup_events.len());
        for event in &warmup_events {
            replay_event(&mut pool, event);
        }
        println!(
            "[WARMUP] Completed: tick={} sqrtPrice={} liquidity={}",
            pool.tick_current(),
            pool.sqrt_price_x96().to_dec_string(),
            pool.liquidity().to_dec_string(),
        );

        // With empty-tick swap skipping (matching TS "LENGTH" assert behavior),
        // warm-up preserves the initial sqrtPriceX96 — no repositioning needed.

        // Pre-load main loop events
        println!("[PRELOAD] Loading events from {} to {}", start_date_str, end_date_str);
        main_events = event_reader.load_all_events(Some(&start_date_str), Some(&end_date_str));
        println!("[PRELOAD] {} events loaded", main_events.len());
    } else {
        main_events = Vec::new();

        // Arb-only mode: position pool at start-date price from external feed
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
    let volatile_raw = if price_human > 0.0 && price_human.is_finite() {
        let volatile_tokens = target_usd / price_human;
        if volatile_tokens.is_finite() {
            (volatile_tokens * 10f64.powi(cfg.pool_config.volatile_decimals() as i32)) as u128
        } else {
            0
        }
    } else {
        // Fallback: derive price from raw sqrtPrice (matches TS account.ts lines 99-121)
        let sqrt_num = pool.sqrt_price_x96().lo as f64 + pool.sqrt_price_x96().hi as f64 * (u128::MAX as f64 + 1.0);
        let q96 = 2.0f64.powi(96);
        let raw_price = (sqrt_num / q96).powi(2);
        let fallback_price = raw_price * decimal_adj;
        if fallback_price > 0.0 && fallback_price.is_finite() {
            let fallback_tokens = target_usd / fallback_price;
            (fallback_tokens * 10f64.powi(cfg.pool_config.volatile_decimals() as i32)).floor() as u128
        } else {
            0
        }
    };

    let volatile_u256 = U256::from_u128(volatile_raw);
    let vol_price_wad = Vault::volatile_price_wad(pool.sqrt_price_x96(), cfg.pool_config.is_volatile_token0());
    let stable_u256 = full_math::mul_div_rounding_up(volatile_u256, vol_price_wad, wad());
    let stable_raw = stable_u256.lo as u128;
    let (a0, a1) = if cfg.pool_config.is_volatile_token0() {
        (volatile_u256, stable_u256)
    } else {
        (stable_u256, volatile_u256)
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
        vault.rebalance_debt(&mut pool, target_cr_wad, cfg.dlv.debt_to_volatile_swap_fee);
        let (t0, t1) = vault.total_amounts(&pool);
        let init_gav = vault.total_value_in_stable(&pool, None);
        let init_nav = vault.total_pool_value(&pool, None);
        let pp = Vault::pool_price(pool.sqrt_price_x96());
        let vp = Vault::volatile_price_wad(pool.sqrt_price_x96(), cfg.pool_config.is_volatile_token0());
        let vol_amt = if cfg.pool_config.is_volatile_token0() { t0 } else { t1 };
        let stb_amt = if cfg.pool_config.is_volatile_token0() { t1 } else { t0 };
        let vol_in_stable = full_math::mul_div(vol_amt, vp, wad());
        println!(
            "[INIT-DEBUG] debt={} GAV={} NAV={} total0={} total1={} idle0={} idle1={} CR={:.2}%",
            vault.virtual_debt.to_dec_string(), init_gav.to_dec_string(), init_nav.to_dec_string(),
            t0.to_dec_string(), t1.to_dec_string(),
            vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
            vault.collateral_ratio_pct(&pool, None),
        );
        println!(
            "[INIT-DEBUG] pool_price={} vol_price_wad={} S={} V={} V_in_stable={} S-V={}",
            pp.to_dec_string(), vp.to_dec_string(),
            stb_amt.to_dec_string(), vol_amt.to_dec_string(), vol_in_stable.to_dec_string(),
            if stb_amt > vol_in_stable { (stb_amt - vol_in_stable).to_dec_string() } else { format!("-{}", (vol_in_stable - stb_amt).to_dec_string()) },
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

    // Event cursor for event-replay mode
    let mut event_cursor: usize = 0;

    // Initialize ALM time trigger: TS uses count % period==0, first fires at tick=period.
    // Setting last_rebalance_ms to start_ms prevents spurious first-tick trigger.
    vault.last_rebalance_ms = start_ms;

    // Main backtest loop
    let step_ms = (cfg.lookup_period as i64) * 1000;
    let mut curr_ms = start_ms;

    while curr_ms < end_ms {
        tick_count += 1;
        if let Some(max) = cfg.max_ticks {
            if tick_count > max { break; }
        }

        // Get external price (used for APY/risk tracking in both modes)
        let (ext_price, ext_sqrt) = price_feed.get_price_at_monotonic(curr_ms);

        let mut arb_profit = U256::ZERO;
        let mut arb_dev_bps = 0.0f64;

        if cfg.is_arb_strategy {
            // ── ARB-ONLY MODE ──
            // Dispatch order: ARB → DLV → ALM → REGULATE_DEBT

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
                        arb_profit = result.profit_stable.abs();
                        if result.profit_stable.is_positive() && !arb_profit.is_zero() {
                            if cfg.pool_config.is_volatile_token0() {
                                vault.idle1 = vault.idle1 + arb_profit;
                            } else {
                                vault.idle0 = vault.idle0 + arb_profit;
                            }
                        }
                        arb_calls += 1;
                    }
                }
            }

            // DLV
            dispatch_dlv(cfg, &mut vault, &mut pool, target_cr_wad, w_const, curr_ms, &mut dlv_calls);

            // ALM
            dispatch_alm(cfg, &mut vault, &mut pool, ext_sqrt, curr_ms, &mut alm_calls);

            // Regulate debt (runs LAST in arb mode)
            dispatch_regulate_debt(cfg, &mut vault, &mut pool, target_cr_wad, &mut regulate_debt_calls);
        } else {
            // ── EVENT-REPLAY MODE ──
            // Dispatch order: REGULATE_DEBT → DLV → ALM → (replay events)

            // Regulate debt (runs FIRST in event-replay mode)
            dispatch_regulate_debt(cfg, &mut vault, &mut pool, target_cr_wad, &mut regulate_debt_calls);

            // DLV
            dispatch_dlv(cfg, &mut vault, &mut pool, target_cr_wad, w_const, curr_ms, &mut dlv_calls);

            // ALM
            dispatch_alm(cfg, &mut vault, &mut pool, ext_sqrt, curr_ms, &mut alm_calls);

            // Per-tick diagnostic (env TICK_DIAG=N to print first N ticks)
            if let Some(diag_limit) = std::env::var("TICK_DIAG").ok().and_then(|v| v.parse::<u64>().ok()) {
                if tick_count <= diag_limit {
                    let (t0, t1) = vault.total_amounts_round_up(&pool);
                    println!("[TICK-DIAG {}] poolTick={} sqrtPrice={} liq={} | t0={} t1={} idle0={} idle1={} debt={} | almCalls={} dlvCalls={} rdCalls={}",
                        tick_count, pool.tick_current(), pool.sqrt_price_x96().to_dec_string(),
                        pool.liquidity().to_dec_string(),
                        t0.to_dec_string(), t1.to_dec_string(),
                        vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                        vault.virtual_debt.to_dec_string(),
                        alm_calls, dlv_calls, regulate_debt_calls);
                }
            }

            // Replay events up to next period boundary
            let next_ms = curr_ms + step_ms;
            while event_cursor < main_events.len() {
                let event = &main_events[event_cursor];
                if event.date_ms >= next_ms { break; }
                event_cursor += 1;
                replay_event(&mut pool, event);
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
            "\"apy\":{},",
            "\"totalReturn\":{},",
            "\"minCR\":{},",
            "\"maxDrawdown\":{},",
            "\"worstMonthReturn\":{},",
            "\"liquidated\":{},",
            "\"sortinoRatio\":{},",
            "\"sharpeRatio\":{},",
            "\"downsideDeviation\":{},",
            "\"monthlyReturnStdev\":{},",
            "\"sigma\":{},",
            "\"sortino\":{},",
            "\"downsideStd\":{}",
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

fn dispatch_regulate_debt(
    cfg: &Config,
    vault: &mut Vault,
    pool: &mut CorePool,
    target_cr_wad: U256,
    regulate_debt_calls: &mut u64,
) {
    if cfg.is_regulate_debt && !vault.virtual_debt.is_zero() {
        vault.collect_fees(pool);
        let rd_mode = vault.regulate_debt(pool, None, target_cr_wad);
        if rd_mode == "mint" {
            vault.deploy_idle_to_lp(pool);
        }
        *regulate_debt_calls += 1;
    }
}

fn dispatch_dlv(
    cfg: &Config,
    vault: &mut Vault,
    pool: &mut CorePool,
    target_cr_wad: U256,
    w_const: U256,
    curr_ms: i64,
    dlv_calls: &mut u64,
) {
    if cfg.is_regulate_debt && !vault.virtual_debt.is_zero() {
        let dlv_period_check = if let Some(p) = cfg.dlv.period {
            let dlv_cooldown_ms = p as i64 * 1000;
            let dlv_time_since = curr_ms - vault.last_debt_rebalance_ms;
            vault.last_debt_rebalance_ms == 0 || dlv_time_since >= dlv_cooldown_ms
        } else {
            true
        };
        if dlv_period_check {
            let cr = vault.collateral_ratio_wad(pool, None);
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
                vault.rebalance_debt_dlv(pool, target_cr_wad, cfg.dlv.debt_to_volatile_swap_fee);
                vault.last_rebalance_ms = curr_ms;
                vault.last_debt_rebalance_ms = curr_ms;
                *dlv_calls += 1;
            }
        }
    }
}

fn dispatch_alm(
    cfg: &Config,
    vault: &mut Vault,
    pool: &mut CorePool,
    ext_sqrt: U256,
    curr_ms: i64,
    alm_calls: &mut u64,
) {
    let time_since_last = curr_ms - vault.last_rebalance_ms;
    let time_trigger = vault.last_rebalance_ms == 0
        || time_since_last >= (vault.params.period as i64 * 1000);
    let (ratio_trigger, dev_bps_val) = if cfg.active_rebalance_ratio_deviation_bps > 0 {
        let dev_bps = vault.share_deviation_bps(pool, None);
        (dev_bps >= cfg.active_rebalance_ratio_deviation_bps as u64, dev_bps)
    } else {
        (false, 0)
    };

    static ALM_DBG_RATIO: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    static ALM_DBG_PRINT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    if ratio_trigger || time_trigger {
        let print_idx = ALM_DBG_PRINT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if print_idx < 20 {
            println!("[ALM-DBG] tick#{} ratio_trigger={} time_trigger={} dev_bps={} poolTick={} last_rebalance_ms={}",
                print_idx, ratio_trigger, time_trigger, dev_bps_val, pool.tick_current(), vault.last_rebalance_ms);
        }
    }

    if ratio_trigger {
        let dbg_idx = ALM_DBG_RATIO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if dbg_idx < 10 {
            let dev_before = vault.share_deviation_bps(pool, None);
            let (t0b, t1b) = vault.total_amounts_round_up(pool);
            eprintln!("[ALM-RATIO] #{} BEFORE: dev_bps={} t0={} t1={} idle0={} idle1={} poolTick={} poolSqrt={}",
                dbg_idx, dev_before,
                t0b.to_dec_string(), t1b.to_dec_string(),
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                pool.tick_current(), pool.sqrt_price_x96().to_dec_string());

            vault.withdraw_all(pool);
            let dev_w = vault.share_deviation_bps(pool, None);
            eprintln!("[ALM-RATIO] #{} AFTER-WITHDRAW: dev_bps={} idle0={} idle1={}",
                dbg_idx, dev_w,
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string());

            vault.active_rebalance_swap(None, pool, cfg.dlv.debt_to_volatile_swap_fee);
            let dev_s = vault.share_deviation_bps(pool, None);
            eprintln!("[ALM-RATIO] #{} AFTER-SWAP: dev_bps={} idle0={} idle1={}",
                dbg_idx, dev_s,
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string());

            vault.rebalance_from_idle(pool);
            let dev_a = vault.share_deviation_bps(pool, None);
            let (t0a, t1a) = vault.total_amounts_round_up(pool);
            eprintln!("[ALM-RATIO] #{} AFTER-DEPLOY: dev_bps={} t0={} t1={} idle0={} idle1={}",
                dbg_idx, dev_a,
                t0a.to_dec_string(), t1a.to_dec_string(),
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string());
        } else {
            vault.withdraw_all(pool);
            vault.active_rebalance_swap(None, pool, cfg.dlv.debt_to_volatile_swap_fee);
            vault.rebalance_from_idle(pool);
        }
        vault.last_rebalance_ms = curr_ms;
        *alm_calls += 1;
    } else if time_trigger {
        vault.last_rebalance_ms = curr_ms;
        *alm_calls += 1;
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
