// tick.rs — Tick model

use crate::types::*;
use crate::liquidity_math;
use crate::full_math;

#[derive(Clone, Debug)]
pub struct Tick {
    pub tick_index: i32,
    pub liquidity_gross: U256,
    pub liquidity_net: I256,
    pub fee_growth_outside_0_x128: U256,
    pub fee_growth_outside_1_x128: U256,
}

impl Tick {
    pub fn new(tick_index: i32) -> Self {
        assert!(
            tick_index >= MIN_TICK && tick_index <= MAX_TICK,
            "TICK out of range"
        );
        Tick {
            tick_index,
            liquidity_gross: ZERO,
            liquidity_net: I256::ZERO,
            fee_growth_outside_0_x128: ZERO,
            fee_growth_outside_1_x128: ZERO,
        }
    }

    pub fn initialized(&self) -> bool {
        !self.liquidity_gross.is_zero()
    }

    /// Update the tick. Returns true if the tick was flipped (initialized↔uninitialized).
    pub fn update(
        &mut self,
        liquidity_delta: I256,
        tick_current: i32,
        fee_growth_global_0_x128: U256,
        fee_growth_global_1_x128: U256,
        upper: bool,
        max_liquidity: U256,
    ) -> bool {
        let liquidity_gross_before = self.liquidity_gross;
        let liquidity_gross_after = liquidity_math::add_delta(liquidity_gross_before, liquidity_delta);
        assert!(
            liquidity_gross_after <= max_liquidity,
            "LO: liquidity_gross_after > max_liquidity"
        );

        let flipped = liquidity_gross_after.is_zero() != liquidity_gross_before.is_zero();

        if liquidity_gross_before.is_zero() {
            if self.tick_index <= tick_current {
                self.fee_growth_outside_0_x128 = fee_growth_global_0_x128;
                self.fee_growth_outside_1_x128 = fee_growth_global_1_x128;
            }
        }

        self.liquidity_gross = liquidity_gross_after;
        if upper {
            self.liquidity_net = self.liquidity_net - liquidity_delta;
        } else {
            self.liquidity_net = self.liquidity_net + liquidity_delta;
        }

        flipped
    }

    /// Cross the tick during a swap. Returns liquidityNet.
    pub fn cross(
        &mut self,
        fee_growth_global_0_x128: U256,
        fee_growth_global_1_x128: U256,
    ) -> I256 {
        self.fee_growth_outside_0_x128 = full_math::mod256_sub(fee_growth_global_0_x128, self.fee_growth_outside_0_x128);
        self.fee_growth_outside_1_x128 = full_math::mod256_sub(fee_growth_global_1_x128, self.fee_growth_outside_1_x128);
        self.liquidity_net
    }
}


