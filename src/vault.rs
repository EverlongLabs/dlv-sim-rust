use v3_pool::core_pool::CorePool;
use v3_pool::tick_math;
use v3_pool::sqrt_price_math;
use v3_pool::full_math;
use v3_pool::types::*;

use crate::config::VaultParams;
use crate::pool_config::PoolConfig;

const MANAGER: &str = "0xVAULT_MANAGER";

fn wad() -> U256 { U256::from_u128(1_000_000_000_000_000_000) }

fn max_liquidity_for_amounts(
    sqrt_price: U256,
    tick_lower: i32,
    tick_upper: i32,
    amount0: U256,
    amount1: U256,
) -> U256 {
    let sqrt_a = tick_math::get_sqrt_ratio_at_tick(tick_lower);
    let sqrt_b = tick_math::get_sqrt_ratio_at_tick(tick_upper);

    if sqrt_price <= sqrt_a {
        liquidity_for_amount0(sqrt_a, sqrt_b, amount0)
    } else if sqrt_price < sqrt_b {
        let l0 = liquidity_for_amount0(sqrt_price, sqrt_b, amount0);
        let l1 = liquidity_for_amount1(sqrt_a, sqrt_price, amount1);
        if l0 < l1 { l0 } else { l1 }
    } else {
        liquidity_for_amount1(sqrt_a, sqrt_b, amount1)
    }
}

fn liquidity_for_amount0(sqrt_a: U256, sqrt_b: U256, amount: U256) -> U256 {
    let (sa, sb) = if sqrt_a > sqrt_b { (sqrt_b, sqrt_a) } else { (sqrt_a, sqrt_b) };
    let diff = sb - sa;
    if diff.is_zero() { return U256::ZERO; }
    let num = full_math::mul_div(amount, sa, U256::ONE);
    full_math::mul_div(num, sb, diff * Q96)
}

fn liquidity_for_amount1(sqrt_a: U256, sqrt_b: U256, amount: U256) -> U256 {
    let (sa, sb) = if sqrt_a > sqrt_b { (sqrt_b, sqrt_a) } else { (sqrt_a, sqrt_b) };
    let diff = sb - sa;
    if diff.is_zero() { return U256::ZERO; }
    full_math::mul_div(amount, Q96, diff)
}

fn amounts_for_liquidity(
    sqrt_price: U256,
    tick_lower: i32,
    tick_upper: i32,
    liquidity: U256,
) -> (U256, U256) {
    let sqrt_a = tick_math::get_sqrt_ratio_at_tick(tick_lower);
    let sqrt_b = tick_math::get_sqrt_ratio_at_tick(tick_upper);
    let liq_i = I256(liquidity);

    if sqrt_price <= sqrt_a {
        let a0 = sqrt_price_math::get_amount0_delta(sqrt_a, sqrt_b, liq_i);
        (a0.abs(), U256::ZERO)
    } else if sqrt_price < sqrt_b {
        let a0 = sqrt_price_math::get_amount0_delta(sqrt_price, sqrt_b, liq_i);
        let a1 = sqrt_price_math::get_amount1_delta(sqrt_a, sqrt_price, liq_i);
        (a0.abs(), a1.abs())
    } else {
        let a1 = sqrt_price_math::get_amount1_delta(sqrt_a, sqrt_b, liq_i);
        (U256::ZERO, a1.abs())
    }
}

#[derive(Debug, Clone)]
pub struct VaultPosition {
    pub tick_lower: i32,
    pub tick_upper: i32,
    pub liquidity: U256,
}

#[derive(Debug)]
pub struct Vault {
    pub params: VaultParams,
    pub pool_config: PoolConfig,

    pub wide: Option<VaultPosition>,
    pub base: Option<VaultPosition>,
    pub limit: Option<VaultPosition>,

    pub idle0: U256,
    pub idle1: U256,

    pub virtual_debt: U256,

    pub accumulated_fees0: U256,
    pub accumulated_fees1: U256,

    pub last_rebalance_ms: i64,
    pub last_debt_rebalance_ms: i64,

    pub tick_spacing: i32,
}

impl Vault {
    pub fn new(params: VaultParams, pool_config: PoolConfig, tick_spacing: i32) -> Self {
        Vault {
            params,
            pool_config,
            wide: None,
            base: None,
            limit: None,
            idle0: U256::ZERO,
            idle1: U256::ZERO,
            virtual_debt: U256::ZERO,
            accumulated_fees0: U256::ZERO,
            accumulated_fees1: U256::ZERO,
            last_rebalance_ms: 0,
            last_debt_rebalance_ms: 0,
            tick_spacing,
        }
    }

