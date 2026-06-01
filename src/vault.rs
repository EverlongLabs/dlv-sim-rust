use v3_pool::core_pool::CorePool;
use v3_pool::tick_math;
use v3_pool::sqrt_price_math;
use v3_pool::full_math;
use v3_pool::types::*;

use crate::config::{VaultParams, LevAmmConfig};
use crate::pool_config::PoolConfig;

const MANAGER: &str = "0xVAULT_MANAGER";
static RBD_DIAG_GLOBAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[derive(Debug)]
struct RebalanceResult {
    mode: u8,
    amount: U256,
    swap_fee: U256,
    shares_to_burn: U256,
    withdraw_stable: U256,
    withdraw_volatile: U256,
}

impl RebalanceResult {
    fn noop() -> Self {
        Self { mode: 0, amount: U256::ZERO, swap_fee: U256::ZERO, shares_to_burn: U256::ZERO, withdraw_stable: U256::ZERO, withdraw_volatile: U256::ZERO }
    }
}

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
    let intermediate = full_math::mul_div(sa, sb, Q96);
    full_math::mul_div(amount, intermediate, diff)
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

    // LevAMM state
    pub lev_amm_notional: U256,
    pub lev_amm_collateral: U256,
    pub lev_amm_insolvent: bool,
    pub lev_amm_fee_revenue: U256,
    pending_lev_amm_mode: Option<LevAmmOpMode>,
    pending_lev_amm_trade: U256,
    pending_lev_amm_is_init: bool,
    pending_lev_amm_mint_amount0: U256,
    pending_lev_amm_mint_amount1: U256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LevAmmOpMode {
    Mint,
    Burn,
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
            lev_amm_notional: U256::ZERO,
            lev_amm_collateral: U256::ZERO,
            lev_amm_insolvent: false,
            lev_amm_fee_revenue: U256::ZERO,
            pending_lev_amm_mode: None,
            pending_lev_amm_trade: U256::ZERO,
            pending_lev_amm_is_init: false,
            pending_lev_amm_mint_amount0: U256::ZERO,
            pending_lev_amm_mint_amount1: U256::ZERO,
        }
    }

    pub fn base_range_ticks(&self) -> (i32, i32) {
        self.base.as_ref().map(|p| (p.tick_lower, p.tick_upper)).unwrap_or((0, 0))
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
        let mut rem0 = amount0;
        let mut rem1 = amount1;

        for &(tick_lower, tick_upper, pos_liq, idx) in &pos_info {
            let liq_to_mint = full_math::mul_div(pos_liq, shares, ts);

            let liq_from_amts = max_liquidity_for_amounts(
                sqrt_price, tick_lower, tick_upper, rem0, rem1,
            );
            let liq = liq_to_mint.min(liq_from_amts);
            if liq.is_zero() { continue; }

            let (a0, a1) = pool.mint(MANAGER, tick_lower, tick_upper, I256(liq));
            let used0 = a0.abs();
            let used1 = a1.abs();

            rem0 = if rem0 > used0 { rem0 - used0 } else { U256::ZERO };
            rem1 = if rem1 > used1 { rem1 - used1 } else { U256::ZERO };
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
    ) -> RebalanceResult {
        let w = wad();
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        if price_wad.is_zero() { return RebalanceResult::noop(); }

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

        static DLV_DIAG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dlv_log = DLV_DIAG.load(std::sync::atomic::Ordering::Relaxed) < 5;

        if dlv_log {
            eprintln!("[DLV-RBA] INPUTS total0={} total1={} total0_ru={} total1_ru={}",
                total0.to_dec_string(), total1.to_dec_string(),
                total0_ru.to_dec_string(), total1_ru.to_dec_string());
            eprintln!("[DLV-RBA] volatile0={} stable0={} D0={} priceWad={}",
                volatile0.to_dec_string(), stable0.to_dec_string(),
                d0.to_dec_string(), price_wad.to_dec_string());
            eprintln!("[DLV-RBA] volatileValStable={} V0={} targetV0={} Rw={} idle0={} idle1={}",
                volatile_val_stable.to_dec_string(), v0.to_dec_string(),
                target_v0.to_dec_string(), rw.to_dec_string(),
                self.idle0.to_dec_string(), self.idle1.to_dec_string());
        }

        if v0 > target_v0 {
            // ===== LEVERAGE =====
            let term_r_1_minus_f = full_math::mul_div(rw, one_minus_fee, fee_den);
            let denom_wad = w + term_r_1_minus_f;
            let denom_plus_fee_wad = denom_wad + f_wad;
            let surplus = v0 - target_v0;

            let b = full_math::mul_div(surplus, denom_wad, denom_plus_fee_wad);
            if b.is_zero() { return RebalanceResult::noop(); }

            let mut x = full_math::mul_div(b, w, denom_wad);
            let mut swap_fee_usdc = full_math::mul_div(x, fee_num, fee_den);
            let x_eff = if x > swap_fee_usdc { x - swap_fee_usdc } else { U256::ZERO };
            let mut volatile_received = self.stable_to_volatile_val(x_eff, price_wad);
            let mut stable_deposit_plan = if b > x { b - x } else { U256::ZERO };

            if dlv_log {
                eprintln!("[DLV-RBA] LEVERAGE surplus={} B={} X={} swapFee={} xEff={} volReceived={} stableDepositPlan={}",
                    surplus.to_dec_string(), b.to_dec_string(), x.to_dec_string(),
                    swap_fee_usdc.to_dec_string(), x_eff.to_dec_string(),
                    volatile_received.to_dec_string(), stable_deposit_plan.to_dec_string());
            }

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

            if dlv_log {
                eprintln!("[DLV-RBA] BSEARCH borrowTarget={} bestBorrow={} bestDiff={} reqStable={} stableDepPlan={}",
                    borrow_target.to_dec_string(), best_borrow.to_dec_string(),
                    best_diff.to_dec_string(), required_stable.to_dec_string(),
                    stable_deposit_plan.to_dec_string());
                DLV_DIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }

            if best_borrow.is_zero() { return RebalanceResult::noop(); }

            // Compute swap fee for the best borrow (matches TS computeScaledPlan)
            let st_scaled_final = if required_stable.is_zero() {
                U256::ZERO
            } else {
                let s = full_math::mul_div_rounding_up(stable_deposit_plan, best_borrow, required_stable);
                if s > best_borrow { best_borrow } else { s }
            };
            let swap_scaled_final = if best_borrow > st_scaled_final { best_borrow - st_scaled_final } else { U256::ZERO };
            let swap_fee_final = full_math::mul_div(swap_scaled_final, fee_num, fee_den);

            RebalanceResult { mode: 1, amount: best_borrow, swap_fee: swap_fee_final, shares_to_burn: U256::ZERO, withdraw_stable: U256::ZERO, withdraw_volatile: U256::ZERO }
        } else if target_v0 > v0 {
            // ===== DELEVERAGE =====
            let two_f_wad = f_wad * U256::from_u128(2);
            let denom_del_term = full_math::mul_div(two_f_wad, w, r_plus_1_wad);
            if denom_del_term >= w {
                if dlv_log {
                    eprintln!("[DLV-RBA] DELEV-FALLBACK denomDelTerm={} >= WAD, repay={}",
                        denom_del_term.to_dec_string(), v0.min(d0).to_dec_string());
                    DLV_DIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let repay = v0.min(d0);
                return RebalanceResult { mode: 2, amount: repay, swap_fee: U256::ZERO, shares_to_burn: self.total_supply, withdraw_stable: stable0, withdraw_volatile: volatile0 };
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

            let volatile_out = self.stable_to_volatile_val(volatile_val_out, price_wad);
            let shares_raw = if v0.is_zero() { U256::ZERO } else { full_math::mul_div(self.total_supply, w_cap, v0) };
            let shares_to_burn = shares_raw.min(self.total_supply);

            if dlv_log {
                eprintln!("[DLV-RBA] DELEV deficit={} denomDelWad={} W={} Wcap={} stableOut={} volValOut={} swapFee={} stableFromVol={} repay={} sharesToBurn={} volOut={}",
                    deficit.to_dec_string(), denom_del_wad.to_dec_string(),
                    w_val.to_dec_string(), w_cap.to_dec_string(),
                    stable_out.to_dec_string(), volatile_val_out.to_dec_string(),
                    swap_fee_del.to_dec_string(), stable_from_volatile.to_dec_string(),
                    repay.to_dec_string(), shares_to_burn.to_dec_string(),
                    volatile_out.to_dec_string());
                DLV_DIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }

            RebalanceResult { mode: 2, amount: repay, swap_fee: swap_fee_del, shares_to_burn, withdraw_stable: stable_out, withdraw_volatile: volatile_out }
        } else {
            RebalanceResult::noop()
        }
    }

    // ── rebalanceDebt (TS: rebalanceDebt) — shared by initial setup and DLV ──

    pub fn rebalance_debt(&mut self, pool: &mut CorePool, target_cr_wad: U256, swap_fee: f64) {
        let w = wad();
        if target_cr_wad <= w { return; }

        let result = self.rebalance_borrowed_amount(pool, None, target_cr_wad, swap_fee);
        if result.mode == 0 || result.amount.is_zero() { return; }

        let rbd_log_idx = RBD_DIAG_GLOBAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        if result.mode == 1 {
            // Leverage: match TS rebalanceDebt pro-rata deposit logic
            let sqrt = pool.sqrt_price_x96();
            let price_wad = Self::pool_price(sqrt);

            let net_new_stable = if result.amount > result.swap_fee { result.amount - result.swap_fee } else { U256::ZERO };

            let (cur0, cur1) = self.total_amounts(pool);
            let (cur_volatile, cur_stable) = if self.pool_config.is_volatile_token0() {
                (cur0, cur1)
            } else {
                (cur1, cur0)
            };
            let cur_volatile_value = self.volatile_to_stable_val(cur_volatile, price_wad);
            let cur_total = cur_stable + cur_volatile_value;

            let (deposit_volatile, deposit_stable) = if cur_total.is_zero() {
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

            if rbd_log_idx >= 19 && rbd_log_idx <= 21 {
                eprintln!("[RBD-LEV] #{} mode=lev borrow={} swapFee={} netNew={} depVol={} depStable={} a0d={} a1d={} idle0={} idle1={}",
                    rbd_log_idx, result.amount.to_dec_string(), result.swap_fee.to_dec_string(),
                    net_new_stable.to_dec_string(), deposit_volatile.to_dec_string(), deposit_stable.to_dec_string(),
                    amount0_desired.to_dec_string(), amount1_desired.to_dec_string(),
                    self.idle0.to_dec_string(), self.idle1.to_dec_string());
            }

            self.virtual_debt = self.virtual_debt + result.amount;

            let (actual0, actual1) = self.deposit_pro_rata(pool, amount0_desired, amount1_desired);

            let excess0 = if amount0_desired > actual0 { amount0_desired - actual0 } else { U256::ZERO };
            let excess1 = if amount1_desired > actual1 { amount1_desired - actual1 } else { U256::ZERO };
            if !excess0.is_zero() { self.idle0 = self.idle0 + excess0; }
            if !excess1.is_zero() { self.idle1 = self.idle1 + excess1; }

            if rbd_log_idx >= 19 && rbd_log_idx <= 21 {
                eprintln!("[RBD-LEV] #{} AFTER actual0={} actual1={} excess0={} excess1={} idle0={} idle1={}",
                    rbd_log_idx, actual0.to_dec_string(), actual1.to_dec_string(),
                    excess0.to_dec_string(), excess1.to_dec_string(),
                    self.idle0.to_dec_string(), self.idle1.to_dec_string());
            }
        } else {
            // Deleverage: asymmetric partial withdrawal (matches TS useAsymmetricDeleverage=true)
            let price_wad = Self::pool_price(pool.sqrt_price_x96());
            let is_vol_t0 = self.pool_config.is_volatile_token0();

            if !result.shares_to_burn.is_zero() && self.total_supply >= result.shares_to_burn {
                let ts_before = self.total_supply;
                let pro_rata_idle0 = full_math::mul_div(self.idle0, result.shares_to_burn, ts_before);
                let pro_rata_idle1 = full_math::mul_div(self.idle1, result.shares_to_burn, ts_before);

                self.total_supply = ts_before - result.shares_to_burn;
                self.idle0 = self.idle0 - pro_rata_idle0;
                self.idle1 = self.idle1 - pro_rata_idle1;

                let (pd_out0, pd_out1) = self.partial_deleverage(pool, result.amount, price_wad);

                self.idle0 = self.idle0 - pd_out0;
                self.idle1 = self.idle1 - pd_out1;

                let actual0 = pro_rata_idle0 + pd_out0;
                let actual1 = pro_rata_idle1 + pd_out1;

                let actual_volatile = if is_vol_t0 { actual0 } else { actual1 };
                let actual_stable = if is_vol_t0 { actual1 } else { actual0 };

                let mut actual_debt_decrease = result.amount;

                if actual_volatile > result.withdraw_volatile {
                    let extra = actual_volatile - result.withdraw_volatile;
                    let extra_stable = self.volatile_to_stable_val(extra, price_wad);
                    actual_debt_decrease = actual_debt_decrease + extra_stable;
                } else if result.withdraw_volatile > actual_volatile {
                    let missing = result.withdraw_volatile - actual_volatile;
                    let missing_stable = self.volatile_to_stable_val(missing, price_wad);
                    actual_debt_decrease = if actual_debt_decrease > missing_stable {
                        actual_debt_decrease - missing_stable
                    } else {
                        U256::ZERO
                    };
                }

                if actual_stable > result.withdraw_stable {
                    actual_debt_decrease = actual_debt_decrease + (actual_stable - result.withdraw_stable);
                } else if result.withdraw_stable > actual_stable {
                    let missing = result.withdraw_stable - actual_stable;
                    actual_debt_decrease = if actual_debt_decrease > missing {
                        actual_debt_decrease - missing
                    } else {
                        U256::ZERO
                    };
                }

                actual_debt_decrease = actual_debt_decrease.min(self.virtual_debt);
                if !actual_debt_decrease.is_zero() {
                    self.virtual_debt = self.virtual_debt - actual_debt_decrease;
                }
            }
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
        static RD_PARITY_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let log_idx = RD_LOG_COUNT.load(std::sync::atomic::Ordering::Relaxed);
        let parity_check = std::env::var("PARITY_CHECK").is_ok();

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
                    if parity_check {
                        let n = RD_PARITY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        eprintln!("[RD-PARITY] n={} mode=mint amt={} total0={} total1={} v={} s={} gav={} cr_wad={} debt_before={}",
                            n, amount.to_dec_string(), total0.to_dec_string(), total1.to_dec_string(),
                            v.to_dec_string(), s.to_dec_string(), gav.to_dec_string(),
                            cr_wad.to_dec_string(), d.to_dec_string());
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
                    if parity_check {
                        let n = RD_PARITY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        eprintln!("[RD-PARITY] n={} mode=burn amt={} total0={} total1={} v={} s={} gav={} cr_wad={} debt_before={}",
                            n, amount.to_dec_string(), total0.to_dec_string(), total1.to_dec_string(),
                            v.to_dec_string(), s.to_dec_string(), gav.to_dec_string(),
                            cr_wad.to_dec_string(), d.to_dec_string());
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

    // ── Slow Recenter ──

    pub fn slow_recenter(&mut self, pool: &mut CorePool, cfg: &crate::config::SlowRecenterConfig) -> i32 {
        let tick = pool.tick_current();
        let sqrt_price = pool.sqrt_price_x96();
        let (t0, t1) = self.total_amounts(pool);
        let price_wad = Self::pool_price(sqrt_price);
        let is_vol_t0 = self.pool_config.is_volatile_token0();
        let (volatile_amt, stable_amt) = if is_vol_t0 { (t0, t1) } else { (t1, t0) };
        let volatile_val = self.volatile_to_stable_val(volatile_amt, price_wad);

        if volatile_val.is_zero() && stable_amt.is_zero() {
            return 0;
        }

        let total_value_f = (stable_amt + volatile_val).lo as f64;
        let stable_share = stable_amt.lo as f64 / total_value_f;
        let deviation = (stable_share - 0.5).abs() * 2.0;

        let base_mid = self.base.as_ref().map(|p| (p.tick_lower + p.tick_upper) / 2).unwrap_or(tick);
        let tick_offset = (tick - base_mid).unsigned_abs() as i32;
        let tick_offset_spacings = (tick_offset as f64 / self.tick_spacing as f64).round() as i32;

        if deviation < cfg.min_deviation && tick_offset_spacings < 1 {
            return 0;
        }

        // Emergency full recenter
        if deviation >= cfg.emergency_threshold {
            self.withdraw_all(pool);
            self.rebalance_from_idle(pool);
            return tick_offset_spacings;
        }

        // Compute shift speed
        let shift_speed = if deviation < cfg.acceleration_threshold {
            let range = cfg.acceleration_threshold - cfg.min_deviation;
            let t = if range > 0.0 { (deviation - cfg.min_deviation) / range } else { 0.0 };
            (t * cfg.max_shift_per_step as f64).ceil().max(1.0) as i32
        } else {
            let range = cfg.emergency_threshold - cfg.acceleration_threshold;
            let t = if range > 0.0 { (deviation - cfg.acceleration_threshold) / range } else { 1.0 };
            (cfg.max_shift_per_step as f64 * cfg.acceleration_multiplier * (1.0 + t)).ceil() as i32
        };

        let actual_shift = tick_offset_spacings.min(shift_speed);
        if actual_shift < 1 { return 0; }

        let shift_direction = if tick > base_mid { 1 } else { -1 };
        let shift_ticks = actual_shift * self.tick_spacing * shift_direction;

        fn bound_tick(t: i32, max: i32) -> i32 { t.max(-max).min(max) }
        let max_tick = MAX_TICK;

        // Shift each range (wide=0, base=1, limit=2)
        for pos_idx in 0..3 {
            let (lo, hi, liq) = match pos_idx {
                0 => match &self.wide { Some(p) if !p.liquidity.is_zero() => (p.tick_lower, p.tick_upper, p.liquidity), _ => continue },
                1 => match &self.base { Some(p) if !p.liquidity.is_zero() => (p.tick_lower, p.tick_upper, p.liquidity), _ => continue },
                2 => match &self.limit { Some(p) if !p.liquidity.is_zero() => (p.tick_lower, p.tick_upper, p.liquidity), _ => continue },
                _ => unreachable!(),
            };

            if cfg.only_shift_oor {
                let in_range = tick >= lo && tick < hi;
                if in_range { continue; }
            }

            let new_lo = bound_tick(lo + shift_ticks, max_tick);
            let new_hi = bound_tick(hi + shift_ticks, max_tick);
            if new_lo == lo && new_hi == hi { continue; }

            let (burned0, burned1) = pool.burn(MANAGER, lo, hi, I256(liq));
            let (c0, c1) = pool.collect(MANAGER, lo, hi, MAX_UINT128, MAX_UINT128);

            let fees0 = c0 - burned0.abs();
            let fees1 = c1 - burned1.abs();
            self.accumulated_fees0 = self.accumulated_fees0 + fees0;
            self.accumulated_fees1 = self.accumulated_fees1 + fees1;
            self.idle0 = self.idle0 + c0;
            self.idle1 = self.idle1 + c1;
            self.compensate_burn_rounding(tick, lo, hi, burned0, burned1);

            match pos_idx {
                0 => if let Some(p) = &mut self.wide { p.tick_lower = new_lo; p.tick_upper = new_hi; p.liquidity = U256::ZERO; },
                1 => if let Some(p) = &mut self.base { p.tick_lower = new_lo; p.tick_upper = new_hi; p.liquidity = U256::ZERO; },
                2 => if let Some(p) = &mut self.limit { p.tick_lower = new_lo; p.tick_upper = new_hi; p.liquidity = U256::ZERO; },
                _ => {}
            }
        }

        // Redeploy limit at current tick if configured
        if cfg.redeploy_limit_at_current_tick {
            let ts = self.tick_spacing;
            let tick_floor = tick.div_euclid(ts) * ts;
            let tick_ceil = tick_floor + ts;
            let bid_lo = bound_tick(tick_floor - self.params.limit_threshold, max_tick);
            let bid_hi = bound_tick(tick_floor, max_tick);
            let ask_lo = bound_tick(tick_ceil, max_tick);
            let ask_hi = bound_tick(tick_ceil + self.params.limit_threshold, max_tick);

            let new_limit_lo;
            let new_limit_hi;
            let bid_ok = bid_hi > bid_lo;
            let ask_ok = ask_hi > ask_lo;
            if bid_ok || ask_ok {
                let bid_liq = if bid_ok {
                    max_liquidity_for_amounts(sqrt_price, bid_lo, bid_hi, self.idle0, self.idle1)
                } else { U256::ZERO };
                let ask_liq = if ask_ok {
                    max_liquidity_for_amounts(sqrt_price, ask_lo, ask_hi, self.idle0, self.idle1)
                } else { U256::ZERO };
                let pick_ask = ask_ok && (!bid_ok || ask_liq > bid_liq);
                if pick_ask {
                    new_limit_lo = ask_lo;
                    new_limit_hi = ask_hi;
                } else {
                    new_limit_lo = bid_lo;
                    new_limit_hi = bid_hi;
                }

                let cur_lo = self.limit.as_ref().map(|p| p.tick_lower).unwrap_or(0);
                let cur_hi = self.limit.as_ref().map(|p| p.tick_upper).unwrap_or(0);
                if new_limit_lo != cur_lo || new_limit_hi != cur_hi {
                    let lim_liq = self.limit.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
                    let lim_lo = self.limit.as_ref().map(|p| p.tick_lower).unwrap_or(0);
                    let lim_hi = self.limit.as_ref().map(|p| p.tick_upper).unwrap_or(0);
                    if !lim_liq.is_zero() {
                        let (b0, b1) = pool.burn(MANAGER, lim_lo, lim_hi, I256(lim_liq));
                        let (c0, c1) = pool.collect(MANAGER, lim_lo, lim_hi, MAX_UINT128, MAX_UINT128);
                        let fees0 = c0 - b0.abs();
                        let fees1 = c1 - b1.abs();
                        self.accumulated_fees0 = self.accumulated_fees0 + fees0;
                        self.accumulated_fees1 = self.accumulated_fees1 + fees1;
                        self.idle0 = self.idle0 + c0;
                        self.idle1 = self.idle1 + c1;
                        self.compensate_burn_rounding(tick, lim_lo, lim_hi, b0, b1);
                    }
                    if let Some(ref mut lim) = self.limit {
                        lim.tick_lower = new_limit_lo;
                        lim.tick_upper = new_limit_hi;
                        lim.liquidity = U256::ZERO;
                    }
                }
            }
        }

        // Redeploy idle at new (shifted) boundaries
        self.deploy_idle_to_lp(pool);
        actual_shift
    }

    fn value_in_stable(&self, amt0: U256, amt1: U256, price_wad: U256) -> U256 {
        let w = wad();
        let (volatile, stable) = if self.pool_config.is_volatile_token0() {
            (amt0, amt1)
        } else {
            (amt1, amt0)
        };
        let vol_val = self.volatile_to_stable_val(volatile, price_wad);
        stable + vol_val
    }

    fn partial_deleverage(&mut self, pool: &mut CorePool, target_stable: U256, price_wad: U256) -> (U256, U256) {
        let sqrt_price = pool.sqrt_price_x96();
        let mut remaining = target_stable;
        let mut total_out0 = U256::ZERO;
        let mut total_out1 = U256::ZERO;

        for pos_idx in [2, 1, 0] {
            if remaining.is_zero() { break; }
            let (tick_lower, tick_upper, liq) = match pos_idx {
                0 => match &self.wide { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                1 => match &self.base { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                2 => match &self.limit { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                _ => unreachable!(),
            };
            if liq.is_zero() { continue; }

            let (amt0, amt1) = amounts_for_liquidity(sqrt_price, tick_lower, tick_upper, liq);
            let range_value = self.value_in_stable(amt0, amt1, price_wad);
            if range_value.is_zero() { continue; }

            let liq_to_burn = if remaining >= range_value {
                liq
            } else {
                let l = full_math::mul_div(liq, remaining, range_value);
                if l.is_zero() { continue; }
                l
            };

            let (burned0, burned1) = pool.burn(MANAGER, tick_lower, tick_upper, I256(liq_to_burn));
            let (coll0, coll1) = pool.collect(MANAGER, tick_lower, tick_upper, MAX_UINT128, MAX_UINT128);

            self.idle0 = self.idle0 + coll0;
            self.idle1 = self.idle1 + coll1;
            self.compensate_burn_rounding(pool.tick_current(), tick_lower, tick_upper, burned0, burned1);

            total_out0 = total_out0 + coll0;
            total_out1 = total_out1 + coll1;

            let withdrawn_value = self.value_in_stable(coll0, coll1, price_wad);
            remaining = if remaining > withdrawn_value { remaining - withdrawn_value } else { U256::ZERO };

            match pos_idx {
                0 => if let Some(p) = &mut self.wide { p.liquidity = p.liquidity - liq_to_burn; },
                1 => if let Some(p) = &mut self.base { p.liquidity = p.liquidity - liq_to_burn; },
                2 => if let Some(p) = &mut self.limit { p.liquidity = p.liquidity - liq_to_burn; },
                _ => {}
            }
        }

        (total_out0, total_out1)
    }

    pub fn withdraw_shares(&mut self, pool: &mut CorePool, shares: U256) -> (U256, U256) {
        if shares.is_zero() { return (U256::ZERO, U256::ZERO); }
        let ts = self.total_supply;
        if ts.is_zero() { return (U256::ZERO, U256::ZERO); }

        self.total_supply = ts - shares;

        let mut out0 = full_math::mul_div(self.idle0, shares, ts);
        let mut out1 = full_math::mul_div(self.idle1, shares, ts);

        static WS_DIAG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let ws_log = WS_DIAG.load(std::sync::atomic::Ordering::Relaxed) < 3;
        if ws_log {
            eprintln!("[WS] shares={} ts={} idle0={} idle1={} proRataIdle0={} proRataIdle1={}",
                shares.to_dec_string(), ts.to_dec_string(),
                self.idle0.to_dec_string(), self.idle1.to_dec_string(),
                out0.to_dec_string(), out1.to_dec_string());
        }

        for pos_idx in 0..3 {
            let (tick_lower, tick_upper, liq) = match pos_idx {
                0 => match &self.wide { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                1 => match &self.base { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                2 => match &self.limit { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                _ => unreachable!(),
            };
            if liq.is_zero() { continue; }
            let liq_share = full_math::mul_div(liq, shares, ts);
            if liq_share.is_zero() { continue; }

            let (burned0, burned1) = pool.burn(MANAGER, tick_lower, tick_upper, I256(liq_share));
            let (coll0, coll1) = pool.collect(MANAGER, tick_lower, tick_upper, MAX_UINT128, MAX_UINT128);

            let b0 = if burned0 > I256::ZERO { burned0.0 } else { U256::ZERO };
            let b1 = if burned1 > I256::ZERO { burned1.0 } else { U256::ZERO };
            let fees0 = if coll0 > b0 { coll0 - b0 } else { U256::ZERO };
            let fees1 = if coll1 > b1 { coll1 - b1 } else { U256::ZERO };

            if ws_log {
                eprintln!("[WS] pos#{} [{},{}] liq={} liqShare={} burned=({},{}) coll=({},{}) fees=({},{})",
                    pos_idx, tick_lower, tick_upper, liq.to_dec_string(), liq_share.to_dec_string(),
                    b0.to_dec_string(), b1.to_dec_string(),
                    coll0.to_dec_string(), coll1.to_dec_string(),
                    fees0.to_dec_string(), fees1.to_dec_string());
            }

            self.idle0 = self.idle0 + coll0;
            self.idle1 = self.idle1 + coll1;
            self.compensate_burn_rounding(pool.tick_current(), tick_lower, tick_upper, burned0, burned1);

            let v0 = full_math::mul_div(fees0, shares, ts);
            let v1 = full_math::mul_div(fees1, shares, ts);

            out0 = out0 + b0 + v0;
            out1 = out1 + b1 + v1;

            match pos_idx {
                0 => if let Some(p) = &mut self.wide { p.liquidity = p.liquidity - liq_share; },
                1 => if let Some(p) = &mut self.base { p.liquidity = p.liquidity - liq_share; },
                2 => if let Some(p) = &mut self.limit { p.liquidity = p.liquidity - liq_share; },
                _ => {}
            }
        }

        if ws_log {
            eprintln!("[WS] TOTAL out0={} out1={}", out0.to_dec_string(), out1.to_dec_string());
            WS_DIAG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        self.idle0 = self.idle0 - out0;
        self.idle1 = self.idle1 - out1;

        (out0, out1)
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

    // ── Dynamic thresholds (match TS setDynamicThresholds) ──

    pub fn set_dynamic_thresholds(&mut self, annualized_vol: f64) {
        let reference_vol = 0.30_f64;
        let reference_base: f64 = 4800.0;
        let reference_limit: f64 = 1000.0;
        let vol_ratio = annualized_vol / reference_vol;
        let clamped = vol_ratio.clamp(0.5, 3.0);
        let ts = self.tick_spacing as f64;
        let new_base = (reference_base * clamped / ts).round() * ts;
        let new_limit = (reference_limit * clamped / ts).round() * ts;
        self.params.base_threshold = (new_base as i32).max(self.tick_spacing * 2);
        self.params.limit_threshold = (new_limit as i32).max(self.tick_spacing);
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

    pub fn all_fees_pub(&self, pool: &CorePool) -> (U256, U256) { self.all_fees(pool) }
    pub fn print_position_details(&self, pool: &CorePool) {
        let sqrt_price = pool.sqrt_price_x96();
        let names = ["wide", "base", "limit"];
        let positions = [&self.wide, &self.base, &self.limit];
        for (i, pos) in positions.iter().enumerate() {
            if let Some(p) = pos {
                let pool_pos = pool.position_manager().get_position_readonly(
                    MANAGER, p.tick_lower, p.tick_upper,
                );
                let (lp0, lp1) = amounts_for_liquidity_round(
                    sqrt_price, p.tick_lower, p.tick_upper, p.liquidity, true,
                );
                let (pf0, pf1) = self.position_fees(pool, p);
                eprintln!("[DEV-POS] {} [{},{}] liq={} lp0={} lp1={} owed0={} owed1={} fees0={} fees1={}",
                    names[i], p.tick_lower, p.tick_upper,
                    p.liquidity.to_dec_string(),
                    lp0.to_dec_string(), lp1.to_dec_string(),
                    pool_pos.tokens_owed_0.to_dec_string(), pool_pos.tokens_owed_1.to_dec_string(),
                    pf0.to_dec_string(), pf1.to_dec_string());
            }
        }
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

    pub fn wide_info(&self) -> (i32, i32, U256) {
        self.wide.as_ref().map(|p| (p.tick_lower, p.tick_upper, p.liquidity)).unwrap_or((0, 0, U256::ZERO))
    }
    pub fn base_info(&self) -> (i32, i32, U256) {
        self.base.as_ref().map(|p| (p.tick_lower, p.tick_upper, p.liquidity)).unwrap_or((0, 0, U256::ZERO))
    }
    pub fn limit_info(&self) -> (i32, i32, U256) {
        self.limit.as_ref().map(|p| (p.tick_lower, p.tick_upper, p.liquidity)).unwrap_or((0, 0, U256::ZERO))
    }

    pub fn total_amounts(&self, pool: &CorePool) -> (U256, U256) {
        let (lp0, lp1) = self.lp_amounts(pool);
        let (fees0, fees1) = self.all_fees(pool);
        (lp0 + fees0 + self.idle0, lp1 + fees1 + self.idle1)
    }

    /// Like total_amounts but values the LP positions at an override sqrt price.
    /// Fees are still read against the real pool tick (matching TS
    /// getTotalAmounts(false, overrideSqrt), where overrideSqrt only changes
    /// the price used by amountsForLiquidityGivenPrice).
    pub fn total_amounts_at(&self, pool: &CorePool, override_sqrt: Option<U256>) -> (U256, U256) {
        let sqrt_price = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let mut t0 = U256::ZERO;
        let mut t1 = U256::ZERO;
        for pos in [&self.wide, &self.base, &self.limit] {
            if let Some(p) = pos {
                if !p.liquidity.is_zero() {
                    let (a0, a1) = amounts_for_liquidity(sqrt_price, p.tick_lower, p.tick_upper, p.liquidity);
                    t0 = t0 + a0;
                    t1 = t1 + a1;
                }
            }
        }
        let (f0, f1) = self.all_fees(pool);
        (t0 + f0 + self.idle0, t1 + f1 + self.idle1)
    }

    /// Per-range token amounts (wide, base, limit) at the given (or current)
    /// price — positions only, no fees. Mirrors TS getPerRangeAmounts(false, …).
    pub fn per_range_amounts(
        &self,
        pool: &CorePool,
        override_sqrt: Option<U256>,
    ) -> [(U256, U256); 3] {
        let sqrt_price = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let mut out = [(U256::ZERO, U256::ZERO); 3];
        for (i, pos) in [&self.wide, &self.base, &self.limit].iter().enumerate() {
            if let Some(p) = pos {
                if !p.liquidity.is_zero() {
                    out[i] = amounts_for_liquidity(sqrt_price, p.tick_lower, p.tick_upper, p.liquidity);
                }
            }
        }
        out
    }

    pub fn lp_amounts_round_up(&self, pool: &CorePool) -> (U256, U256) {
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
        (total0, total1)
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

        let (total0, total1) = self.total_amounts_round_up(pool);

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

    /// currentPPS, fundamentalPPS, equilibriumPriceWad — ports TS
    /// computeFundamentalPPSFrom / _computeFundamentalPPSCore (managerFee = 0).
    /// `current_nav` is the precomputed NAV (GAV − virtualDebt).
    pub fn compute_fundamental_pps(
        &self,
        pool: &CorePool,
        current_nav: U256,
        override_sqrt: Option<U256>,
    ) -> (U256, U256, U256) {
        let w = wad();
        let ts = self.total_supply;
        if ts.is_zero() {
            return (U256::ZERO, U256::ZERO, U256::ZERO);
        }
        let current_price_wad = Self::pool_price(override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96()));
        let current_pps = full_math::mul_div(current_nav, w, ts);

        let is_vol_t0 = self.pool_config.is_volatile_token0();
        let vol_to_stable = |amt: U256, price: U256| -> U256 {
            if is_vol_t0 {
                full_math::mul_div(amt, price, w)
            } else if price.is_zero() {
                U256::ZERO
            } else {
                full_math::mul_div(amt, w, price)
            }
        };

        // Gather position data (liquidity + fees owed, managerFee = 0 → fees = tokensOwed).
        let mut positions: Vec<(i32, i32, U256, U256, U256)> = Vec::with_capacity(3);
        let mut has_active = false;
        for pos in [&self.wide, &self.base, &self.limit] {
            if let Some(p) = pos {
                let pp = pool.position_manager().get_position_readonly(MANAGER, p.tick_lower, p.tick_upper);
                if !pp.liquidity.is_zero() {
                    has_active = true;
                }
                positions.push((p.tick_lower, p.tick_upper, pp.liquidity, pp.tokens_owed_0, pp.tokens_owed_1));
            }
        }
        if !has_active {
            return (current_pps, current_pps, current_price_wad);
        }

        let idle0 = self.idle0;
        let idle1 = self.idle1;
        let compute_totals_at = |sqrt: U256| -> (U256, U256) {
            let mut t0 = idle0;
            let mut t1 = idle1;
            for &(lo, hi, liq, f0, f1) in &positions {
                if liq.is_zero() && f0.is_zero() && f1.is_zero() {
                    continue;
                }
                let (a0, a1) = amounts_for_liquidity(sqrt, lo, hi, liq);
                t0 = t0 + a0 + f0;
                t1 = t1 + a1 + f1;
            }
            (t0, t1)
        };
        let volatile_share = |sqrt: U256| -> U256 {
            let (t0, t1) = compute_totals_at(sqrt);
            let price = Self::pool_price(sqrt);
            let (vol_amt, stable_amt) = if is_vol_t0 { (t0, t1) } else { (t1, t0) };
            let vol_val = vol_to_stable(vol_amt, price);
            let total_val = stable_amt + vol_val;
            if total_val.is_zero() {
                return U256::ZERO;
            }
            full_math::mul_div(vol_val, w, total_val)
        };

        let min_pos_lower = positions.iter().map(|p| p.0).min().unwrap();
        let max_pos_upper = positions.iter().map(|p| p.1).max().unwrap();
        let min_tick = std::cmp::max(-MAX_TICK, min_pos_lower - 5000);
        let max_tick = std::cmp::min(MAX_TICK, max_pos_upper + 5000);
        let share_increases_with_tick = !is_vol_t0;

        let target = w / U256::from_u128(2);
        let tolerance = w / U256::from_u128(1000);

        let bisect = |lo: i32, hi: i32, init_sqrt: U256, init_diff: U256| -> (U256, U256) {
            let mut b_sqrt = init_sqrt;
            let mut b_diff = init_diff;
            let mut t_low = lo;
            let mut t_high = hi;
            for _ in 0..30 {
                let mid_tick = (t_low + t_high) / 2; // floor (TS Math.floor on int avg)
                if mid_tick <= t_low || mid_tick >= t_high {
                    break;
                }
                let mid = tick_math::get_sqrt_ratio_at_tick(mid_tick);
                let ratio = volatile_share(mid);
                let diff = if ratio > target { ratio - target } else { target - ratio };
                if diff < b_diff {
                    b_diff = diff;
                    b_sqrt = mid;
                }
                if diff < tolerance {
                    break;
                }
                let needs_higher = if share_increases_with_tick { ratio < target } else { ratio > target };
                if needs_higher {
                    t_low = mid_tick;
                } else {
                    t_high = mid_tick;
                }
            }
            (b_sqrt, b_diff)
        };

        let current_sqrt = pool.sqrt_price_x96();
        let current_tick = tick_math::get_tick_at_sqrt_ratio(current_sqrt);
        let current_ratio = volatile_share(current_sqrt);
        let current_diff = if current_ratio > target { current_ratio - target } else { target - current_ratio };

        let (lo_sqrt, lo_diff) = bisect(min_tick, current_tick, current_sqrt, current_diff);
        let (hi_sqrt, hi_diff) = bisect(current_tick, max_tick, current_sqrt, current_diff);
        let best_sqrt = if lo_diff <= hi_diff { lo_sqrt } else { hi_sqrt };

        let eq_price_wad = Self::pool_price(best_sqrt);
        let (eq_t0, eq_t1) = compute_totals_at(best_sqrt);
        let (eq_vol, eq_stable) = if is_vol_t0 { (eq_t0, eq_t1) } else { (eq_t1, eq_t0) };
        let eq_vol_in_stable = vol_to_stable(eq_vol, eq_price_wad);
        let eq_gav = eq_stable + eq_vol_in_stable;
        let eq_nav = if eq_gav > self.virtual_debt { eq_gav - self.virtual_debt } else { U256::ZERO };
        let fundamental_pps = full_math::mul_div(eq_nav, w, ts);

        (current_pps, fundamental_pps, eq_price_wad)
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
        let result = if stable_share_bps > half_bps {
            (stable_share_bps - half_bps).lo as u64
        } else {
            (half_bps - stable_share_bps).lo as u64
        };
        if result == 100 {
            static DEV_EDGE_CTR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let idx = DEV_EDGE_CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if idx < 5 {
                eprintln!("[DEV-EDGE] dev={} stableAmt={} volAmt={} volValStable={} totalVal={} stableShareBps={} priceWad={} sqrt={} idle0={} idle1={}",
                    result, stable_amt.to_dec_string(), volatile_amt.to_dec_string(),
                    volatile_value_stable.to_dec_string(), total_value.to_dec_string(),
                    stable_share_bps.to_dec_string(), price_wad.to_dec_string(),
                    sqrt.to_dec_string(), self.idle0.to_dec_string(), self.idle1.to_dec_string());
            }
        }
        result
    }

    pub fn collateral_ratio_pct(&self, pool: &CorePool, override_sqrt: Option<U256>) -> f64 {
        if self.virtual_debt.is_zero() { return f64::INFINITY; }
        let cr_wad = self.collateral_ratio_wad(pool, override_sqrt);
        cr_wad.lo as f64 / 1e18 * 100.0
    }

    // ── LevAMM ──

    fn sqrt_bigint(value: U256) -> U256 {
        if value < U256::from_u128(2) { return value; }
        let mut current = value;
        let mut next = (current + U256::ONE) >> 1;
        while next < current {
            current = next;
            next = (next + value / next) >> 1;
        }
        current
    }

    fn lev_amm_curve_ratio_wad(tc: U256) -> U256 {
        let w = wad();
        let denom = tc + w;
        full_math::mul_div(tc * tc, w, denom * denom)
    }

    fn lev_amm_curve_x0(coll_value: U256, debt: U256, curve_ratio_wad: U256) -> U256 {
        let w = wad();
        let scaled_coll = w * coll_value;
        let disc = scaled_coll * scaled_coll - U256::from_u128(4) * curve_ratio_wad * w * coll_value * debt;
        (scaled_coll + Self::sqrt_bigint(disc)) / (U256::from_u128(2) * curve_ratio_wad)
    }

    fn lev_amm_debt_for_collateral(curve_ratio_wad: U256, x0: U256, coll_value: U256) -> U256 {
        if curve_ratio_wad.is_zero() || x0.is_zero() || coll_value.is_zero() { return U256::ZERO; }
        let w = wad();
        let offset = full_math::mul_div(curve_ratio_wad, x0 * x0, w * coll_value);
        if x0 > offset { x0 - offset } else { U256::ZERO }
    }

    fn lev_amm_collateral_for_debt(curve_ratio_wad: U256, x0: U256, debt: U256) -> U256 {
        if curve_ratio_wad.is_zero() || x0 <= debt { return U256::ZERO; }
        let w = wad();
        ceil_div(curve_ratio_wad * x0 * x0, w * (x0 - debt))
    }

    fn lev_amm_collateral_for_price(curve_ratio_wad: U256, x0: U256, price_wad: U256, round_up: bool) -> U256 {
        if curve_ratio_wad.is_zero() || x0.is_zero() || price_wad.is_zero() { return U256::ZERO; }
        let squared = full_math::mul_div(curve_ratio_wad, x0 * x0, price_wad);
        let mut root = Self::sqrt_bigint(squared);
        if round_up && root * root < squared { root = root + U256::ONE; }
        root
    }

    fn lev_amm_stable_split(lp_stable_value: U256, stable_balance: U256, total_at_oracle: U256) -> (U256, U256) {
        if lp_stable_value.is_zero() || total_at_oracle.is_zero() {
            return (U256::ZERO, U256::ZERO);
        }
        let stable_part = if stable_balance.is_zero() {
            U256::ZERO
        } else {
            full_math::mul_div(lp_stable_value, stable_balance, total_at_oracle)
        };
        let vol_part = if lp_stable_value > stable_part { lp_stable_value - stable_part } else { U256::ZERO };
        (stable_part, vol_part)
    }

    fn lev_amm_lp_acquire_cost_wad(stable_balance: U256, total_at_oracle: U256, fee_num: U256) -> U256 {
        let w = wad();
        let fee_den = U256::from_u128(1_000_000);
        if total_at_oracle.is_zero() { return U256::ZERO; }
        let stable_share_wad = if stable_balance.is_zero() { U256::ZERO } else { full_math::mul_div(stable_balance, w, total_at_oracle) };
        let volatile_share_wad = if stable_share_wad < w { w - stable_share_wad } else { U256::ZERO };
        let one_minus_fee = fee_den - fee_num;
        stable_share_wad + full_math::mul_div_rounding_up(volatile_share_wad, fee_den, one_minus_fee)
    }

    fn lev_amm_lp_unwind_proceeds_wad(stable_balance: U256, total_at_oracle: U256, fee_num: U256) -> U256 {
        let w = wad();
        let fee_den = U256::from_u128(1_000_000);
        if total_at_oracle.is_zero() { return U256::ZERO; }
        let stable_share_wad = if stable_balance.is_zero() { U256::ZERO } else { full_math::mul_div(stable_balance, w, total_at_oracle) };
        let volatile_share_wad = if stable_share_wad < w { w - stable_share_wad } else { U256::ZERO };
        let one_minus_fee = fee_den - fee_num;
        stable_share_wad + full_math::mul_div(volatile_share_wad, one_minus_fee, fee_den)
    }

    fn lev_amm_lp_acquire_cost(lp_stable_value: U256, stable_balance: U256, total_at_oracle: U256, fee_num: U256) -> U256 {
        let fee_den = U256::from_u128(1_000_000);
        let (stable_part, volatile_stable_part) = Self::lev_amm_stable_split(lp_stable_value, stable_balance, total_at_oracle);
        let one_minus_fee = fee_den - fee_num;
        stable_part + full_math::mul_div_rounding_up(volatile_stable_part, fee_den, one_minus_fee)
    }

    fn lev_amm_lp_unwind_proceeds(lp_stable_value: U256, stable_balance: U256, total_at_oracle: U256, fee_num: U256) -> U256 {
        let fee_den = U256::from_u128(1_000_000);
        let (stable_part, volatile_stable_part) = Self::lev_amm_stable_split(lp_stable_value, stable_balance, total_at_oracle);
        stable_part + full_math::mul_div(volatile_stable_part, fee_den - fee_num, fee_den)
    }

    /// Returns (fee, fired) where fired=true when the step was NOT a noop.
    /// `override_sqrt` selects the price source used for both `priceWad` and
    /// the totals fed into `lev_amm_step_sync`. TS uses Binance external sqrt
    /// when `dlvConfig.almSwapPriceSource === "binance"`; pass `None` to use
    /// the pool's current sqrt (TS "30bp" / unset).
    pub fn run_lev_amm_step(&mut self, pool: &mut CorePool, lev_amm_cfg: &LevAmmConfig, target_cr_wad: U256, override_sqrt: Option<U256>) -> (U256, bool) {
        let sqrt = override_sqrt.unwrap_or_else(|| pool.sqrt_price_x96());
        let price_wad = Self::pool_price(sqrt);
        let (t0, t1) = self.total_amounts_at(pool, override_sqrt);
        let result = self.lev_amm_step_sync(t0, t1, price_wad, lev_amm_cfg, target_cr_wad);
        let fired = self.pending_lev_amm_mode.is_some();
        self.apply_pending_lev_amm_op(pool, price_wad);
        (result.fee, fired)
    }

    fn lev_amm_step_sync(
        &mut self,
        total0: U256,
        total1: U256,
        price_wad: U256,
        lev_amm_cfg: &LevAmmConfig,
        target_cr_wad: U256,
    ) -> LevAmmResult {
        let noop = LevAmmResult { fee: U256::ZERO };
        assert!(self.pending_lev_amm_mode.is_none(), "LevAMM: prior pending op leaked");
        let w = wad();
        let fee_den = U256::from_u128(1_000_000);

        let lp_value = self.value_in_stable(total0, total1, price_wad);
        if lp_value.is_zero() { return noop; }
        if target_cr_wad <= w { return noop; }
        let tc_minus_wad = target_cr_wad - w;
        let is_vol_t0 = self.pool_config.is_volatile_token0();

        // ---- INIT ----
        if self.lev_amm_notional.is_zero() {
            let volatile_amt = if is_vol_t0 { total0 } else { total1 };
            let volatile_value_stable = self.volatile_to_stable_val(volatile_amt, price_wad);
            let initial_mint = full_math::mul_div(volatile_value_stable, tc_minus_wad, w);
            if initial_mint.is_zero() { return noop; }
            self.virtual_debt = self.virtual_debt + initial_mint;
            if is_vol_t0 {
                self.idle1 = self.idle1 + initial_mint;
            } else {
                self.idle0 = self.idle0 + initial_mint;
            }
            let post_mint_gav = lp_value + initial_mint;
            self.lev_amm_notional = post_mint_gav;
            self.lev_amm_collateral = post_mint_gav;
            self.pending_lev_amm_mode = Some(LevAmmOpMode::Mint);
            self.pending_lev_amm_trade = initial_mint;
            self.pending_lev_amm_is_init = true;
            return LevAmmResult { fee: U256::ZERO };
        }

        // ---- STEADY STATE ----
        let p_lp = full_math::mul_div(lp_value, w, self.lev_amm_notional);
        let lp_collateral = self.lev_amm_collateral;
        if lp_collateral.is_zero() || p_lp.is_zero() { return noop; }
        let coll_value = full_math::mul_div(p_lp, lp_collateral, w);
        if coll_value.is_zero() { return noop; }

        let d = self.virtual_debt;
        let v = if is_vol_t0 { total0 } else { total1 };
        let s = if is_vol_t0 { total1 } else { total0 };
        let v_stable = self.volatile_to_stable_val(v, price_wad);
        let total_at_oracle = v_stable + s;
        if total_at_oracle.is_zero() { return noop; }

        let effective_tc = target_cr_wad;
        let target_debt = full_math::mul_div(coll_value, w, effective_tc);
        let curve_ratio_wad = Self::lev_amm_curve_ratio_wad(effective_tc);
        if curve_ratio_wad.is_zero() { return noop; }

        let insolvency_threshold = full_math::mul_div(coll_value, w, U256::from_u128(4) * curve_ratio_wad);
        let min_safe_debt = target_debt / U256::from_u128(8);
        let max_safe_debt = if insolvency_threshold > coll_value / U256::from_u128(32) {
            insolvency_threshold - coll_value / U256::from_u128(32)
        } else {
            insolvency_threshold
        };

        if d > insolvency_threshold && d < target_debt {
            if !self.lev_amm_insolvent {
                self.lev_amm_insolvent = true;
                eprintln!("[LEV-AMM] INSOLVENT (mint blocked, burn allowed)");
            }
            return noop;
        }
        if self.lev_amm_insolvent && d <= insolvency_threshold {
            self.lev_amm_insolvent = false;
            eprintln!("[LEV-AMM] solvent again");
        }

        let fee_num = U256::from_u128((lev_amm_cfg.swap_fee * 1_000_000.0).max(0.0).floor() as u128);
        let one_minus_fee = fee_den - fee_num;
        if one_minus_fee.is_zero() { return noop; }

        let curve_debt_input = if d > insolvency_threshold { insolvency_threshold } else { d };
        let x0 = Self::lev_amm_curve_x0(coll_value, curve_debt_input, curve_ratio_wad);
        if x0.is_zero() { return noop; }

        let lp_acquire_cost_wad = Self::lev_amm_lp_acquire_cost_wad(s, total_at_oracle, fee_num);
        let lp_unwind_proceeds_wad = Self::lev_amm_lp_unwind_proceeds_wad(s, total_at_oracle, fee_num);
        let amm_sell_threshold_wad = full_math::mul_div_rounding_up(lp_acquire_cost_wad, fee_den, one_minus_fee);
        let amm_buy_threshold_wad = full_math::mul_div(lp_unwind_proceeds_wad, fee_den, fee_den + fee_num);

        // ---- MINT (add debt) ----
        if d < max_safe_debt {
            let mut target_coll_value = Self::lev_amm_collateral_for_price(curve_ratio_wad, x0, amm_sell_threshold_wad, true);
            let max_safe_coll_value = Self::lev_amm_collateral_for_debt(curve_ratio_wad, x0, max_safe_debt);
            if !max_safe_coll_value.is_zero() && (target_coll_value.is_zero() || target_coll_value > max_safe_coll_value) {
                target_coll_value = max_safe_coll_value;
            }
            if target_coll_value <= coll_value { return noop; }

            let desired_trade = target_coll_value - coll_value;
            let mut trade = desired_trade;
            let max_frac = lev_amm_cfg.max_arb_per_tick_frac.max(0.0).min(1.0);
            if max_frac < 1.0 {
                let max_arb = (coll_value * U256::from_u128((max_frac * 1_000_000.0) as u128)) / U256::from_u128(1_000_000);
                if trade > max_arb { trade = max_arb; }
            }
            if trade.is_zero() { return noop; }

            let final_coll_value = coll_value + trade;
            let mut debt_target = Self::lev_amm_debt_for_collateral(curve_ratio_wad, x0, final_coll_value);
            if debt_target <= d { return noop; }

            let mut principal = debt_target - d;
            // principal cap not set (env LEVAMM_PRINCIPAL_CAP_BPS defaults to 0)
            let fee = full_math::mul_div(principal, fee_num, fee_den);
            let arb_cost = Self::lev_amm_lp_acquire_cost(trade, s, total_at_oracle, fee_num);
            let arb_net = if principal > fee { principal - fee } else { U256::ZERO };
            if arb_net <= arb_cost { return noop; }

            self.virtual_debt = debt_target;
            let (stable_part, volatile_stable_part) = Self::lev_amm_stable_split(trade, s, total_at_oracle);
            let add_volatile = self.stable_to_volatile_val(volatile_stable_part, price_wad);
            if is_vol_t0 {
                self.pending_lev_amm_mint_amount0 = add_volatile;
                self.pending_lev_amm_mint_amount1 = stable_part;
            } else {
                self.pending_lev_amm_mint_amount1 = add_volatile;
                self.pending_lev_amm_mint_amount0 = stable_part;
            }
            if !fee.is_zero() {
                if is_vol_t0 { self.idle1 = self.idle1 + fee; }
                else { self.idle0 = self.idle0 + fee; }
                self.lev_amm_fee_revenue = self.lev_amm_fee_revenue + fee;
            }
            self.pending_lev_amm_mode = Some(LevAmmOpMode::Mint);
            self.pending_lev_amm_trade = trade;
            return LevAmmResult { fee };
        }

        // ---- BURN (reduce debt) ----
        if d > min_safe_debt {
            let mut target_coll_value = Self::lev_amm_collateral_for_price(curve_ratio_wad, x0, amm_buy_threshold_wad, false);
            let min_safe_coll_value = Self::lev_amm_collateral_for_debt(curve_ratio_wad, x0, min_safe_debt);
            if !min_safe_coll_value.is_zero() && (target_coll_value.is_zero() || target_coll_value < min_safe_coll_value) {
                target_coll_value = min_safe_coll_value;
            }
            if target_coll_value >= coll_value || target_coll_value.is_zero() { return noop; }

            let desired_trade = coll_value - target_coll_value;
            let mut trade = desired_trade;
            let max_frac = lev_amm_cfg.max_arb_per_tick_frac.max(0.0).min(1.0);
            if max_frac < 1.0 {
                let max_arb = (coll_value * U256::from_u128((max_frac * 1_000_000.0) as u128)) / U256::from_u128(1_000_000);
                if trade > max_arb { trade = max_arb; }
            }
            if trade.is_zero() { return noop; }

            let final_coll_value = coll_value - trade;
            let mut debt_target = Self::lev_amm_debt_for_collateral(curve_ratio_wad, x0, final_coll_value);
            if debt_target < min_safe_debt { debt_target = min_safe_debt; }
            if debt_target >= d { return noop; }

            let principal = d - debt_target;
            // burn principal cap not set (env LEVAMM_BURN_PRINCIPAL_CAP_BPS defaults to 0)
            let fee = full_math::mul_div(principal, fee_num, fee_den);
            let arb_proceeds = Self::lev_amm_lp_unwind_proceeds(trade, s, total_at_oracle, fee_num);
            let arb_cost = principal + fee;
            if arb_proceeds <= arb_cost { return noop; }

            self.virtual_debt = debt_target;
            if !fee.is_zero() {
                if is_vol_t0 { self.idle1 = self.idle1 + fee; }
                else { self.idle0 = self.idle0 + fee; }
                self.lev_amm_fee_revenue = self.lev_amm_fee_revenue + fee;
            }
            self.pending_lev_amm_mode = Some(LevAmmOpMode::Burn);
            self.pending_lev_amm_trade = trade;
            return LevAmmResult { fee };
        }

        noop
    }

    fn apply_pending_lev_amm_op(&mut self, pool: &mut CorePool, price_wad: U256) {
        let mode = self.pending_lev_amm_mode.take();
        let trade = self.pending_lev_amm_trade;
        let is_init = self.pending_lev_amm_is_init;
        let mint_amt0 = self.pending_lev_amm_mint_amount0;
        let mint_amt1 = self.pending_lev_amm_mint_amount1;
        self.pending_lev_amm_trade = U256::ZERO;
        self.pending_lev_amm_is_init = false;
        self.pending_lev_amm_mint_amount0 = U256::ZERO;
        self.pending_lev_amm_mint_amount1 = U256::ZERO;

        let mode = match mode {
            Some(m) => m,
            None => return,
        };

        if mode == LevAmmOpMode::Mint {
            if is_init {
                if !mint_amt0.is_zero() || !mint_amt1.is_zero() {
                    self.idle0 = self.idle0 + mint_amt0;
                    self.idle1 = self.idle1 + mint_amt1;
                }
                self.withdraw_all(pool);
                self.rebalance_from_idle(pool);
                return;
            }
            self.lev_amm_mint_direct_to_lp(pool, mint_amt0, mint_amt1);
            return;
        }

        // mode == Burn
        if trade.is_zero() { return; }
        let idle0_before = self.idle0;
        let idle1_before = self.idle1;
        let m_fees0_before = U256::ZERO; // no manager fees in sim
        let m_fees1_before = U256::ZERO;
        let s_fees0_before = self.accumulated_fees0;
        let s_fees1_before = self.accumulated_fees1;

        let (actual0, actual1) = self.partial_deleverage(pool, trade, price_wad);

        let reserved_fees0 = (self.accumulated_fees0 - s_fees0_before);
        let reserved_fees1 = (self.accumulated_fees1 - s_fees1_before);
        let strip0 = if actual0 > reserved_fees0 { actual0 - reserved_fees0 } else { U256::ZERO };
        let strip1 = if actual1 > reserved_fees1 { actual1 - reserved_fees1 } else { U256::ZERO };
        let idle_delta0 = if self.idle0 > idle0_before { self.idle0 - idle0_before } else { U256::ZERO };
        let idle_delta1 = if self.idle1 > idle1_before { self.idle1 - idle1_before } else { U256::ZERO };
        let drop0 = strip0.min(idle_delta0);
        let drop1 = strip1.min(idle_delta1);
        if !drop0.is_zero() { self.idle0 = self.idle0 - drop0; }
        if !drop1.is_zero() { self.idle1 = self.idle1 - drop1; }
    }

    fn lev_amm_mint_direct_to_lp(&mut self, pool: &mut CorePool, amount0: U256, amount1: U256) {
        if amount0.is_zero() && amount1.is_zero() { return; }

        // Stage into idle (match TS: amounts enter idle, then mint subtracts used)
        self.idle0 = self.idle0 + amount0;
        self.idle1 = self.idle1 + amount1;

        let sqrt_price = pool.sqrt_price_x96();
        let wide_liq = self.wide.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
        let base_liq = self.base.as_ref().map(|p| p.liquidity).unwrap_or(U256::ZERO);
        let total_liq = wide_liq + base_liq;
        if total_liq.is_zero() {
            // No in-range positions — leave in idle for next deploy cycle
            return;
        }

        let frac_den = U256::from_u128(1_000_000);

        for pos_idx in [0, 1] {
            let (tick_lower, tick_upper, existing_liq) = match pos_idx {
                0 => match &self.wide { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                1 => match &self.base { Some(p) => (p.tick_lower, p.tick_upper, p.liquidity), None => continue },
                _ => unreachable!(),
            };
            if existing_liq.is_zero() { continue; }
            let frac = full_math::mul_div(existing_liq, frac_den, total_liq);
            let frac_a0 = full_math::mul_div(amount0, frac, frac_den);
            let frac_a1 = full_math::mul_div(amount1, frac, frac_den);
            if frac_a0.is_zero() && frac_a1.is_zero() { continue; }

            let liq = max_liquidity_for_amounts(sqrt_price, tick_lower, tick_upper, frac_a0, frac_a1);
            let capped = cap_liquidity(sqrt_price, tick_lower, tick_upper, liq, self.idle0, self.idle1);
            if capped.is_zero() { continue; }

            let (a0, a1) = pool.mint(MANAGER, tick_lower, tick_upper, I256(capped));
            let used0 = a0.abs();
            let used1 = a1.abs();
            self.idle0 = if self.idle0 > used0 { self.idle0 - used0 } else { U256::ZERO };
            self.idle1 = if self.idle1 > used1 { self.idle1 - used1 } else { U256::ZERO };
            match pos_idx {
                0 => if let Some(p) = &mut self.wide { p.liquidity = p.liquidity + capped; },
                1 => if let Some(p) = &mut self.base { p.liquidity = p.liquidity + capped; },
                _ => {}
            }
        }
    }
}

struct LevAmmResult {
    fee: U256,
}
