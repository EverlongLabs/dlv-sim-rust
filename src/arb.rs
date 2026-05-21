use v3_pool::core_pool::CorePool;
use v3_pool::full_math;
use v3_pool::types::*;

const BPS: u128 = 10_000;

#[derive(Debug)]
pub struct ArbDetection {
    pub is_arbitrable: bool,
    pub deviation_bps: f64,
    pub zero_for_one: bool,
    pub pool_sqrt_price_x96: U256,
    pub target_sqrt_price_x96: U256,
}

#[derive(Debug)]
pub struct ArbResult {
    pub amount0: I256,
    pub amount1: I256,
    pub profit_stable: I256,
    pub zero_for_one: bool,
}

fn wad() -> U256 { U256::from_u128(1_000_000_000_000_000_000) }

pub fn detect_arb(
    pool_sqrt: U256,
    external_sqrt: U256,
    fee_ppm: u32,
) -> ArbDetection {
    let pool_p = pool_sqrt * pool_sqrt;
    let ext_p = external_sqrt * external_sqrt;
    let w = wad();

    let pool_wad = full_math::mul_div(pool_p, w, Q192);
    let ext_wad = full_math::mul_div(ext_p, w, Q192);

    let fee_wad = U256::from_u128(fee_ppm as u128 * 1_000_000_000_000);
    let one_minus_fee = w - fee_wad;

    if ext_wad > pool_wad {
        let diff = ext_wad - pool_wad;
        let dev_bps_u = full_math::mul_div(diff, U256::from_u128(BPS), pool_wad);
        let deviation_bps = dev_bps_u.lo as f64;

        let target_wad = full_math::mul_div(ext_wad, one_minus_fee, w);
        let is_arbitrable = pool_wad < target_wad;

        let target_sqrt = if is_arbitrable {
            sqrt_from_wad_price(target_wad)
        } else {
            pool_sqrt
        };

        ArbDetection {
            is_arbitrable,
            deviation_bps,
            zero_for_one: false,
            pool_sqrt_price_x96: pool_sqrt,
            target_sqrt_price_x96: target_sqrt,
        }
    } else {
        let diff = pool_wad - ext_wad;
        let dev_bps_u = if !pool_wad.is_zero() {
            full_math::mul_div(diff, U256::from_u128(BPS), pool_wad)
        } else {
            U256::ZERO
        };
        let deviation_bps = -(dev_bps_u.lo as f64);

        let target_wad = full_math::mul_div(ext_wad, w, one_minus_fee);
        let is_arbitrable = pool_wad > target_wad;

        let target_sqrt = if is_arbitrable {
            sqrt_from_wad_price(target_wad)
        } else {
            pool_sqrt
        };

        ArbDetection {
            is_arbitrable,
            deviation_bps,
            zero_for_one: true,
            pool_sqrt_price_x96: pool_sqrt,
            target_sqrt_price_x96: target_sqrt,
        }
    }
}

pub fn execute_arb_close_gap(
    pool: &mut CorePool,
    detection: &ArbDetection,
    is_volatile_token0: bool,
) -> ArbResult {
    if !detection.is_arbitrable {
        return ArbResult {
            amount0: I256::ZERO,
            amount1: I256::ZERO,
            profit_stable: I256::ZERO,
            zero_for_one: detection.zero_for_one,
        };
    }

    let large_amount = I256(U256::from_u128(1_000_000_000_000_000_000_000));
    let limit = Some(detection.target_sqrt_price_x96);
    let (a0, a1) = pool.swap(detection.zero_for_one, large_amount, limit);

    let profit_stable = compute_arb_profit(
        a0, a1,
        detection.target_sqrt_price_x96,
        is_volatile_token0,
    );

    ArbResult {
        amount0: a0,
        amount1: a1,
        profit_stable,
        zero_for_one: detection.zero_for_one,
    }
}

fn compute_arb_profit(
    amount0: I256,
    amount1: I256,
    external_sqrt: U256,
    is_volatile_token0: bool,
) -> I256 {
    let w = wad();
    let ext_price_x192 = external_sqrt * external_sqrt;
    let t1_per_t0_wad = full_math::mul_div(ext_price_x192, w, Q192);
    if t1_per_t0_wad.is_zero() { return I256::ZERO; }

    let abs0 = amount0.abs();
    let abs1 = amount1.abs();
    let amount0_value_in_t1 = full_math::mul_div(abs0, t1_per_t0_wad, w);
    let token0_goes_in = amount0.is_positive();

    let (profit_t1, is_neg) = if token0_goes_in {
        if abs1 >= amount0_value_in_t1 {
            (abs1 - amount0_value_in_t1, false)
        } else {
            (amount0_value_in_t1 - abs1, true)
        }
    } else {
        if amount0_value_in_t1 >= abs1 {
            (amount0_value_in_t1 - abs1, false)
        } else {
            (abs1 - amount0_value_in_t1, true)
        }
    };

    if is_volatile_token0 {
        if is_neg { I256::ZERO - I256(profit_t1) } else { I256(profit_t1) }
    } else {
        let converted = full_math::mul_div(profit_t1, w, t1_per_t0_wad);
        if is_neg { I256::ZERO - I256(converted) } else { I256(converted) }
    }
}

fn sqrt_from_wad_price(price_wad: U256) -> U256 {
    // sqrtPriceX96 = sqrt(priceWad * Q192 / WAD)
    let w = wad();
    let price_x192 = full_math::mul_div(price_wad, Q192, w);
    let sqrt = full_math::sqrt(price_x192);
    sqrt
}
