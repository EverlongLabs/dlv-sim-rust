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

fn cap_liquidity(
    sqrt_price: U256,
    tick_lower: i32,
    tick_upper: i32,
    liq_raw: U256,
    idle0: U256,
    idle1: U256,
) -> U256 {
    if liq_raw.is_zero() { return U256::ZERO; }
    let (need0, need1) = amounts_for_liquidity_round(sqrt_price, tick_lower, tick_upper, liq_raw, true);
    let w = wad();
    let mut scale = w;
    if need0 > idle0 {
        let s = full_math::mul_div(w, idle0, need0);
        if s < scale { scale = s; }
    }
    if need1 > idle1 {
        let s = full_math::mul_div(w, idle1, need1);
        if s < scale { scale = s; }
    }
    if scale.is_zero() { return U256::ZERO; }
    full_math::mul_div(liq_raw, scale, w)
}

fn amounts_for_liquidity(
    sqrt_price: U256,
    tick_lower: i32,
    tick_upper: i32,
    liquidity: U256,
) -> (U256, U256) {
    amounts_for_liquidity_round(sqrt_price, tick_lower, tick_upper, liquidity, false)
}

fn ceil_div(a: U256, b: U256) -> U256 {
    if b.is_zero() { return U256::ZERO; }
    let d = a / b;
    if a % b != U256::ZERO { d + U256::ONE } else { d }
}

