// sqrt_price_math.rs — SqrtPriceMath

use crate::types::*;
use crate::full_math;

/// Masks to 256 bits (simulates EVM uint256 overflow)
#[inline]
fn multiply_in_256(x: U256, y: U256) -> U256 {
    x.wrapping_mul(y) // wrapping_mul already gives lower 256 bits
}

#[inline]
fn add_in_256(x: U256, y: U256) -> U256 {
    let (result, _) = x.overflowing_add(y);
    result
}

/// Signed getAmount0Delta: dispatches based on sign of liquidity
pub fn get_amount0_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: I256,
) -> I256 {
    if liquidity.is_negative() {
        let abs_liq = liquidity.negate().0;
        I256(get_amount0_delta_unsigned(sqrt_ratio_a_x96, sqrt_ratio_b_x96, abs_liq, false)).negate()
    } else {
        I256(get_amount0_delta_unsigned(
            sqrt_ratio_a_x96,
            sqrt_ratio_b_x96,
            liquidity.0,
            true,
        ))
    }
}

/// Signed getAmount1Delta
pub fn get_amount1_delta(
    sqrt_ratio_a_x96: U256,
    sqrt_ratio_b_x96: U256,
    liquidity: I256,
) -> I256 {
    if liquidity.is_negative() {
        let abs_liq = liquidity.negate().0;
        I256(get_amount1_delta_unsigned(sqrt_ratio_a_x96, sqrt_ratio_b_x96, abs_liq, false)).negate()
    } else {
        I256(get_amount1_delta_unsigned(
            sqrt_ratio_a_x96,
            sqrt_ratio_b_x96,
            liquidity.0,
            true,
        ))
    }
}

pub fn get_next_sqrt_price_from_input(
    sqrt_p_x96: U256,
    liquidity: U256,
    amount_in: U256,
    zero_for_one: bool,
) -> U256 {
    assert!(sqrt_p_x96 > ZERO);
    assert!(liquidity > ZERO);

    if zero_for_one {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_p_x96, liquidity, amount_in, true)
    } else {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_p_x96, liquidity, amount_in, true)
    }
}

pub fn get_next_sqrt_price_from_output(
    sqrt_p_x96: U256,
    liquidity: U256,
    amount_out: U256,
    zero_for_one: bool,
) -> U256 {
    assert!(sqrt_p_x96 > ZERO);
    assert!(liquidity > ZERO);

    if zero_for_one {
        get_next_sqrt_price_from_amount1_rounding_down(sqrt_p_x96, liquidity, amount_out, false)
    } else {
        get_next_sqrt_price_from_amount0_rounding_up(sqrt_p_x96, liquidity, amount_out, false)
    }
}

pub fn get_amount0_delta_unsigned(
    mut sqrt_ratio_a_x96: U256,
    mut sqrt_ratio_b_x96: U256,
    liquidity: U256,
    round_up: bool,
) -> U256 {
    if sqrt_ratio_a_x96 > sqrt_ratio_b_x96 {
        std::mem::swap(&mut sqrt_ratio_a_x96, &mut sqrt_ratio_b_x96);
    }

    let numerator1 = liquidity << 96;
    let numerator2 = sqrt_ratio_b_x96 - sqrt_ratio_a_x96;

    if round_up {
        full_math::mul_div_rounding_up(
            full_math::mul_div_rounding_up(numerator1, numerator2, sqrt_ratio_b_x96),
            ONE,
            sqrt_ratio_a_x96,
        )
    } else {
        full_math::mul_div(numerator1, numerator2, sqrt_ratio_b_x96) / sqrt_ratio_a_x96
    }
}

pub fn get_amount1_delta_unsigned(
    mut sqrt_ratio_a_x96: U256,
    mut sqrt_ratio_b_x96: U256,
    liquidity: U256,
    round_up: bool,
) -> U256 {
    if sqrt_ratio_a_x96 > sqrt_ratio_b_x96 {
        std::mem::swap(&mut sqrt_ratio_a_x96, &mut sqrt_ratio_b_x96);
    }

    if round_up {
        full_math::mul_div_rounding_up(liquidity, sqrt_ratio_b_x96 - sqrt_ratio_a_x96, Q96)
    } else {
        full_math::mul_div(liquidity, sqrt_ratio_b_x96 - sqrt_ratio_a_x96, Q96)
    }
}

fn get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_p_x96: U256,
    liquidity: U256,
    amount: U256,
    add: bool,
) -> U256 {
    if amount.is_zero() {
        return sqrt_p_x96;
    }
    let numerator1 = liquidity << 96;

    if add {
        let product = multiply_in_256(amount, sqrt_p_x96);
        if product / amount == sqrt_p_x96 {
            let denominator = add_in_256(numerator1, product);
            if denominator >= numerator1 {
                return full_math::mul_div_rounding_up(numerator1, sqrt_p_x96, denominator);
            }
        }
        full_math::mul_div_rounding_up(numerator1, ONE, numerator1 / sqrt_p_x96 + amount)
    } else {
        let product = multiply_in_256(amount, sqrt_p_x96);
        assert!(product / amount == sqrt_p_x96, "product overflow");
        assert!(numerator1 > product, "numerator1 <= product");
        let denominator = numerator1 - product;
        full_math::mul_div_rounding_up(numerator1, sqrt_p_x96, denominator)
    }
}

fn get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_p_x96: U256,
    liquidity: U256,
    amount: U256,
    add: bool,
) -> U256 {
    if add {
        let quotient = if amount <= MAX_UINT160 {
            (amount << 96) / liquidity
        } else {
            full_math::mul_div(amount, Q96, liquidity)
        };
        sqrt_p_x96 + quotient
    } else {
        let quotient = full_math::mul_div_rounding_up(amount, Q96, liquidity);
        assert!(sqrt_p_x96 > quotient, "sqrtPX96 <= quotient");
        sqrt_p_x96 - quotient
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::U256;

    #[test]
    fn test_amount0_delta_tick_215822() {
        let current = U256::from_dec_str("2077921856167377953745809051759");
        let target = U256::from_dec_str("2100035313168839811738972618346");
        let liquidity = U256::from_u128(7608560);

        let numerator1 = liquidity << 96;
        let numerator2 = target - current;
        eprintln!("numerator1 = {}", numerator1.to_dec_string());
        eprintln!("numerator2 = {}", numerator2.to_dec_string());

        let intermediate = crate::full_math::mul_div(numerator1, numerator2, target);
        eprintln!("intermediate (mul_div) = {}", intermediate.to_dec_string());

        let result = intermediate / current;
        eprintln!("result (intermediate / current) = {}", result.to_dec_string());

        assert_eq!(result, U256::from_u128(3054), "amount0 should be 3054");

        let full_result = get_amount0_delta_unsigned(current, target, liquidity, false);
        assert_eq!(full_result, U256::from_u128(3054), "get_amount0_delta_unsigned should be 3054");
    }

    #[test]
    fn test_amount1_delta_tick_215822() {
        let current = U256::from_dec_str("2077921856167377953745809051759");
        let target = U256::from_dec_str("2100035313168839811738972618346");
        let liquidity = U256::from_u128(7608560);

        let result = get_amount1_delta_unsigned(current, target, liquidity, true);
        eprintln!("amount1_delta(roundUp=true) = {}", result.to_dec_string());
        assert_eq!(result, U256::from_u128(2123634), "amount1 should be 2123634");
    }
}