    // ── Price helpers (match TS poolPrice / volatilePrice) ──

    pub fn pool_price(sqrt_price_x96: U256) -> U256 {
        let price_x192 = sqrt_price_x96 * sqrt_price_x96;
        full_math::mul_div(price_x192, wad(), Q192)
    }

    pub fn volatile_price_wad(sqrt_price_x96: U256, is_volatile_token0: bool) -> U256 {
        let t1_per_t0 = Self::pool_price(sqrt_price_x96);
        if is_volatile_token0 {
            t1_per_t0
        } else {
            if t1_per_t0.is_zero() { return U256::ZERO; }
            full_math::mul_div(wad(), wad(), t1_per_t0)
        }
    }

    // ── Deposit ──

    pub fn deposit(&mut self, pool: &mut CorePool, amount0: U256, amount1: U256) {
        self.idle0 = self.idle0 + amount0;
        self.idle1 = self.idle1 + amount1;
        self.rebalance_from_idle(pool);
    }

    // ── rebalanceDebt (TS: rebalanceDebt) — initial debt setup ──

    pub fn rebalance_debt(&mut self, pool: &mut CorePool, target_cr_wad: U256) {
        let w = wad();
        if target_cr_wad <= w { return; }
        let gav = self.total_value_in_stable(pool, None);
        if gav.is_zero() { return; }

        let borrow = full_math::mul_div(gav, w, target_cr_wad - w);
        if borrow.is_zero() { return; }

        self.virtual_debt = borrow;
        if self.pool_config.is_volatile_token0() {
            self.idle1 = self.idle1 + borrow;
        } else {
            self.idle0 = self.idle0 + borrow;
        }

        self.withdraw_all(pool);
        self.rebalance_from_idle(pool);
    }

    // ── regulateDebt (TS: regulateDebt, 'both' mode) ──
    // Mint: CR > TC AND V > S  →  amount = min(almCorrection, debtCorrection)
    // Burn: CR < TC AND S > V  →  amount = min(almCorrection, debtCorrection), clamped by idle