fn amounts_for_liquidity_round(
    sqrt_price: U256,
    tick_lower: i32,
    tick_upper: i32,
    liquidity: U256,
    round_up: bool,
) -> (U256, U256) {
    let sqrt_a = tick_math::get_sqrt_ratio_at_tick(tick_lower);
    let sqrt_b = tick_math::get_sqrt_ratio_at_tick(tick_upper);

    if sqrt_price <= sqrt_a {
        let a0 = sqrt_price_math::get_amount0_delta_unsigned(sqrt_a, sqrt_b, liquidity, round_up);
        (a0, U256::ZERO)
    } else if sqrt_price < sqrt_b {
        let a0 = sqrt_price_math::get_amount0_delta_unsigned(sqrt_price, sqrt_b, liquidity, round_up);
        let a1 = sqrt_price_math::get_amount1_delta_unsigned(sqrt_a, sqrt_price, liquidity, round_up);
        (a0, a1)
    } else {
        let a1 = sqrt_price_math::get_amount1_delta_unsigned(sqrt_a, sqrt_b, liquidity, round_up);
        (U256::ZERO, a1)
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

    pub total_supply: U256,
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
            total_supply: U256::ZERO,
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

    // ── Price conversion helpers ──

    fn volatile_to_stable_val(&self, volatile_raw: U256, price_wad: U256) -> U256 {
        let w = wad();
        if self.pool_config.is_volatile_token0() {
            full_math::mul_div(volatile_raw, price_wad, w)
        } else {
            if price_wad.is_zero() { return U256::ZERO; }
            full_math::mul_div(volatile_raw, w, price_wad)
        }
    }

    fn stable_to_volatile_val(&self, stable_raw: U256, price_wad: U256) -> U256 {
        let w = wad();
        if self.pool_config.is_volatile_token0() {
            if price_wad.is_zero() { return U256::ZERO; }
            full_math::mul_div(stable_raw, w, price_wad)
        } else {
            full_math::mul_div(stable_raw, price_wad, w)
        }
    }

    // ── Deposit ──

    pub fn deposit(&mut self, pool: &mut CorePool, amount0: U256, amount1: U256) {
        let minimum_liquidity = U256::from_u128(1000);
        if self.total_supply.is_zero() {
            let max_amt = amount0.max(amount1);
            let shares = if max_amt > minimum_liquidity { max_amt - minimum_liquidity } else { U256::ZERO };
            self.total_supply = minimum_liquidity + shares; // = max(a0, a1)
        }
        self.idle0 = self.idle0 + amount0;
        self.idle1 = self.idle1 + amount1;
        self.rebalance_from_idle(pool);
    }

    // ── deposit_pro_rata: matches TS deposit() proportional minting ──
    // Used by rebalanceDebt leverage path to add tokens proportionally to existing positions.

    pub fn deposit_pro_rata(&mut self, pool: &mut CorePool, amount0_desired: U256, amount1_desired: U256) -> (U256, U256) {
        let (total0, total1) = self.total_amounts_round_up(pool);
        let ts = self.total_supply;

        if ts.is_zero() || (total0.is_zero() && total1.is_zero()) {
            return (U256::ZERO, U256::ZERO);
        }

        // _calcSharesAndAmounts (TS)
        let (amount0, amount1, shares) = if total0.is_zero() {
            let shares = full_math::mul_div(amount1_desired, ts, total1);
            (U256::ZERO, amount1_desired, shares)
        } else if total1.is_zero() {
            let shares = full_math::mul_div(amount0_desired, ts, total0);
            (amount0_desired, U256::ZERO, shares)
        } else {
            let cross0 = amount0_desired * total1;
            let cross1 = amount1_desired * total0;
            let cross = cross0.min(cross1);
            if cross.is_zero() {
                return (U256::ZERO, U256::ZERO);
            }
            let amount0 = ceil_div(cross, total1);
            let amount1 = ceil_div(cross, total0);
            let shares = (cross * ts) / (total0 * total1);
            (amount0, amount1, shares)
        };

        if shares.is_zero() {
            return (U256::ZERO, U256::ZERO);
        }

        // Add to idle (TS: pull into idle balances)
        self.idle0 = self.idle0 + amount0;
        self.idle1 = self.idle1 + amount1;

        let sqrt_price = pool.sqrt_price_x96();

        // Collect position info before mutating
        let pos_info: Vec<(i32, i32, U256, usize)> = [&self.wide, &self.base, &self.limit]
            .iter()
            .enumerate()
            .filter_map(|(idx, p)| {
                p.as_ref()
                    .filter(|pp| !pp.liquidity.is_zero())
                    .map(|pp| (pp.tick_lower, pp.tick_upper, pp.liquidity, idx))
            })
            .collect();

        let mut added_liq = [U256::ZERO; 3];

        for &(tick_lower, tick_upper, pos_liq, idx) in &pos_info {
            // liqToMint = mulDiv(pos.liquidity, shares, totalSupply)
            let liq_to_mint = full_math::mul_div(pos_liq, shares, ts);

            // Cap to what's mintable from remaining idle
            let liq_from_amts = max_liquidity_for_amounts(
                sqrt_price, tick_lower, tick_upper, self.idle0, self.idle1,
            );
            let liq = liq_to_mint.min(liq_from_amts);
            if liq.is_zero() { continue; }

            let (a0, a1) = pool.mint(MANAGER, tick_lower, tick_upper, I256(liq));
            let used0 = a0.abs();
            let used1 = a1.abs();
            self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
            self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
            added_liq[idx] = liq;
        }

        // Update position liquidities
        if !added_liq[0].is_zero() {
            if let Some(ref mut p) = self.wide { p.liquidity = p.liquidity + added_liq[0]; }
        }
        if !added_liq[1].is_zero() {
            if let Some(ref mut p) = self.base { p.liquidity = p.liquidity + added_liq[1]; }
        }
        if !added_liq[2].is_zero() {
            if let Some(ref mut p) = self.limit { p.liquidity = p.liquidity + added_liq[2]; }
        }

        // Update total supply
        self.total_supply = self.total_supply + shares;

        (amount0, amount1)
    }

    // ── rebalanceBorrowedAmount (TS: vault.rebalanceBorrowedAmount) ──
    // Fee-adjusted leverage/deleverage computation with binary search.
    // Returns (mode, borrow_or_repay, swap_fee_stable): mode 0=noop, 1=leverage, 2=deleverage

    fn rebalance_borrowed_amount(
        &self, pool: &CorePool, override_sqrt: Option<U256>,
        target_cr_wad: U256, swap_fee: f64,
    ) -> (u8, U256, U256) {
        let w = wad();
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        if price_wad.is_zero() { return (0, U256::ZERO, U256::ZERO); }

        let (total0, total1) = self.total_amounts(pool);
        let (total0_ru, total1_ru) = self.total_amounts_round_up(pool);

        let (volatile0, stable0) = if self.pool_config.is_volatile_token0() {
            (total0, total1)
        } else {
            (total1, total0)
        };

        let d0 = self.virtual_debt;
        let volatile_val_stable = self.volatile_to_stable_val(volatile0, price_wad);
        let v0 = stable0 + volatile_val_stable;
        let target_v0 = full_math::mul_div(d0, target_cr_wad, w);

        let fee_den = U256::from_u128(1_000_000);
        let fee_num = U256::from_u128((swap_fee * 1_000_000.0) as u128);
        let one_minus_fee = fee_den - fee_num;
        let f_wad = full_math::mul_div(w, fee_num, fee_den);

        let (volatile_ru, stable_ru) = if self.pool_config.is_volatile_token0() {
            (total0_ru, total1_ru)
        } else {
            (total1_ru, total0_ru)
        };
        let volatile_val_stable_ru = self.volatile_to_stable_val(volatile_ru, price_wad);
        let rw = if volatile_val_stable_ru.is_zero() {
            U256::from_dec_str("1000000000000000000000000000000000000")
        } else {
            full_math::mul_div(stable_ru, w, volatile_val_stable_ru)
        };
        let r_plus_1_wad = rw + w;

        if v0 > target_v0 {
            // ===== LEVERAGE =====
            let term_r_1_minus_f = full_math::mul_div(rw, one_minus_fee, fee_den);
            let denom_wad = w + term_r_1_minus_f;
            let denom_plus_fee_wad = denom_wad + f_wad;
            let surplus = v0 - target_v0;

            let b = full_math::mul_div(surplus, denom_wad, denom_plus_fee_wad);
            if b.is_zero() { return (0, U256::ZERO, U256::ZERO); }

            let mut x = full_math::mul_div(b, w, denom_wad);
            let mut swap_fee_usdc = full_math::mul_div(x, fee_num, fee_den);
            let x_eff = if x > swap_fee_usdc { x - swap_fee_usdc } else { U256::ZERO };
            let mut volatile_received = self.stable_to_volatile_val(x_eff, price_wad);
            let mut stable_deposit_plan = if b > x { b - x } else { U256::ZERO };

            // ── robust pair selection (matches TS when thresholds < 1.0) ──
            // TS uses raw token amounts for rRU/rRD (not value-converted),
            // so the ratio has dimensions of (stable_raw / volatile_raw * WAD).
            if !volatile_ru.is_zero() && !stable_ru.is_zero()
                && !volatile0.is_zero() && !stable0.is_zero()
            {
                let r_ru_val = full_math::mul_div(stable_ru, w, volatile_ru);
                let r_rd_val = full_math::mul_div(stable0, w, volatile0);

                let below_ppm = U256::from_u128(60_000);
                let above_ppm = U256::from_u128(60_000);

                let r_min = r_ru_val.min(r_rd_val);
                let r_max = r_ru_val.max(r_rd_val);

                let r_lb = full_math::mul_div(r_min, fee_den - below_ppm, fee_den);
                let r_ub = full_math::mul_div(r_max, fee_den + above_ppm, fee_den);

                let mut vr = volatile_received;
                let mut a1 = full_math::mul_div(vr, r_lb, w);

                let v_min = full_math::mul_div_rounding_up(a1, w, r_ub);
                let delta_cushion = U256::from_u128(10);

                if vr + delta_cushion >= v_min {
                    let target_vr = if v_min > delta_cushion { v_min - delta_cushion } else { U256::ZERO };
                    let x_eff_target = self.volatile_to_stable_val(target_vr, price_wad);
                    let x_new = full_math::mul_div_rounding_up(x_eff_target, fee_den, one_minus_fee);
                    let swap_fee_new = full_math::mul_div(x_new, fee_num, fee_den);
                    let x_eff_new = if x_new > swap_fee_new { x_new - swap_fee_new } else { U256::ZERO };
                    vr = self.stable_to_volatile_val(x_eff_new, price_wad);
                    a1 = full_math::mul_div(vr, r_lb, w);
                    x = x_new;
                    swap_fee_usdc = swap_fee_new;
                    volatile_received = vr;
                }
                stable_deposit_plan = a1;
            }

            let swap_fee_final = full_math::mul_div(x, fee_num, fee_den);
            let required_stable = x + stable_deposit_plan;
            let net_base = if required_stable > swap_fee_final {
                required_stable - swap_fee_final
            } else {
                U256::ZERO
            };

            let scale_num = if v0 * w > target_cr_wad * d0 {
                v0 * w - target_cr_wad * d0
            } else {
                U256::ZERO
            };
            let scale_denom = if target_cr_wad * required_stable > w * net_base {
                target_cr_wad * required_stable - w * net_base
            } else {
                U256::ONE
            };

            let borrow_target = if required_stable.is_zero() || scale_num.is_zero() {
                required_stable
            } else {
                full_math::mul_div_rounding_up(required_stable, scale_num, scale_denom)
            };

            // ── Binary search to refine post-CR within 0.1% of target ──
            let compute_post_cr = |borrow: U256| -> U256 {
                let mut st_scaled = if required_stable.is_zero() {
                    U256::ZERO
                } else {
                    full_math::mul_div_rounding_up(stable_deposit_plan, borrow, required_stable)
                };
                if st_scaled > borrow { st_scaled = borrow; }
                let swap_scaled = if borrow > st_scaled { borrow - st_scaled } else { U256::ZERO };
                let swap_fee_s = full_math::mul_div(swap_scaled, fee_num, fee_den);
                let net_stable = if borrow > swap_fee_s { borrow - swap_fee_s } else { U256::ZERO };
                let v1 = v0 + net_stable;
                let d1 = d0 + borrow;
                full_math::mul_div(v1, w, d1)
            };

            let cr_tol = full_math::mul_div(target_cr_wad, U256::ONE, U256::from_u128(1000));
            let post_cr_init = compute_post_cr(borrow_target);
            let abs_diff = |a: U256, b: U256| -> U256 { if a > b { a - b } else { b - a } };
            let mut best_diff = abs_diff(post_cr_init, target_cr_wad);
            let mut best_borrow = borrow_target;

            if best_diff > cr_tol {
                let mut lo = if borrow_target > U256::ONE { borrow_target / U256::from_u128(2) } else { U256::ONE };
                let mut hi = borrow_target * U256::from_u128(2);
                for _ in 0..40 {
                    if hi - lo <= U256::ONE { break; }
                    let mid = (lo + hi) / U256::from_u128(2);
                    let trial_cr = compute_post_cr(mid);
                    let diff = abs_diff(trial_cr, target_cr_wad);
                    if diff < best_diff {
                        best_diff = diff;
                        best_borrow = mid;
                    }
                    if trial_cr > target_cr_wad {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                    if best_diff <= cr_tol { break; }
                }
            }

            if best_borrow.is_zero() { return (0, U256::ZERO, U256::ZERO); }

            // Compute swap fee for the best borrow (matches TS computeScaledPlan)
            let st_scaled_final = if required_stable.is_zero() {
                U256::ZERO
            } else {
                let s = full_math::mul_div_rounding_up(stable_deposit_plan, best_borrow, required_stable);
                if s > best_borrow { best_borrow } else { s }
            };
            let swap_scaled_final = if best_borrow > st_scaled_final { best_borrow - st_scaled_final } else { U256::ZERO };
            let swap_fee_final = full_math::mul_div(swap_scaled_final, fee_num, fee_den);

            (1, best_borrow, swap_fee_final)
        } else if target_v0 > v0 {
            // ===== DELEVERAGE =====
            let two_f_wad = f_wad * U256::from_u128(2);
            let denom_del_term = full_math::mul_div(two_f_wad, w, r_plus_1_wad);
            if denom_del_term >= w {
                let repay = v0.min(d0);
                return (2, repay, U256::ZERO);
            }
            let denom_del_wad = w - denom_del_term;
            let deficit = target_v0 - v0;

            let w_val_num = full_math::mul_div(deficit, w, U256::ONE);
            let w_val = if w_val_num % denom_del_wad != U256::ZERO {
                w_val_num / denom_del_wad + U256::ONE
            } else {
                w_val_num / denom_del_wad
            };
            let w_cap = w_val.min(v0);

            let stable_out = full_math::mul_div(w_cap, rw, r_plus_1_wad);
            let volatile_val_out = if w_cap > stable_out { w_cap - stable_out } else { U256::ZERO };
            let swap_fee_del = full_math::mul_div(volatile_val_out, fee_num, fee_den);
            let stable_from_volatile = if volatile_val_out > swap_fee_del {
                volatile_val_out - swap_fee_del
            } else {
                U256::ZERO
            };
            let repay = (stable_out + stable_from_volatile).min(d0);

            (2, repay, U256::ZERO)
        } else {
            (0, U256::ZERO, U256::ZERO)
        }
    }

    // ── rebalanceDebt (TS: rebalanceDebt) — shared by initial setup and DLV ──

    pub fn rebalance_debt(&mut self, pool: &mut CorePool, target_cr_wad: U256, swap_fee: f64) {
        let w = wad();
        if target_cr_wad <= w { return; }

        let (mode, amount, swap_fee_stable) = self.rebalance_borrowed_amount(pool, None, target_cr_wad, swap_fee);
        if mode == 0 || amount.is_zero() { return; }

        if mode == 1 {
            // Leverage: match TS rebalanceDebt pro-rata deposit logic
            let sqrt = pool.sqrt_price_x96();
            let price_wad = Self::pool_price(sqrt);

            // net new value added (stable units after swap fee)
            let net_new_stable = if amount > swap_fee_stable { amount - swap_fee_stable } else { U256::ZERO };

            // Current vault composition (roundDown, matches TS getTotalAmounts(false))
            let (cur0, cur1) = self.total_amounts(pool);
            let (cur_volatile, cur_stable) = if self.pool_config.is_volatile_token0() {
                (cur0, cur1)
            } else {
                (cur1, cur0)
            };
            let cur_volatile_value = self.volatile_to_stable_val(cur_volatile, price_wad);
            let cur_total = cur_stable + cur_volatile_value;

            let (deposit_volatile, deposit_stable) = if cur_total.is_zero() {
                // Vault empty — fallback (shouldn't happen in normal flow)
                (U256::ZERO, net_new_stable)
            } else {
                let stable_frac = full_math::mul_div(cur_stable, w, cur_total);
                let dep_stable = full_math::mul_div(net_new_stable, stable_frac, w);
                let volatile_value_stable = if net_new_stable > dep_stable { net_new_stable - dep_stable } else { U256::ZERO };
                let dep_volatile = self.stable_to_volatile_val(volatile_value_stable, price_wad);
                (dep_volatile, dep_stable)
            };

            let (amount0_desired, amount1_desired) = if self.pool_config.is_volatile_token0() {
                (deposit_volatile, deposit_stable)
            } else {
                (deposit_stable, deposit_volatile)
            };

            // Add debt first (matches TS: virtualDebt += borrowStable)
            self.virtual_debt = self.virtual_debt + amount;

            // Pro-rata deposit into existing positions (matches TS this.deposit())
            let (actual0, actual1) = self.deposit_pro_rata(pool, amount0_desired, amount1_desired);

            // Credit excess to idle (matches TS excess token handling)
            let excess0 = if amount0_desired > actual0 { amount0_desired - actual0 } else { U256::ZERO };
            let excess1 = if amount1_desired > actual1 { amount1_desired - actual1 } else { U256::ZERO };
            if !excess0.is_zero() { self.idle0 = self.idle0 + excess0; }
            if !excess1.is_zero() { self.idle1 = self.idle1 + excess1; }
        } else {
            // Deleverage: withdraw all, repay debt, swap, redeploy
            self.withdraw_all(pool);
            let repay = amount.min(self.virtual_debt);
            let stable_idle = if self.pool_config.is_volatile_token0() {
                self.idle1
            } else {
                self.idle0
            };
            let repay = repay.min(stable_idle);
            if !repay.is_zero() {
                self.virtual_debt = self.virtual_debt - repay;
                if self.pool_config.is_volatile_token0() {
                    self.idle1 = self.idle1 - repay;
                } else {
                    self.idle0 = self.idle0 - repay;
                }
            }
            self.active_rebalance_swap(None, pool, swap_fee);
            self.rebalance_from_idle(pool);
        }
    }

    // ── rebalanceDebt for DLV — delegates to shared rebalance_debt ──

    pub fn rebalance_debt_dlv(&mut self, pool: &mut CorePool, target_cr_wad: U256, swap_fee: f64) {
        let w = wad();
        if target_cr_wad <= w { return; }
        if self.virtual_debt.is_zero() { return; }
        self.rebalance_debt(pool, target_cr_wad, swap_fee);
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

        static RD_LOG_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let log_idx = RD_LOG_COUNT.load(std::sync::atomic::Ordering::Relaxed);

        // Case 1: CR > target AND V > S → mint stable debt
        if cr_wad > tc && v > s {
            let alm_correction = v - s;
            let gav_w = gav * w;
            let tc_d = tc * d;
            if gav_w > tc_d {
                let debt_correction = (gav_w - tc_d) / denominator;
                let amount = alm_correction.min(debt_correction);
                if !amount.is_zero() {
                    if log_idx < 30 {
                        RD_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        eprintln!("[RD-DIAG] MINT amt={} | S={} V={} CR={} idle0={} idle1={} debt={} almCorr={} debtCorr={}",
                            amount.to_dec_string(), s.to_dec_string(), v.to_dec_string(),
                            cr_wad.to_dec_string(), self.idle0.to_dec_string(), self.idle1.to_dec_string(),
                            d.to_dec_string(), alm_correction.to_dec_string(), debt_correction.to_dec_string());
                    }
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
                    if log_idx < 30 {
                        RD_LOG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        eprintln!("[RD-DIAG] BURN amt={} | S={} V={} CR={} idle0={} idle1={} debt={} almCorr={} debtCorr={} stableIdle={}",
                            amount.to_dec_string(), s.to_dec_string(), v.to_dec_string(),
                            cr_wad.to_dec_string(), self.idle0.to_dec_string(), self.idle1.to_dec_string(),
                            d.to_dec_string(), alm_correction.to_dec_string(), debt_correction.to_dec_string(),
                            stable_idle.to_dec_string());
                    }
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
        let tick_floor = tick_current.div_euclid(ts) * ts;

        let tick_ceil = tick_floor + ts;
        let base_lower = (tick_floor - self.params.base_threshold).max(MIN_TICK);
        let base_upper = (tick_ceil + self.params.base_threshold).min(MAX_TICK);

        let sqrt_price = pool.sqrt_price_x96();

        if base_lower < base_upper && (!self.idle0.is_zero() || !self.idle1.is_zero()) {
            let liq_raw = max_liquidity_for_amounts(sqrt_price, base_lower, base_upper, self.idle0, self.idle1);
            let liq = cap_liquidity(sqrt_price, base_lower, base_upper, liq_raw, self.idle0, self.idle1);

            if !liq.is_zero() {
                let (a0, a1) = pool.mint(MANAGER, base_lower, base_upper, I256(liq));
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
            let wide_upper = (tick_ceil + self.params.wide_threshold).min(MAX_TICK);

            if wide_lower >= wide_upper { return; }

            let liq_raw = max_liquidity_for_amounts(sqrt_price, wide_lower, wide_upper, self.idle0, self.idle1);
            let liq = cap_liquidity(sqrt_price, wide_lower, wide_upper, liq_raw, self.idle0, self.idle1);

            if !liq.is_zero() {
                let (a0, a1) = pool.mint(MANAGER, wide_lower, wide_upper, I256(liq));
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

        // Limit position: deploy remaining idle into bid or ask side
        if !self.idle0.is_zero() || !self.idle1.is_zero() {
            let bid_lower = tick_floor - self.params.limit_threshold;
            let bid_upper = tick_floor;
            let ask_lower = tick_ceil;
            let ask_upper = tick_ceil + self.params.limit_threshold;

            let bid_liq_raw = if bid_lower < bid_upper {
                max_liquidity_for_amounts(sqrt_price, bid_lower, bid_upper, self.idle0, self.idle1)
            } else { U256::ZERO };

            let ask_liq_raw = if ask_lower < ask_upper {
                max_liquidity_for_amounts(sqrt_price, ask_lower, ask_upper, self.idle0, self.idle1)
            } else { U256::ZERO };

            let (limit_lower, limit_upper, limit_liq_raw) = if bid_liq_raw > ask_liq_raw {
                (bid_lower, bid_upper, bid_liq_raw)
            } else {
                (ask_lower, ask_upper, ask_liq_raw)
            };

            let limit_liq = cap_liquidity(sqrt_price, limit_lower, limit_upper, limit_liq_raw, self.idle0, self.idle1);

            if !limit_liq.is_zero() {
                let (a0, a1) = pool.mint(MANAGER, limit_lower, limit_upper, I256(limit_liq));
                let used0 = a0.abs();
                let used1 = a1.abs();
                self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
                self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
                self.limit = Some(VaultPosition {
                    tick_lower: limit_lower,
                    tick_upper: limit_upper,
                    liquidity: limit_liq,
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

        if let Some(ref mut pos) = self.limit {
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

        let vol_price_wad = Self::volatile_price_wad(sqrt, self.pool_config.is_volatile_token0());

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

        let stable_to_swap = full_math::mul_div_rounding_up(diff, fee_den, denom);
        if stable_to_swap.is_zero() { return; }

        eprintln!("[SWAP-DBG] volIdle={} stableIdle={} volValueStable={} stableVal={} isStableHeavy={} diff={} stableToSwap={} poolPrice={} volPriceWad={}",
            volatile_idle.to_dec_string(), stable_idle.to_dec_string(),
            volatile_value_stable.to_dec_string(), stable_value.to_dec_string(),
            is_stable_heavy, diff.to_dec_string(), stable_to_swap.to_dec_string(),
            price_wad.to_dec_string(), vol_price_wad.to_dec_string());

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
            let volatile_to_send = if self.pool_config.is_volatile_token0() {
                full_math::mul_div(stable_to_swap, w, price_wad)
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
            eprintln!("[SWAP-DBG] SELL-VOL: volToSend={} capped={} sentValueStable={} effectiveValue={} wasCapped={}",
                volatile_to_send.to_dec_string(), capped.to_dec_string(),
                sent_value_stable.to_dec_string(), effective_value.to_dec_string(),
                volatile_to_send > volatile_idle);
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
                    let (burned0, burned1) = pool.burn(MANAGER, p.tick_lower, p.tick_upper, I256(p.liquidity));
                    let (c0, c1) = pool.collect(
                        MANAGER, p.tick_lower, p.tick_upper, MAX_UINT128, MAX_UINT128,
                    );
                    self.idle0 = self.idle0 + c0;
                    self.idle1 = self.idle1 + c1;
                    self.compensate_burn_rounding(pool.tick_current(), p.tick_lower, p.tick_upper, burned0, burned1);
                }
            }
        }
    }

    fn compensate_burn_rounding(&mut self, tick: i32, lo: i32, hi: i32, burned0: I256, burned1: I256) {
        if burned0 == I256::ZERO && burned1 == I256::ZERO { return; }
        let one = U256::ONE;
        if tick < lo {
            if burned0 > I256::ZERO { self.idle0 = self.idle0 + one; }
        } else if tick >= hi {
            if burned1 > I256::ZERO { self.idle1 = self.idle1 + one; }
        } else {
            if burned0 > I256::ZERO { self.idle0 = self.idle0 + one; }
            if burned1 > I256::ZERO { self.idle1 = self.idle1 + one; }
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

    fn position_fees(&self, pool: &CorePool, vault_pos: &VaultPosition) -> (U256, U256) {
        let pool_pos = pool.position_manager().get_position_readonly(
            MANAGER, vault_pos.tick_lower, vault_pos.tick_upper,
        );
        if pool_pos.liquidity.is_zero() {
            return (pool_pos.tokens_owed_0, pool_pos.tokens_owed_1);
        }
        let (fg0, fg1) = pool.tick_manager().get_fee_growth_inside_readonly(
            vault_pos.tick_lower,
            vault_pos.tick_upper,
            pool.tick_current(),
            pool.fee_growth_global_0_x128(),
            pool.fee_growth_global_1_x128(),
        );
        let delta0 = full_math::mod256_sub(fg0, pool_pos.fee_growth_inside_0_last_x128);
        let delta1 = full_math::mod256_sub(fg1, pool_pos.fee_growth_inside_1_last_x128);
        let est0 = full_math::mul_div(delta0, pool_pos.liquidity, Q128);
        let est1 = full_math::mul_div(delta1, pool_pos.liquidity, Q128);
        (pool_pos.tokens_owed_0 + est0, pool_pos.tokens_owed_1 + est1)
    }

    fn all_fees(&self, pool: &CorePool) -> (U256, U256) {
        let mut f0 = U256::ZERO;
        let mut f1 = U256::ZERO;
        for pos in [&self.wide, &self.base, &self.limit] {
            if let Some(p) = pos {
                let (pf0, pf1) = self.position_fees(pool, p);
                f0 = f0 + pf0;
                f1 = f1 + pf1;
            }
        }
        (f0, f1)
    }

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
        let (fees0, fees1) = self.all_fees(pool);
        (lp0 + fees0 + self.idle0, lp1 + fees1 + self.idle1)
    }

    pub fn total_amounts_round_up(&self, pool: &CorePool) -> (U256, U256) {
        let sqrt_price = pool.sqrt_price_x96();
        let mut total0 = U256::ZERO;
        let mut total1 = U256::ZERO;
        for pos in [&self.wide, &self.base, &self.limit] {
            if let Some(p) = pos {
                if !p.liquidity.is_zero() {
                    let (a0, a1) = amounts_for_liquidity_round(
                        sqrt_price, p.tick_lower, p.tick_upper, p.liquidity, true,
                    );
                    total0 = total0 + a0;
                    total1 = total1 + a1;
                }
            }
        }
        let (fees0, fees1) = self.all_fees(pool);
        (total0 + fees0 + self.idle0, total1 + fees1 + self.idle1)
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
    /// Uses roundUp totals to match TS shouldForceActiveRebalance(getTotalAmounts(true))
    pub fn share_deviation_bps(&self, pool: &CorePool, override_sqrt: Option<U256>) -> u64 {
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        let w = wad();
        let (total0, total1) = self.total_amounts_round_up(pool);
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
        let half = total_value >> 1;
        let stable_share_bps = (stable_amt * bps_scale + half) / total_value;
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
