// swap_math.rs — SwapMath.computeSwapStep

use crate::types::*;
use crate::full_math;
use crate::sqrt_price_math;

pub struct SwapStepResult {
    pub sqrt_ratio_next_x96: U256,
    pub amount_in: U256,
    pub amount_out: U256,
    pub fee_amount: U256,
}

pub fn compute_swap_step(
    sqrt_ratio_current_x96: U256,
    sqrt_ratio_target_x96: U256,
    liquidity: U256,
    amount_remaining: I256,
    fee_pips: FeeAmount,
) -> SwapStepResult {
    let zero_for_one = sqrt_ratio_current_x96 >= sqrt_ratio_target_x96;
    let exact_in = amount_remaining >= I256::ZERO;
    let fee_pips_u256 = fee_pips.as_u256();

    let sqrt_ratio_next_x96: U256;
    let mut amount_in: U256;
    let mut amount_out: U256;

    if exact_in {
        let amount_remaining_less_fee = full_math::mul_div(
            amount_remaining.0, // positive, so .0 is unsigned
            MAX_FEE - fee_pips_u256,
            MAX_FEE,
        );

        amount_in = if zero_for_one {
            sqrt_price_math::get_amount0_delta_unsigned(
                sqrt_ratio_target_x96,
                sqrt_ratio_current_x96,
                liquidity,
                true,
            )
        } else {
            sqrt_price_math::get_amount1_delta_unsigned(
                sqrt_ratio_current_x96,
                sqrt_ratio_target_x96,
                liquidity,
                true,
            )
        };

        if amount_remaining_less_fee >= amount_in {
            sqrt_ratio_next_x96 = sqrt_ratio_target_x96;
        } else {
            sqrt_ratio_next_x96 = sqrt_price_math::get_next_sqrt_price_from_input(
                sqrt_ratio_current_x96,
                liquidity,
                amount_remaining_less_fee,
                zero_for_one,
            );
        }
    } else {
        amount_out = if zero_for_one {
            sqrt_price_math::get_amount1_delta_unsigned(
                sqrt_ratio_target_x96,
                sqrt_ratio_current_x96,
                liquidity,
                false,
            )
        } else {
            sqrt_price_math::get_amount0_delta_unsigned(
                sqrt_ratio_current_x96,
                sqrt_ratio_target_x96,
                liquidity,
                false,
            )
        };

        let neg_remaining = amount_remaining.negate().0;
        if neg_remaining >= amount_out {
            sqrt_ratio_next_x96 = sqrt_ratio_target_x96;
        } else {
            sqrt_ratio_next_x96 = sqrt_price_math::get_next_sqrt_price_from_output(
                sqrt_ratio_current_x96,
                liquidity,
                neg_remaining,
                zero_for_one,
            );
        }

        // Need to set amount_in to something for reassignment below
        amount_in = ZERO;
    }

    let max = sqrt_ratio_target_x96 == sqrt_ratio_next_x96;

    if zero_for_one {
        if !(max && exact_in) {
            amount_in = sqrt_price_math::get_amount0_delta_unsigned(
                sqrt_ratio_next_x96,
                sqrt_ratio_current_x96,
                liquidity,
                true,
            );
        }
        amount_out = if max && !exact_in {
            // Keep the amount_out computed above
            if exact_in { ZERO } else {
                // We need to check: for exact_out + max case, keep original
                if zero_for_one {
                    sqrt_price_math::get_amount1_delta_unsigned(
                        sqrt_ratio_target_x96,
                        sqrt_ratio_current_x96,
                        liquidity,
                        false,
                    )
                } else {
                    sqrt_price_math::get_amount0_delta_unsigned(
                        sqrt_ratio_current_x96,
                        sqrt_ratio_target_x96,
                        liquidity,
                        false,
                    )
                }
            }
        } else {
            sqrt_price_math::get_amount1_delta_unsigned(
                sqrt_ratio_next_x96,
                sqrt_ratio_current_x96,
                liquidity,
                false,
            )
        };
    } else {
        if !(max && exact_in) {
            amount_in = sqrt_price_math::get_amount1_delta_unsigned(
                sqrt_ratio_current_x96,
                sqrt_ratio_next_x96,
                liquidity,
                true,
            );
        }
        amount_out = if max && !exact_in {
            if exact_in { ZERO } else {
                sqrt_price_math::get_amount0_delta_unsigned(
                    sqrt_ratio_current_x96,
                    sqrt_ratio_target_x96,
                    liquidity,
                    false,
                )
            }
        } else {
            sqrt_price_math::get_amount0_delta_unsigned(
                sqrt_ratio_current_x96,
                sqrt_ratio_next_x96,
                liquidity,
                false,
            )
        };
    }

    // Cap amount_out for exact output case
    if !exact_in {
        let neg_remaining = amount_remaining.negate().0;
        if amount_out > neg_remaining {
            amount_out = neg_remaining;
        }
    }

    let fee_amount = if exact_in && sqrt_ratio_next_x96 != sqrt_ratio_target_x96 {
        // Didn't reach target: remainder is fee
        amount_remaining.0 - amount_in
    } else {
        full_math::mul_div_rounding_up(amount_in, fee_pips_u256, MAX_FEE - fee_pips_u256)
    };

    SwapStepResult {
        sqrt_ratio_next_x96,
        amount_in,
        amount_out,
        fee_amount,
    }
}
