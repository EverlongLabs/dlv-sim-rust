use chrono::NaiveDate;
use std::collections::HashMap;

use v3_pool::core_pool::CorePool;
use v3_pool::full_math;
use v3_pool::tick_manager::TickManager;
use v3_pool::position_manager::PositionManager;
use v3_pool::types::*;

use crate::arb;
use crate::config::{Config, ActiveRebalanceMode};
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

    let (_pool_id, base_fee, base_tick_spacing) = event_reader.get_pool_config();
    let (fee, tick_spacing) = if let Some(override_fee) = cfg.pool_fee_override {
        let ts = match override_fee {
            100 => 1,
            500 => 10,
            3000 => 60,
            _ => 200,
        };
        (override_fee, ts)
    } else {
        (base_fee, base_tick_spacing)
    };
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

    let mut start_ms = date_to_ms(cfg.start_date);
    let mut end_ms = date_to_ms(cfg.end_date);

    // Arb mode: override simulation range with arb config dates (matches TS line 2996-2997)
    if cfg.is_arb_strategy {
        start_ms = date_to_ms(cfg.arb.start_date);
        end_ms = date_to_ms(cfg.arb.end_date);
    }

    let sampling_ms = (cfg.lookup_period as i64) * 1000;
    let mut price_feed = price_feed::load_price_feed(
        &cfg.arb.price_feed_dir,
        start_ms,
        end_ms,
        sampling_ms,
        &cfg.pool_config,
    );

    // Clip end_ms to last available price feed entry (matches TS line 3002-3008)
    if cfg.is_arb_strategy && price_feed.end_ms() < end_ms {
        eprintln!(
            "[ARB] endDate exceeds last price feed entry — clipping to avoid frozen-price tail."
        );
        end_ms = price_feed.end_ms();
    }

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
    let stable_u256 = if cfg.lev_amm.enabled {
        U256::ZERO
    } else {
        full_math::mul_div_rounding_up(volatile_u256, vol_price_wad, wad())
    };
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
    // When LevAMM is enabled, TS skips rebalanceDebt during init (returns ZERO)
    // because LevAMM manages debt from its own init step on the first tick.
    let target_cr_wad = U256::from_u128(cfg.target_cr_wad());
    if !cfg.is_alm_only && cfg.is_regulate_debt && !cfg.lev_amm.enabled {
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
    let mut lev_amm_calls: u64 = 0;
    let mut slow_recenter_calls: u64 = 0;
    let mut last_slow_recenter_ms: i64 = 0;
    let w_const = U256::from_u128(1_000_000_000_000_000_000);

    let mut arb_trace_file: Option<std::fs::File> = std::env::var("ARB_TRACE_FILE").ok().map(|p| {
        std::fs::File::create(&p).unwrap_or_else(|e| panic!("Cannot create {}: {}", p, e))
    });

    // APY + risk metrics tracking (pool price, matching TS with no quoter/oracle)
    let mut first_apy_value: Option<(f64, f64)> = None;
    let mut last_apy_value: Option<(f64, f64)> = None;
    let mut btc_values_for_risk: Vec<f64> = Vec::new();
    let mut min_cr: f64 = f64::INFINITY;
    let mut peak_btc_value: f64 = 0.0;
    let mut max_drawdown_pct: f64 = 0.0;
    let mut monthly_btc_values: HashMap<String, (f64, f64)> = HashMap::new();

    // Idle snapshot interval: TS logs every 60 min when no event fires
    let idle_snapshot_interval_ms: i64 = 3600 * 1000;
    let mut last_idle_snapshot_ms: i64 = 0;

    // Log-return circular buffer for dynamic thresholds (match TS LogReturnsWindow)
    let vol_window_size = ((cfg.volatility_window_minutes as usize) * 60)
        .checked_div(cfg.lookup_period as usize)
        .unwrap_or(1)
        .max(1);
    let mut log_returns: Vec<f64> = Vec::with_capacity(vol_window_size);
    let mut log_returns_idx: usize = 0;
    let mut prev_ext_price: f64 = 0.0;

    // Event cursor for event-replay mode
    let mut event_cursor: usize = 0;
    // Initialize ALM time trigger: TS uses count % period==0, first fires at tick=period.
    // Setting last_rebalance_ms to start_ms prevents spurious first-tick trigger.
    vault.last_rebalance_ms = start_ms;

    // Main backtest loop
    let step_ms = (cfg.lookup_period as i64) * 1000;
    let mut curr_ms = start_ms;

    // Pre-replay state (only set in event-replay mode; TS computes CR and
    // SNAPSHOT btcValue before replay events shift the pool price)
    let mut pre_replay_cr_pct: Option<f64> = None;
    let mut pre_replay_snapshot_btc_value: Option<f64> = None;

    // TS SNAPSHOT_PENDING_KEY: set when DLV trigger returns false (CR within
    // deviation threshold), cleared at start of next period's RD act. Drives
    // whether the period's SNAP entry fires EARLY (during RD act, pre-DLV-ALM
    // state, pushed before ALM/DLV entries) or LATE (during SNAPSHOT dispatch
    // at end of period, post-ALM state, pushed after).
    let mut snapshot_pending: bool = false;

    while curr_ms < end_ms {
        tick_count += 1;
        pre_replay_cr_pct = None;
        pre_replay_snapshot_btc_value = None;
        if let Some(max) = cfg.max_ticks {
            if tick_count > max { break; }
        }

        // Capture period-start state for SNAPSHOT entries. TS logs the SNAPSHOT
        // entry inside RD act (when SNAPSHOT_PENDING is set from prior period's
        // DLV trigger) — that fires BEFORE DLV/ALM dispatch and captures
        // post-collect_fees-but-pre-rebalance state. collect_fees is invariant
        // for total_pool_value (it just moves unpoked fees from LP into idle),
        // so capturing here (before any dispatch) is equivalent.
        let start_cr_pct = vault.collateral_ratio_pct(&pool, None);
        let start_snapshot_btc_value: Option<f64> = {
            let s = pool.sqrt_price_x96();
            let v = vault.total_pool_value(&pool, None).lo as f64
                / 10f64.powi(cfg.pool_config.stable_decimals() as i32);
            let p = Vault::volatile_price_wad(s, cfg.pool_config.is_volatile_token0()).lo as f64 / 1e18;
            if p > 0.0 { Some(v / p) } else { None }
        };

        // Get external price (used for APY/risk tracking in both modes)
        let (ext_price, ext_sqrt) = price_feed.get_price_at_monotonic(curr_ms);

        // Update log-return buffer (match TS cache phase AFTER_NEW_TIME_PERIOD)
        if cfg.use_dynamic_width && ext_price > 0.0 && prev_ext_price > 0.0 {
            let lr = (ext_price / prev_ext_price).ln();
            if log_returns.len() < vol_window_size {
                log_returns.push(lr);
            } else {
                log_returns[log_returns_idx] = lr;
            }
            log_returns_idx = (log_returns_idx + 1) % vol_window_size;
        }
        if ext_price > 0.0 { prev_ext_price = ext_price; }

        let mut arb_profit = U256::ZERO;
        let mut arb_dev_bps = 0.0f64;

        let prev_alm = alm_calls;
        let prev_dlv = dlv_calls;
        let mut _rd_fired = false;
        let mut snapshot_pending_at_start: bool = false;
        // TS captures snapshot price BEFORE each rebalance (collectVaultSnapshotInputs
        // runs before the swap). Track per-event pre-dispatch sqrt for btcValue computation.
        let mut dlv_pre_sqrt = U256::ZERO;
        let mut alm_pre_sqrt = U256::ZERO;
        // Intermediate NAV: TS computes vaultValue AFTER each dispatch using the
        // post-dispatch pool state. We capture these at intermediate points.
        let mut dlv_nav = U256::ZERO;
        let mut alm_nav = U256::ZERO;

        let mut arb_fired = false;
        let mut arb_pre_sqrt = U256::ZERO;
        let mut arb_nav = U256::ZERO;
        let mut arb_debt = U256::ZERO;
        let mut lev_amm_fired = false;
        let mut lev_amm_nav = U256::ZERO;
        let mut lev_amm_debt = U256::ZERO;

        // Per-step parity log gate (env ARB_PARITY_START/ARB_PARITY_END)
        let arb_parity_active = std::env::var("ARB_PARITY_START").ok()
            .and_then(|v| v.parse::<u64>().ok())
            .zip(std::env::var("ARB_PARITY_END").ok().and_then(|v| v.parse::<u64>().ok()))
            .map_or(false, |(s, e)| tick_count >= s && tick_count <= e);
        let log_arb_parity = |stage: &str, vault: &Vault, pool: &CorePool, tc: u64| {
            if !arb_parity_active { return; }
            let (t0, t1) = vault.total_amounts(pool);
            let cr_wad = vault.collateral_ratio_wad(pool, None);
            eprintln!(
                "[ARB-PARITY] tick={} stage={} pool_tick={} total0={} total1={} idle0={} idle1={} vdebt={} cr_wad={}",
                tc, stage, pool.tick_current(),
                t0.to_dec_string(), t1.to_dec_string(),
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                vault.virtual_debt.to_dec_string(),
                cr_wad.to_dec_string());
        };

        if cfg.is_arb_strategy {
            // ── ARB-ONLY MODE ──
            // Dispatch order: ARB → LEV_AMM → SLOW_RECENTER → DLV → ALM → REGULATE_DEBT
            log_arb_parity("pre", &vault, &pool, tick_count);

            if ext_price > 0.0 {
                let detection = arb::detect_arb(pool.sqrt_price_x96(), ext_sqrt, fee);
                arb_dev_bps = detection.deviation_bps;

                if let Some(diag_limit) = std::env::var("TICK_DIAG").ok().and_then(|v| v.parse::<u64>().ok()) {
                    if tick_count <= diag_limit {
                        eprintln!(
                            "[TICK_DIAG] t={} pool_tick={} pool_sqrt={} ext_sqrt={} ext_price={:.2} fee={} dev_bps={:.1} arb={}",
                            tick_count, pool.tick_current(), pool.sqrt_price_x96().to_dec_string(),
                            ext_sqrt.to_dec_string(), ext_price, fee, detection.deviation_bps, detection.is_arbitrable,
                        );
                        let (t0, t1) = vault.total_amounts(&pool);
                        eprintln!(
                            "[TICK_DIAG]   total0={} total1={} idle0={} idle1={} debt={} liq={} CR={:.2}%",
                            t0.to_dec_string(), t1.to_dec_string(),
                            vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                            vault.virtual_debt.to_dec_string(), pool.liquidity().to_dec_string(),
                            vault.collateral_ratio_pct(&pool, None),
                        );
                    }
                }

                if let Some(ref mut f) = arb_trace_file {
                    use std::io::Write;
                    let pre_sqrt = pool.sqrt_price_x96();
                    let pre_liq = pool.liquidity();
                    let _ = writeln!(f, "{}\t{}\t{}\t{}\t{:.1}\t{}\t{}", tick_count,
                        if detection.is_arbitrable { 1 } else { 0 },
                        pre_sqrt.to_dec_string(),
                        ext_sqrt.to_dec_string(),
                        detection.deviation_bps,
                        pre_liq.to_dec_string(),
                        detection.target_sqrt_price_x96.to_dec_string());
                }

                if detection.is_arbitrable {
                    arb_pre_sqrt = pool.sqrt_price_x96();
                    let result = arb::execute_arb_close_gap(&mut pool, &detection, cfg.pool_config.is_volatile_token0());
                    if let Some(ref mut f) = arb_trace_file {
                        use std::io::Write;
                        let _ = writeln!(f, "{}\tARB_EXEC\ta0={}\ta1={}\tprofit={}\tpost_sqrt={}", tick_count,
                            result.amount0, result.amount1, result.profit_stable,
                            pool.sqrt_price_x96().to_dec_string());
                    }
                    arb_profit = result.profit_stable.abs();
                    if result.profit_stable.is_positive() && !arb_profit.is_zero() && !cfg.no_arb_donation {
                        if cfg.pool_config.is_volatile_token0() {
                            vault.idle1 = vault.idle1 + arb_profit;
                        } else {
                            vault.idle0 = vault.idle0 + arb_profit;
                        }
                    }
                    arb_calls += 1;

                    if cfg.use_fee_recycling {
                        vault.collect_fees(&mut pool);
                        vault.deploy_idle_to_lp(&mut pool);
                    }
                    arb_fired = true;
                    arb_nav = vault.total_pool_value(&pool, None);
                    arb_debt = vault.virtual_debt;
                }
            }
            log_arb_parity("post_arb", &vault, &pool, tick_count);

            // LevAMM
            if cfg.lev_amm.enabled {
                // TS routes the LevAMM step through resolveAlmSwapSqrt(variable):
                //   "30bp" → pool sqrt (None override)
                //   "5bp"  → 5bp quoter sqrt (not modelled in Rust yet)
                //   "binance" → external feed sqrt
                let lev_amm_override = match cfg.dlv.alm_swap_price_source.as_str() {
                    "binance" => if ext_sqrt.is_zero() { None } else { Some(ext_sqrt) },
                    _ => None,
                };
                let (_fee, fired) = vault.run_lev_amm_step(&mut pool, &cfg.lev_amm, target_cr_wad, lev_amm_override);
                lev_amm_calls += 1;
                if fired {
                    lev_amm_fired = true;
                    lev_amm_nav = vault.total_pool_value(&pool, None);
                    lev_amm_debt = vault.virtual_debt;
                }
            }
            log_arb_parity("post_lev_amm", &vault, &pool, tick_count);

            // Slow Recenter
            if cfg.slow_recenter.enabled {
                let sr_elapsed = curr_ms - last_slow_recenter_ms;
                if last_slow_recenter_ms == 0 || sr_elapsed >= (cfg.slow_recenter.trigger_interval_seconds as i64) * 1000 {
                    let shifted = vault.slow_recenter(&mut pool, &cfg.slow_recenter);
                    last_slow_recenter_ms = curr_ms;
                    if shifted > 0 {
                        slow_recenter_calls += 1;
                    }
                }
            }
            log_arb_parity("post_sr", &vault, &pool, tick_count);

            // DLV
            dlv_pre_sqrt = pool.sqrt_price_x96();
            dispatch_dlv(cfg, &mut vault, &mut pool, target_cr_wad, w_const, curr_ms, &mut dlv_calls, tick_count);
            dlv_nav = vault.total_pool_value(&pool, None);
            log_arb_parity("post_dlv", &vault, &pool, tick_count);

            // ALM
            alm_pre_sqrt = pool.sqrt_price_x96();
            dispatch_alm(cfg, &mut vault, &mut pool, ext_sqrt, curr_ms, &mut alm_calls, tick_count);
            alm_nav = vault.total_pool_value(&pool, None);
            log_arb_parity("post_alm", &vault, &pool, tick_count);

            // Regulate debt (runs LAST in arb mode)
            _rd_fired = dispatch_regulate_debt(cfg, &mut vault, &mut pool, target_cr_wad, &mut regulate_debt_calls, &log_returns);
            log_arb_parity("post_rd", &vault, &pool, tick_count);
        } else {
            // ── EVENT-REPLAY MODE ──
            // Dispatch order: REGULATE_DEBT → DLV → ALM → (replay events)

            // Capture pending state from prior period before RD clears it.
            // This determines whether THIS period's SNAP (if due) fires early.
            snapshot_pending_at_start = snapshot_pending;

            // Regulate debt (runs FIRST in event-replay mode)
            _rd_fired = dispatch_regulate_debt(cfg, &mut vault, &mut pool, target_cr_wad, &mut regulate_debt_calls, &log_returns);
            // TS clears SNAPSHOT_PENDING_KEY inside RD act regardless of mint/burn outcome.
            snapshot_pending = false;

            // DLV
            dlv_pre_sqrt = pool.sqrt_price_x96();
            dispatch_dlv(cfg, &mut vault, &mut pool, target_cr_wad, w_const, curr_ms, &mut dlv_calls, tick_count);
            dlv_nav = vault.total_pool_value(&pool, None);
            // TS DLV trigger sets SNAPSHOT_PENDING_KEY=true when DLV does NOT
            // fire (CR within deviation threshold). Mirror that here.
            if dlv_calls == prev_dlv {
                snapshot_pending = true;
            }

            // ALM
            alm_pre_sqrt = pool.sqrt_price_x96();
            dispatch_alm(cfg, &mut vault, &mut pool, ext_sqrt, curr_ms, &mut alm_calls, tick_count);
            alm_nav = vault.total_pool_value(&pool, None);

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

            // Per-tick window dump (env TICK_DUMP_START..TICK_DUMP_END to dump state per tick in window)
            if let (Ok(start_s), Ok(end_s)) = (std::env::var("TICK_DUMP_START"), std::env::var("TICK_DUMP_END")) {
                if let (Ok(start), Ok(end)) = (start_s.parse::<u64>(), end_s.parse::<u64>()) {
                    if tick_count >= start && tick_count <= end {
                        let (t0, t1) = vault.total_amounts(&pool);
                        let cr_wad = vault.collateral_ratio_wad(&pool, None);
                        eprintln!("[TICK-WIN] tick={} pool_tick={} total0={} total1={} idle0={} idle1={} vdebt={} cr_wad={}",
                            tick_count, pool.tick_current(),
                            t0.to_dec_string(), t1.to_dec_string(),
                            vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                            vault.virtual_debt.to_dec_string(),
                            cr_wad.to_dec_string());
                    }
                }
            }

            // Capture pre-replay state for metrics (TS computes CR and
            // SNAPSHOT btcValue before replay events shift the pool price)
            pre_replay_cr_pct = Some(vault.collateral_ratio_pct(&pool, None));
            {
                let s = pool.sqrt_price_x96();
                let v = vault.total_pool_value(&pool, None).lo as f64
                    / 10f64.powi(cfg.pool_config.stable_decimals() as i32);
                let p = Vault::volatile_price_wad(s, cfg.pool_config.is_volatile_token0()).lo as f64 / 1e18;
                if p > 0.0 { pre_replay_snapshot_btc_value = Some(v / p); }
            }

            // Replay events up to next period boundary
            let next_ms = curr_ms + step_ms;
            let trace_replay = {
                let single = std::env::var("REPLAY_TRACE_TICK")
                    .ok().and_then(|v| v.parse::<u64>().ok())
                    .map_or(false, |t| t == tick_count);
                let win = match (std::env::var("REPLAY_TRACE_WIN_START").ok().and_then(|v| v.parse::<u64>().ok()),
                                 std::env::var("REPLAY_TRACE_WIN_END").ok().and_then(|v| v.parse::<u64>().ok())) {
                    (Some(s), Some(e)) => tick_count >= s && tick_count <= e,
                    _ => false,
                };
                single || win
            };
            while event_cursor < main_events.len() {
                let event = &main_events[event_cursor];
                if event.date_ms >= next_ms { break; }
                event_cursor += 1;
                if trace_replay {
                    let before_sqrt = pool.sqrt_price_x96();
                    let before_tick = pool.tick_current();
                    replay_event(&mut pool, event);
                    let after_sqrt = pool.sqrt_price_x96();
                    let after_tick = pool.tick_current();
                    let evt_type = format!("{:?}", event.event_type);
                    let evt_sqrt = event.sqrt_price_x96.map(|s| s.to_dec_string()).unwrap_or_else(|| "-".into());
                    eprintln!("[REPLAY-TRACE] tick={} evt_type={} evt_a0={} evt_a1={} evt_sqrt={} before_sqrt={} before_tick={} after_sqrt={} after_tick={}",
                        tick_count, evt_type,
                        event.amount0.to_dec_string(), event.amount1.to_dec_string(),
                        evt_sqrt,
                        before_sqrt.to_dec_string(), before_tick,
                        after_sqrt.to_dec_string(), after_tick);
                } else {
                    replay_event(&mut pool, event);
                }
            }
        }

        // Detect which events fired this tick (RD does NOT contribute to sparse sampling —
        // TS only calls registerLogSummary for ALM, DLV, SNAPSHOT, and circuit breakers)
        let alm_fired = alm_calls > prev_alm;
        let dlv_fired = dlv_calls > prev_dlv;
        let _event_fired = alm_fired || dlv_fired;

        // TS fires SNAPSHOT independently of ALM/DLV — both can log on same tick.
        // Idle snapshot fires when interval elapsed (regardless of event).
        let idle_snapshot_due = curr_ms - last_idle_snapshot_ms >= idle_snapshot_interval_ms;
        if idle_snapshot_due {
            last_idle_snapshot_ms = curr_ms;
        }
        // Daily checkpoint diagnostic (every 7200 ticks = 1 day at 12s periods)
        if std::env::var("DAILY_DIAG").is_ok() && tick_count % 7200 == 0 {
            let day = tick_count / 7200;
            let (t0, t1) = vault.total_amounts(&pool);
            let cr = vault.collateral_ratio_pct(&pool, None);
            let wide_info = vault.wide_info();
            let base_info = vault.base_info();
            let limit_info = vault.limit_info();
            eprintln!(
                "[DAY_DIAG] day={} t={} pool_tick={} pool_sqrt={} pool_liq={}",
                day, tick_count, pool.tick_current(), pool.sqrt_price_x96().to_dec_string(),
                pool.liquidity().to_dec_string(),
            );
            eprintln!(
                "[DAY_DIAG]   total0={} total1={} idle0={} idle1={} debt={} CR={:.2}%",
                t0.to_dec_string(), t1.to_dec_string(),
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                vault.virtual_debt.to_dec_string(), cr,
            );
            eprintln!(
                "[DAY_DIAG]   notional={} collateral={} fee_rev={}",
                vault.lev_amm_notional.to_dec_string(),
                vault.lev_amm_collateral.to_dec_string(),
                vault.lev_amm_fee_revenue.to_dec_string(),
            );
            eprintln!(
                "[DAY_DIAG]   wide=[{},{},liq={}] base=[{},{},liq={}] limit=[{},{},liq={}]",
                wide_info.0, wide_info.1, wide_info.2.to_dec_string(),
                base_info.0, base_info.1, base_info.2.to_dec_string(),
                limit_info.0, limit_info.1, limit_info.2.to_dec_string(),
            );
            eprintln!(
                "[DAY_DIAG]   arb={} levamm={} sr={} dlv={} rd={}",
                arb_calls, lev_amm_calls, slow_recenter_calls, dlv_calls, regulate_debt_calls,
            );
        }

        // Post-dispatch pool state (used for idle snapshots and output row)
        let pool_sqrt = pool.sqrt_price_x96();
        let nav = vault.total_pool_value(&pool, None);
        let value_stable = vault.total_value_in_stable(&pool, None);
        let vol_price_wad =
            Vault::volatile_price_wad(pool_sqrt, cfg.pool_config.is_volatile_token0());
        let cr_pct = vault.collateral_ratio_pct(&pool, None);
        let snap_price_wad = Vault::pool_price(pool_sqrt);
        let cr_wad = vault.collateral_ratio_wad(&pool, None);
        let lp_ratio = vault.lp_ratio(&pool, None);

        let vault_value_f =
            nav.lo as f64 / 10f64.powi(cfg.pool_config.stable_decimals() as i32);
        let price_f = vol_price_wad.lo as f64 / 1e18;

        // TS registers ARB, LEV_AMM, DLV, ALM, and SNAPSHOT entries independently
        // per tick via registerLogSummary. Each entry uses its own price/NAV:
        // event entries use pre-dispatch price and post-dispatch NAV; idle
        // snapshots use post-all-dispatch price and NAV.
        let is_vol_t0 = cfg.pool_config.is_volatile_token0();
        let stable_scale = 10f64.powi(cfg.pool_config.stable_decimals() as i32);

        struct RiskEntry {
            btc_value: f64,
            cr_pct: f64,
            source: &'static str,
        }
        let mut tick_entries: Vec<RiskEntry> = Vec::with_capacity(5);

        // Determine SNAP placement:
        // - EARLY SNAP: when SNAPSHOT_PENDING was true at start of period AND
        //   RD didn't mint/burn (TS logs SNAP inside RD act in this case,
        //   before DLV/ALM dispatch). State captured: start-of-period (matches
        //   TS's post-collect_fees state since collect_fees is invariant for
        //   total_pool_value).
        // - LATE SNAP: otherwise, SNAP fires during TS's SNAPSHOT dispatch at
        //   end of period using post-ALM state.
        let early_snap_fires = idle_snapshot_due && snapshot_pending_at_start && !_rd_fired;
        if early_snap_fires {
            if let Some(snap_btc) = start_snapshot_btc_value {
                tick_entries.push(RiskEntry { btc_value: snap_btc, cr_pct: start_cr_pct, source: "SNAP" });
            }
        }
        if arb_fired {
            let p = Vault::volatile_price_wad(arb_pre_sqrt, is_vol_t0).lo as f64 / 1e18;
            let v = arb_nav.lo as f64 / stable_scale;
            if p > 0.0 {
                let arb_cr = if arb_debt.is_zero() { f64::INFINITY }
                    else { (arb_nav.lo as f64 + arb_debt.lo as f64) / arb_debt.lo as f64 * 100.0 };
                tick_entries.push(RiskEntry { btc_value: v / p, cr_pct: arb_cr, source: "ARB" });
            }
        }
        if lev_amm_fired {
            let p = Vault::volatile_price_wad(pool.sqrt_price_x96(), is_vol_t0).lo as f64 / 1e18;
            let v = lev_amm_nav.lo as f64 / stable_scale;
            if p > 0.0 {
                let la_cr = if lev_amm_debt.is_zero() { f64::INFINITY }
                    else { (lev_amm_nav.lo as f64 + lev_amm_debt.lo as f64) / lev_amm_debt.lo as f64 * 100.0 };
                tick_entries.push(RiskEntry { btc_value: v / p, cr_pct: la_cr, source: "LEV_AMM" });
            }
        }
        if dlv_fired {
            let p = Vault::volatile_price_wad(dlv_pre_sqrt, is_vol_t0).lo as f64 / 1e18;
            let v = dlv_nav.lo as f64 / stable_scale;
            if p > 0.0 {
                let dlv_cr = vault.collateral_ratio_pct(&pool, None);
                tick_entries.push(RiskEntry { btc_value: v / p, cr_pct: dlv_cr, source: "DLV" });
            }
        }
        if alm_fired {
            let vol_price_wad = Vault::volatile_price_wad(alm_pre_sqrt, is_vol_t0);
            let p = vol_price_wad.lo as f64 / 1e18;
            let v = alm_nav.lo as f64 / stable_scale;
            if p > 0.0 {
                let alm_cr = vault.collateral_ratio_pct(&pool, None);
                if std::env::var("ALM_BTC_DIAG").is_ok() {
                    eprintln!("[ALM-BTC-DIAG] alm_n={} tick={} alm_pre_sqrt={} vol_price_wad={} alm_nav={} p={:.18e} v={:.18e} btc={:.18e}",
                        alm_calls, tick_count,
                        alm_pre_sqrt.to_dec_string(),
                        vol_price_wad.to_dec_string(),
                        alm_nav.to_dec_string(),
                        p, v, v / p);
                }
                tick_entries.push(RiskEntry { btc_value: v / p, cr_pct: alm_cr, source: "ALM" });
            }
        }
        // LATE SNAP: when SNAP didn't fire early, push it here using post-ALM
        // state (matches TS's SNAPSHOT dispatch at end of period).
        if idle_snapshot_due && !early_snap_fires {
            let snap_cr = pre_replay_cr_pct.unwrap_or(cr_pct);
            if let Some(snap_btc) = pre_replay_snapshot_btc_value {
                tick_entries.push(RiskEntry { btc_value: snap_btc, cr_pct: snap_cr, source: "SNAP" });
            } else if price_f > 0.0 {
                tick_entries.push(RiskEntry { btc_value: vault_value_f / price_f, cr_pct: snap_cr, source: "SNAP" });
            }
        }

        for entry in &tick_entries {
            let btc_value = entry.btc_value;
            if first_apy_value.is_none() {
                first_apy_value = Some((btc_value, curr_ms as f64));
            }
            last_apy_value = Some((btc_value, curr_ms as f64));

            if btc_value > 0.0 && btc_value.is_finite() {
                if std::env::var("BTC_SRC_DUMP").is_ok() {
                    eprintln!("[BTC-SRC] idx={} source={} tick={} btc={:.18e}", btc_values_for_risk.len(), entry.source, tick_count, btc_value);
                }
                if let Ok(path) = std::env::var("BTC_SRC_DUMP_PATH") {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                        let _ = writeln!(f, "{} {} t={} btc={:.18e}", btc_values_for_risk.len(), entry.source, curr_ms, btc_value);
                    }
                }
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

            let ec = entry.cr_pct;
            if ec.is_finite() && ec > 0.0 && ec < min_cr {
                min_cr = ec;
            }
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

        let (total0, total1) = vault.total_amounts(&pool);
        let per_range = vault.per_range_amounts(&pool, None);
        let after_cr = if cr_pct.is_finite() {
            (cr_pct * 100.0).round() as i64
        } else {
            0
        };
        let lev_amm_debt = if vault.lev_amm_notional > U256::ZERO {
            vault.virtual_debt
        } else {
            U256::ZERO
        };

        writer.write_row(&RebalanceLogRow {
            timestamp_ms: curr_ms,
            date: date_str,
            rebalance_type: "SNAPSHOT",
            raw_pool_price: snap_price_wad,
            non_volatile_asset_price: snap_price_wad,
            external_price: ext_price,
            prev_total_pool_value: U256::ZERO,
            after_total_pool_value: nav,
            prev_collateral_ratio: 0,
            after_collateral_ratio: after_cr,
            lp_ratio,
            swap_fee_stable: U256::ZERO,
            alm_swap_fee_stable: U256::ZERO,
            accumulated_swap_fees0: vault.accumulated_fees0,
            accumulated_swap_fees1: vault.accumulated_fees1,
            debt: vault.virtual_debt,
            idle0: vault.idle0,
            idle1: vault.idle1,
            total0,
            total1,
            wide_amount0: per_range[0].0,
            wide_amount1: per_range[0].1,
            base_amount0: per_range[1].0,
            base_amount1: per_range[1].1,
            limit_amount0: per_range[2].0,
            limit_amount1: per_range[2].1,
            lev_amm_collateral: vault.lev_amm_collateral,
            lev_amm_notional: vault.lev_amm_notional,
            lev_amm_debt,
            lev_amm_fee_revenue: vault.lev_amm_fee_revenue,
            volatile_hold_value_stable: U256::ZERO,
            realized_il: I256::ZERO,
            swap_fees_gained_this_period: U256::ZERO,
            regulate_debt_amount: I256::ZERO,
            current_pps: U256::ZERO,
            fundamental_pps: U256::ZERO,
            equilibrium_price_wad: U256::ZERO,
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
    println!("LevAMM calls: {}", lev_amm_calls);
    println!("Slow Recenter calls: {}", slow_recenter_calls);
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
    log_returns: &[f64],
) -> bool {
    if cfg.is_regulate_debt && !vault.virtual_debt.is_zero() {
        vault.collect_fees(pool);

        if cfg.use_dynamic_width && log_returns.len() >= 30 {
            let n = log_returns.len() as f64;
            let mean = log_returns.iter().sum::<f64>() / n;
            let variance = log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
            let steps_per_year = (365.25 * 24.0 * 3600.0) / (cfg.lookup_period as f64);
            let annualized_vol = (variance * steps_per_year).sqrt();
            vault.set_dynamic_thresholds(annualized_vol);
        }

        // LevAMM manages debt — regulateDebtFrom returns noop unconditionally.
        if cfg.lev_amm.enabled {
            *regulate_debt_calls += 1;
            return false;
        }

        let rd_mode = vault.regulate_debt(pool, None, target_cr_wad);
        if rd_mode == "mint" {
            vault.deploy_idle_to_lp(pool);
        }
        *regulate_debt_calls += 1;
        rd_mode != "noop"
    } else {
        false
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
    tick_count: u64,
) {
    if !vault.virtual_debt.is_zero() {
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
                // Under LevAMM, rebalanceDebt returns ZERO (noop) — LevAMM manages
                // debt. But the DLV trigger still fires and TS still logs the entry
                // (which feeds into registerLogSummary for risk metrics).
                if !cfg.lev_amm.enabled {
                    let debt_before = vault.virtual_debt;
                    vault.rebalance_debt_dlv(pool, target_cr_wad, cfg.dlv.debt_to_volatile_swap_fee);
                    let debt_after = vault.virtual_debt;
                    eprintln!("[DLV-ACT] debtBefore={} debtAfter={} diff={} idle0={} idle1={} totalSupply={}",
                        debt_before.to_dec_string(), debt_after.to_dec_string(),
                        if debt_before > debt_after { format!("-{}", (debt_before - debt_after).to_dec_string()) }
                        else { format!("+{}", (debt_after - debt_before).to_dec_string()) },
                        vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                        vault.total_supply.to_dec_string());
                    vault.last_rebalance_ms = curr_ms;
                    vault.last_debt_rebalance_ms = curr_ms;
                }
                *dlv_calls += 1;
                emit_parity_log("DLV", *dlv_calls, tick_count, vault, pool);
            }
        }
    }
}

fn emit_parity_log(kind: &str, n: u64, tick_count: u64, vault: &Vault, pool: &CorePool) {
    if std::env::var("PARITY_CHECK").is_err() { return; }
    let (t0, t1) = vault.total_amounts(pool);
    let cr_wad = vault.collateral_ratio_wad(pool, None);
    eprintln!(
        "[PARITY] kind={} n={} tick={} total0={} total1={} idle0={} idle1={} vdebt={} pool_tick={} cr_wad={}",
        kind, n, tick_count,
        t0.to_dec_string(), t1.to_dec_string(),
        vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
        vault.virtual_debt.to_dec_string(),
        pool.tick_current(),
        cr_wad.to_dec_string(),
    );
}

fn dispatch_alm(
    cfg: &Config,
    vault: &mut Vault,
    pool: &mut CorePool,
    ext_sqrt: U256,
    curr_ms: i64,
    alm_calls: &mut u64,
    tick_count: u64,
) {
    let allow_active = cfg.active_rebalance_mode != ActiveRebalanceMode::Passive;
    let charm_rebalance_period = vault.params.period as u64 / cfg.lookup_period as u64;
    let time_trigger = charm_rebalance_period > 0 && tick_count % charm_rebalance_period == 0;
    let (ratio_trigger, dev_bps_val) = if allow_active && cfg.active_rebalance_ratio_deviation_bps > 0 {
        let dev_bps = vault.share_deviation_bps(pool, None);
        (dev_bps >= cfg.active_rebalance_ratio_deviation_bps as u64, dev_bps)
    } else {
        (false, 0)
    };

    if false && tick_count == 1514949 {
        let (t0, t1) = vault.total_amounts_round_up(pool);
        let (f0, f1) = vault.all_fees_pub(pool);
        let (lp0, lp1) = vault.lp_amounts_round_up(pool);
        eprintln!("[DEV-EDGE] tick={} dev_bps={} t0={} t1={} lp0={} lp1={} fees0={} fees1={} idle0={} idle1={} priceWad={} sqrt={} poolTick={}",
            tick_count, dev_bps_val, t0.to_dec_string(), t1.to_dec_string(),
            lp0.to_dec_string(), lp1.to_dec_string(),
            f0.to_dec_string(), f1.to_dec_string(),
            vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
            Vault::pool_price(pool.sqrt_price_x96()).to_dec_string(),
            pool.sqrt_price_x96().to_dec_string(),
            pool.tick_current());
        vault.print_position_details(pool);
    }

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
        vault.withdraw_all(pool);
        if *alm_calls >= 391 && *alm_calls <= 395 {
            eprintln!("[ALM-STEP] alm#{} tick={} AFTER-WITHDRAW idle0={} idle1={} poolTick={}",
                *alm_calls + 1, tick_count,
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string(),
                pool.tick_current());
        }
        vault.active_rebalance_swap(None, pool, cfg.dlv.debt_to_volatile_swap_fee);
        if *alm_calls >= 391 && *alm_calls <= 395 {
            eprintln!("[ALM-STEP] alm#{} tick={} AFTER-SWAP idle0={} idle1={}",
                *alm_calls + 1, tick_count,
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string());
        }
        vault.rebalance_from_idle(pool);
        if *alm_calls >= 391 && *alm_calls <= 395 {
            let bl = vault.base.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
            let ll = vault.limit.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
            eprintln!("[ALM-STEP] alm#{} tick={} AFTER-DEPLOY baseLiq={} limitLiq={} idle0={} idle1={}",
                *alm_calls + 1, tick_count, bl.to_dec_string(), ll.to_dec_string(),
                vault.idle0.to_dec_string(), vault.idle1.to_dec_string());
        }
        vault.last_rebalance_ms = curr_ms;
        *alm_calls += 1;
        {
            let bl = vault.base.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
            let ll = vault.limit.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
            eprintln!("[ALM-LOG] tick={} type=ratio dev_bps={} alm_count={} baseLiq={} limitLiq={}",
                tick_count, dev_bps_val, *alm_calls, bl.to_dec_string(), ll.to_dec_string());
        }
        emit_parity_log("ALM", *alm_calls, tick_count, vault, pool);
    } else if time_trigger {
        vault.last_rebalance_ms = curr_ms;
        *alm_calls += 1;
        {
            let bl = vault.base.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
            let ll = vault.limit.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
            eprintln!("[ALM-LOG] tick={} type=time alm_count={} baseLiq={} limitLiq={}",
                tick_count, *alm_calls, bl.to_dec_string(), ll.to_dec_string());
        }
        emit_parity_log("ALM", *alm_calls, tick_count, vault, pool);
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

    if let Ok(path) = std::env::var("BTC_DUMP_PATH") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::File::create(&path) {
            for (i, v) in btc_values.iter().enumerate() {
                let _ = writeln!(f, "{} {:.18e}", i, v);
            }
        }
    }

    let mut rets: Vec<f64> = Vec::new();
    for i in 1..btc_values.len() {
        if btc_values[i - 1] > 0.0 {
            let r = (btc_values[i] / btc_values[i - 1]).ln();
            if r.is_finite() { rets.push(r); }
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

    if std::env::var("RISK_DIAG").is_ok() {
        let neg_count = rets.iter().filter(|r| **r < 0.0).count();
        let pos_count = rets.iter().filter(|r| **r > 0.0).count();
        let zero_count = rets.iter().filter(|r| **r == 0.0).count();
        let min_abs_neg = rets.iter().filter(|r| **r < 0.0).map(|r| r.abs()).fold(f64::INFINITY, f64::min);
        let min_abs_pos = rets.iter().filter(|r| **r > 0.0).map(|r| r.abs()).fold(f64::INFINITY, f64::min);
        eprintln!("[RISK-DIAG] rets.len={} neg={} pos={} zero={} btc.len={} mean={:.18e} variance={:.18e} min_abs_neg={:.18e} min_abs_pos={:.18e}",
            rets.len(), neg_count, pos_count, zero_count, btc_values.len(), mean, variance, min_abs_neg, min_abs_pos);
    }

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
