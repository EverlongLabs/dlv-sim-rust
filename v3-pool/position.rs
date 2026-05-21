// position.rs — Position model

use crate::types::*;
use crate::full_math;
use crate::liquidity_math;

#[derive(Clone, Debug)]
pub struct Position {
    pub liquidity: U256,
    pub fee_growth_inside_0_last_x128: U256,
    pub fee_growth_inside_1_last_x128: U256,
    pub tokens_owed_0: U256,
    pub tokens_owed_1: U256,
}

impl Position {
    pub fn new() -> Self {
        Position {
            liquidity: ZERO,
            fee_growth_inside_0_last_x128: ZERO,
            fee_growth_inside_1_last_x128: ZERO,
            tokens_owed_0: ZERO,
            tokens_owed_1: ZERO,
        }
    }

    pub fn update(
        &mut self,
        liquidity_delta: I256,
        fee_growth_inside_0_x128: U256,
        fee_growth_inside_1_x128: U256,
    ) {
        let liquidity_next = if liquidity_delta.is_zero() {
            self.liquidity
        } else {
            liquidity_math::add_delta(self.liquidity, liquidity_delta)
        };

        let tokens_owed_0 = full_math::mul_div(
            full_math::mod256_sub(fee_growth_inside_0_x128, self.fee_growth_inside_0_last_x128),
            self.liquidity,
            Q128,
        );
        let tokens_owed_1 = full_math::mul_div(
            full_math::mod256_sub(fee_growth_inside_1_x128, self.fee_growth_inside_1_last_x128),
            self.liquidity,
            Q128,
        );

        if !liquidity_delta.is_zero() {
            self.liquidity = liquidity_next;
        }
        self.fee_growth_inside_0_last_x128 = fee_growth_inside_0_x128;
        self.fee_growth_inside_1_last_x128 = fee_growth_inside_1_x128;
        if tokens_owed_0 > ZERO || tokens_owed_1 > ZERO {
            self.tokens_owed_0 = self.tokens_owed_0 + tokens_owed_0;
            self.tokens_owed_1 = self.tokens_owed_1 + tokens_owed_1;
        }
    }

    pub fn update_burn(&mut self, new_tokens_owed_0: U256, new_tokens_owed_1: U256) {
        self.tokens_owed_0 = new_tokens_owed_0;
        self.tokens_owed_1 = new_tokens_owed_1;
    }

    pub fn is_empty(&self) -> bool {
        self.liquidity.is_zero()
            && self.tokens_owed_0.is_zero()
            && self.tokens_owed_1.is_zero()
    }
}