    pub fn regulate_debt(&mut self, pool: &CorePool, override_sqrt: Option<U256>, target_cr_wad: U256) -> &'static str {
        if self.virtual_debt.is_zero() { return "noop"; }
        let w = wad();
        if target_cr_wad <= w { return "noop"; }

        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);

        let (total0, total1) = self.total_amounts(pool);

        let (volatile_amt, stable_amt) = if self.pool_config.is_volatile_token0() {
            (total0, total1)
        } else {
            (total1, total0)
        };

        // V = volatileValueInStable, S = stableAmt
        let v = if self.pool_config.is_volatile_token0() {
            full_math::mul_div(volatile_amt, price_wad, w)
        } else {
            if price_wad.is_zero() { return "noop"; }
            full_math::mul_div(volatile_amt, w, price_wad)
        };

        let s = stable_amt;
        let d = self.virtual_debt;
        let tc = target_cr_wad;
        let gav = s + v;
        let cr_wad = full_math::mul_div(gav, w, d);
        let denominator = tc - w;
        if denominator.is_zero() { return "noop"; }

        // Case 1: CR > target AND V > S → mint stable debt
        if cr_wad > tc && v > s {
            let alm_correction = v - s;
            let gav_w = gav * w;
            let tc_d = tc * d;
            if gav_w > tc_d {
                let debt_correction = (gav_w - tc_d) / denominator;
                let amount = alm_correction.min(debt_correction);
                if !amount.is_zero() {
                    self.virtual_debt = self.virtual_debt + amount;
                    if self.pool_config.is_volatile_token0() {
                        self.idle1 = self.idle1 + amount;
                    } else {
                        self.idle0 = self.idle0 + amount;
                    }
                    return "mint";
                }
            }
        }
        // Case 2: CR < target AND S > V → burn stable debt
        else if cr_wad < tc && s > v {
            let alm_correction = s - v;
            let tc_d = tc * d;
            let gav_w = gav * w;
            if tc_d > gav_w {
                let debt_correction = (tc_d - gav_w) / denominator;
                let mut amount = alm_correction.min(debt_correction);
                amount = amount.min(self.virtual_debt);
                let stable_idle = if self.pool_config.is_volatile_token0() {
                    self.idle1
                } else {
                    self.idle0
                };
                amount = amount.min(stable_idle);
                if !amount.is_zero() {
                    self.virtual_debt = self.virtual_debt - amount;
                    if self.pool_config.is_volatile_token0() {
                        self.idle1 = self.idle1 - amount;
                    } else {
                        self.idle0 = self.idle0 - amount;
                    }
                    return "burn";
                }
            }
        }
        "noop"
    }

    // ── _rebalanceFromIdle (TS: _rebalanceFromIdle with recenterTicks=true) ──

    pub fn rebalance_from_idle(&mut self, pool: &mut CorePool) {
        let tick_current = pool.tick_current();
        let ts = self.tick_spacing;
        let tick_floor = (tick_current / ts) * ts;

        let base_lower = (tick_floor - self.params.base_threshold).max(MIN_TICK);
        let base_upper = (tick_floor + self.params.base_threshold).min(MAX_TICK);

        if base_lower < base_upper && (!self.idle0.is_zero() || !self.idle1.is_zero()) {
            let liq = max_liquidity_for_amounts(
                pool.sqrt_price_x96(),
                base_lower,
                base_upper,
                self.idle0,
                self.idle1,
            );

            if !liq.is_zero() {
                let liq_i = I256(liq);
                let (a0, a1) = pool.mint(MANAGER, base_lower, base_upper, liq_i);
                let used0 = a0.abs();
                let used1 = a1.abs();
                self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
                self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
                self.base = Some(VaultPosition {
                    tick_lower: base_lower,
                    tick_upper: base_upper,
                    liquidity: liq,
                });
            }
        }

        if self.params.wide_range_weight > 0 && (!self.idle0.is_zero() || !self.idle1.is_zero()) {
            let wide_lower = (tick_floor - self.params.wide_threshold).max(MIN_TICK);
            let wide_upper = (tick_floor + self.params.wide_threshold).min(MAX_TICK);

            if wide_lower >= wide_upper { return; }

            let liq = max_liquidity_for_amounts(
                pool.sqrt_price_x96(),
                wide_lower,
                wide_upper,
                self.idle0,
                self.idle1,
            );

            if !liq.is_zero() {
                let liq_i = I256(liq);
                let (a0, a1) = pool.mint(MANAGER, wide_lower, wide_upper, liq_i);
                let used0 = a0.abs();
                let used1 = a1.abs();
                self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
                self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
                self.wide = Some(VaultPosition {
                    tick_lower: wide_lower,
                    tick_upper: wide_upper,
                    liquidity: liq,
                });
            }
        }
    }

    // ── deployIdleToLP (TS: deployIdleToLP for 3pos) ──
    // Mints idle into existing positions without recentering ticks.

    pub fn deploy_idle_to_lp(&mut self, pool: &mut CorePool) {
        if self.idle0.is_zero() && self.idle1.is_zero() { return; }

        if let Some(ref mut pos) = self.base {
            if !self.idle0.is_zero() || !self.idle1.is_zero() {
                let liq = max_liquidity_for_amounts(
                    pool.sqrt_price_x96(),
                    pos.tick_lower,
                    pos.tick_upper,
                    self.idle0,
                    self.idle1,
                );
                if !liq.is_zero() {
                    let (a0, a1) = pool.mint(MANAGER, pos.tick_lower, pos.tick_upper, I256(liq));
                    let used0 = a0.abs();
                    let used1 = a1.abs();
                    self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
                    self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
                    pos.liquidity = pos.liquidity + liq;
                }
            }
        }

        if let Some(ref mut pos) = self.wide {
            if !self.idle0.is_zero() || !self.idle1.is_zero() {
                let liq = max_liquidity_for_amounts(
                    pool.sqrt_price_x96(),
                    pos.tick_lower,
                    pos.tick_upper,
                    self.idle0,
                    self.idle1,
                );
                if !liq.is_zero() {
                    let (a0, a1) = pool.mint(MANAGER, pos.tick_lower, pos.tick_upper, I256(liq));
                    let used0 = a0.abs();
                    let used1 = a1.abs();
                    self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
                    self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
                    pos.liquidity = pos.liquidity + liq;
                }
            }
        }
    }

    // ── active rebalance swap (synthetic, matching TS _swapImbalance) ──

    pub fn active_rebalance_swap(&mut self, override_sqrt: Option<U256>, pool: &CorePool, swap_fee: f64) {
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        let w = wad();
        if price_wad.is_zero() { return; }

        let (volatile_idle, stable_idle) = if self.pool_config.is_volatile_token0() {
            (self.idle0, self.idle1)
        } else {
            (self.idle1, self.idle0)
        };

        let volatile_value_stable = if self.pool_config.is_volatile_token0() {
            full_math::mul_div(volatile_idle, price_wad, w)
        } else {
            full_math::mul_div(volatile_idle, w, price_wad)
        };
        let stable_value = stable_idle;

        if volatile_value_stable.is_zero() && stable_value.is_zero() { return; }

        let is_stable_heavy = stable_value > volatile_value_stable;
        let diff = if is_stable_heavy {
            stable_value - volatile_value_stable
        } else {
            volatile_value_stable - stable_value
        };
        if diff.is_zero() { return; }

        let fee_den = U256::from_u128(1_000_000);
        let fee_num = U256::from_u128((swap_fee * 1_000_000.0) as u128);
        let one_minus_fee = fee_den - fee_num;
        let denom = fee_den + fee_den - fee_num;
        if denom.is_zero() { return; }

        let stable_to_swap = full_math::mul_div(diff, fee_den, denom);
        if stable_to_swap.is_zero() { return; }

        if is_stable_heavy {
            let capped = if stable_to_swap > stable_idle { stable_idle } else { stable_to_swap };
            if capped.is_zero() { return; }
            let effective_in = full_math::mul_div(capped, one_minus_fee, fee_den);
            let received_volatile = if self.pool_config.is_volatile_token0() {
                full_math::mul_div(effective_in, w, price_wad)
            } else {
                full_math::mul_div(effective_in, price_wad, w)
            };
            if received_volatile.is_zero() { return; }
            if self.pool_config.is_volatile_token0() {
                self.idle1 = self.idle1 - capped;
                self.idle0 = self.idle0 + received_volatile;
            } else {
                self.idle0 = self.idle0 - capped;
                self.idle1 = self.idle1 + received_volatile;
            }
        } else {
            let volatile_to_send = full_math::mul_div(stable_to_swap, w, price_wad);
            let volatile_to_send = if self.pool_config.is_volatile_token0() {
                volatile_to_send
            } else {
                full_math::mul_div(stable_to_swap, price_wad, w)
            };
            let capped = if volatile_to_send > volatile_idle { volatile_idle } else { volatile_to_send };
            if capped.is_zero() { return; }
            let sent_value_stable = if self.pool_config.is_volatile_token0() {
                full_math::mul_div(capped, price_wad, w)
            } else {
                full_math::mul_div(capped, w, price_wad)
            };
            let effective_value = full_math::mul_div(sent_value_stable, one_minus_fee, fee_den);
            if effective_value.is_zero() { return; }
            if self.pool_config.is_volatile_token0() {
                self.idle0 = self.idle0 - capped;
                self.idle1 = self.idle1 + effective_value;
            } else {
                self.idle1 = self.idle1 - capped;
                self.idle0 = self.idle0 + effective_value;
            }
        }
    }

    // ── withdraw_all ──

    pub fn withdraw_all(&mut self, pool: &mut CorePool) {
        let positions = [self.wide.take(), self.base.take(), self.limit.take()];
        for pos in positions {
            if let Some(p) = pos {
                if !p.liquidity.is_zero() {
                    pool.burn(MANAGER, p.tick_lower, p.tick_upper, I256(p.liquidity));
                    let (c0, c1) = pool.collect(
                        MANAGER, p.tick_lower, p.tick_upper, MAX_UINT128, MAX_UINT128,
                    );
                    self.idle0 = self.idle0 + c0;
                    self.idle1 = self.idle1 + c1;
                }
            }
        }
    }

    // ── collectFees ──

    pub fn collect_fees(&mut self, pool: &mut CorePool) {
        let positions: Vec<Option<&VaultPosition>> =
            vec![self.wide.as_ref(), self.base.as_ref(), self.limit.as_ref()];
        for pos in positions.into_iter().flatten() {
            let _ = pool.burn(MANAGER, pos.tick_lower, pos.tick_upper, I256::ZERO);
            let (c0, c1) = pool.collect(
                MANAGER, pos.tick_lower, pos.tick_upper, MAX_UINT128, MAX_UINT128,
            );
            self.accumulated_fees0 = self.accumulated_fees0 + c0;
            self.accumulated_fees1 = self.accumulated_fees1 + c1;
            self.idle0 = self.idle0 + c0;
            self.idle1 = self.idle1 + c1;
        }
    }

    // ── Amount / value helpers (match TS getTotalAmounts, totalValueInStable, etc.) ──

    pub fn lp_amounts(&self, pool: &CorePool) -> (U256, U256) {
        let sqrt_price = pool.sqrt_price_x96();
        let mut total0 = U256::ZERO;
        let mut total1 = U256::ZERO;

        for pos in [&self.wide, &self.base, &self.limit] {
            if let Some(p) = pos {
                if !p.liquidity.is_zero() {
                    let (a0, a1) = amounts_for_liquidity(
                        sqrt_price,
                        p.tick_lower,
                        p.tick_upper,
                        p.liquidity,
                    );
                    total0 = total0 + a0;
                    total1 = total1 + a1;
                }
            }
        }

        (total0, total1)
    }

    pub fn total_amounts(&self, pool: &CorePool) -> (U256, U256) {
        let (lp0, lp1) = self.lp_amounts(pool);
        (lp0 + self.idle0, lp1 + self.idle1)
    }

    /// GAV = stableAmt + volatileValueInStable (matches TS totalValueInStable)
    pub fn total_value_in_stable(&self, pool: &CorePool, override_sqrt: Option<U256>) -> U256 {
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        let w = wad();

        let (total0, total1) = self.total_amounts(pool);

        if self.pool_config.is_volatile_token0() {
            let vol_as_stable = full_math::mul_div(total0, price_wad, w);
            total1 + vol_as_stable
        } else {
            if !price_wad.is_zero() {
                let vol_as_stable = full_math::mul_div(total1, w, price_wad);
                total0 + vol_as_stable
            } else {
                total0
            }
        }
    }

    /// NAV = GAV - virtualDebt (matches TS totalPoolValue)
    pub fn total_pool_value(&self, pool: &CorePool, override_sqrt: Option<U256>) -> U256 {
        let gav = self.total_value_in_stable(pool, override_sqrt);
        if gav > self.virtual_debt { gav - self.virtual_debt } else { U256::ZERO }
    }

    /// lpRatio = stableAmt * WAD / volatileValueInStable (matches TS lpRatio)
    /// Returns WAD-scaled ratio. 1.0 WAD = equal stable and volatile value.
    pub fn lp_ratio(&self, pool: &CorePool, override_sqrt: Option<U256>) -> U256 {
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        let w = wad();

        let (total0, total1) = self.total_amounts(pool);

        let (volatile_amt, stable_amt) = if self.pool_config.is_volatile_token0() {
            (total0, total1)
        } else {
            (total1, total0)
        };

        let volatile_value_stable = if self.pool_config.is_volatile_token0() {
            full_math::mul_div(volatile_amt, price_wad, w)
        } else {
            if price_wad.is_zero() { return U256::from_u128(u128::MAX / 2); }
            full_math::mul_div(volatile_amt, w, price_wad)
        };

        if volatile_value_stable.is_zero() {
            return U256::from_u128(u128::MAX / 2);
        }
        full_math::mul_div(stable_amt, w, volatile_value_stable)
    }

    /// CR in WAD = GAV * WAD / virtualDebt (matches TS collateralRatioWad)
    pub fn collateral_ratio_wad(&self, pool: &CorePool, override_sqrt: Option<U256>) -> U256 {
        if self.virtual_debt.is_zero() {
            return U256::from_u128(u128::MAX / 2);
        }
        let value = self.total_value_in_stable(pool, override_sqrt);
        full_math::mul_div(value, wad(), self.virtual_debt)
    }

    /// |stableShareBps - 5000| (matches TS shareDeviationBpsFromValues)
    pub fn share_deviation_bps(&self, pool: &CorePool, override_sqrt: Option<U256>) -> u64 {
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        let w = wad();
        let (total0, total1) = self.total_amounts(pool);
        let (volatile_amt, stable_amt) = if self.pool_config.is_volatile_token0() {
            (total0, total1)
        } else {
            (total1, total0)
        };
        let volatile_value_stable = if self.pool_config.is_volatile_token0() {
            full_math::mul_div(volatile_amt, price_wad, w)
        } else {
            if price_wad.is_zero() { return 0; }
            full_math::mul_div(volatile_amt, w, price_wad)
        };
        let total_value = stable_amt + volatile_value_stable;
        if total_value.is_zero() { return 0; }
        let bps_scale = U256::from_u128(10_000);
        let half_bps = U256::from_u128(5_000);
        let stable_share_bps = full_math::mul_div(stable_amt, bps_scale, total_value);
        if stable_share_bps > half_bps {
            (stable_share_bps - half_bps).lo as u64
        } else {
            (half_bps - stable_share_bps).lo as u64
        }
    }

    pub fn collateral_ratio_pct(&self, pool: &CorePool, override_sqrt: Option<U256>) -> f64 {
        if self.virtual_debt.is_zero() { return f64::INFINITY; }
        let cr_wad = self.collateral_ratio_wad(pool, override_sqrt);
        cr_wad.lo as f64 / 1e18 * 100.0
    }
}
