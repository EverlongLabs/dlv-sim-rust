// core_pool.rs — CorePool: the hot path

use crate::full_math;
use crate::liquidity_math;
use crate::position::Position;
use crate::sqrt_price_math;
use crate::swap_math;
use crate::tick_manager::TickManager;
use crate::tick_math;
use crate::position_manager::PositionManager;
use crate::types::*;

#[derive(Clone, Debug)]
pub struct CorePool {
    pub token0: String,
    pub token1: String,
    pub fee: u32,
    pub tick_spacing: i32,
    pub max_liquidity_per_tick: U256,

    token0_balance: U256,
    token1_balance: U256,
    sqrt_price_x96: U256,
    liquidity: U256,
    tick_current: i32,
    fee_growth_global_0_x128: U256,
    fee_growth_global_1_x128: U256,

    tick_manager: TickManager,
    position_manager: PositionManager,
}

#[derive(Clone, Debug)]
pub struct SwapResult {
    pub amount0: I256,
    pub amount1: I256,
    pub sqrt_price_x96: U256,
}

impl CorePool {
    pub fn new(
        token0: String,
        token1: String,
        fee: u32,
        tick_spacing: i32,
        token0_balance: U256,
        token1_balance: U256,
        sqrt_price_x96: U256,
        liquidity: U256,
        tick_current: i32,
        fee_growth_global_0_x128: U256,
        fee_growth_global_1_x128: U256,
        tick_manager: TickManager,
        position_manager: PositionManager,
    ) -> Self {
        let max_liquidity_per_tick =
            tick_math::tick_spacing_to_max_liquidity_per_tick(tick_spacing);
        CorePool {
            token0,
            token1,
            fee,
            tick_spacing,
            max_liquidity_per_tick,
            token0_balance,
            token1_balance,
            sqrt_price_x96,
            liquidity,
            tick_current,
            fee_growth_global_0_x128,
            fee_growth_global_1_x128,
            tick_manager,
            position_manager,
        }
    }

    // ─── Accessors ───
    pub fn token0_balance(&self) -> U256 { self.token0_balance }
    pub fn token1_balance(&self) -> U256 { self.token1_balance }
    pub fn sqrt_price_x96(&self) -> U256 { self.sqrt_price_x96 }
    pub fn liquidity(&self) -> U256 { self.liquidity }
    pub fn tick_current(&self) -> i32 { self.tick_current }
    pub fn fee_growth_global_0_x128(&self) -> U256 { self.fee_growth_global_0_x128 }
    pub fn fee_growth_global_1_x128(&self) -> U256 { self.fee_growth_global_1_x128 }
    pub fn tick_manager(&self) -> &TickManager { &self.tick_manager }
    pub fn tick_manager_mut(&mut self) -> &mut TickManager { &mut self.tick_manager }
    pub fn position_manager(&self) -> &PositionManager { &self.position_manager }
    pub fn position_manager_mut(&mut self) -> &mut PositionManager { &mut self.position_manager }

    // ─── Initialize ───
    pub fn initialize(&mut self, sqrt_price_x96: U256) {
        assert!(self.sqrt_price_x96.is_zero(), "Already initialized!");
        self.tick_current = tick_math::get_tick_at_sqrt_ratio(sqrt_price_x96);
        self.sqrt_price_x96 = sqrt_price_x96;
    }

    // ─── Mint variants ───
    pub fn mint(
        &mut self,
        recipient: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> (I256, I256) {
        assert!(amount > I256::ZERO, "Mint amount should be > 0");
        let (_, a0, a1) = self.modify_position(recipient, tick_lower, tick_upper, amount);
        (a0, a1)
    }

    pub fn phantom_mint(
        &mut self,
        recipient: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> (I256, I256) {
        assert!(amount > I256::ZERO, "Mint amount should be > 0");
        let (_, a0, a1) = self.phantom_modify_position(recipient, tick_lower, tick_upper, amount);
        (a0, a1)
    }

    pub fn rapid_mint(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> (I256, I256) {
        assert!(amount > I256::ZERO, "Mint amount should be > 0");
        self.rapid_modify_position(tick_lower, tick_upper, amount)
    }

    // ─── Burn variants ───
    pub fn burn(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> (I256, I256) {
        let neg_amount = -amount;
        let (_pos_snapshot, mut a0, mut a1) =
            self.modify_position(owner, tick_lower, tick_upper, neg_amount);

        a0 = -a0;
        a1 = -a1;

        if a0 > I256::ZERO || a1 > I256::ZERO {
            // Get the position and update burn tokens owed
            let pos = self
                .position_manager
                .get_position_and_init_if_absent(owner, tick_lower, tick_upper);
            let new0 = pos.tokens_owed_0 + a0.0;
            let new1 = pos.tokens_owed_1 + a1.0;
            pos.update_burn(new0, new1);
        }

        (a0, a1)
    }

    pub fn phantom_burn(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> (I256, I256) {
        let neg_amount = -amount;
        let (_pos_snapshot, mut a0, mut a1) =
            self.phantom_modify_position(owner, tick_lower, tick_upper, neg_amount);

        a0 = -a0;
        a1 = -a1;

        if a0 > I256::ZERO || a1 > I256::ZERO {
            let pos = self
                .position_manager
                .get_position_and_init_if_absent(owner, tick_lower, tick_upper);
            let new0 = pos.tokens_owed_0 + a0.0;
            let new1 = pos.tokens_owed_1 + a1.0;
            pos.update_burn(new0, new1);
        }

        (a0, a1)
    }

    pub fn rapid_burn(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> (I256, I256) {
        let neg_amount = -amount;
        let (mut a0, mut a1) = self.rapid_modify_position(tick_lower, tick_upper, neg_amount);
        a0 = -a0;
        a1 = -a1;
        (a0, a1)
    }

    // ─── Collect ───
    pub fn collect(
        &mut self,
        recipient: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount0_requested: U256,
        amount1_requested: U256,
    ) -> (U256, U256) {
        self.check_ticks(tick_lower, tick_upper);
        self.position_manager.collect_position(
            recipient,
            tick_lower,
            tick_upper,
            amount0_requested,
            amount1_requested,
        )
    }

    // ─── Swap ───
    pub fn swap(
        &mut self,
        zero_for_one: bool,
        amount_specified: I256,
        sqrt_price_limit_x96: Option<U256>,
    ) -> (I256, I256) {
        let r = self.handle_swap(zero_for_one, amount_specified, sqrt_price_limit_x96);
        (r.amount0, r.amount1)
    }

    pub fn query_swap(
        &self,
        zero_for_one: bool,
        amount_specified: I256,
        sqrt_price_limit_x96: Option<U256>,
    ) -> SwapResult {
        // Read-only swap: inline the loop without mutating self (no clone needed)
        let sqrt_price_limit_x96 = sqrt_price_limit_x96.unwrap_or_else(|| {
            if zero_for_one {
                min_sqrt_ratio() + ONE
            } else {
                max_sqrt_ratio() - ONE
            }
        });

        if zero_for_one {
            assert!(sqrt_price_limit_x96 > min_sqrt_ratio(), "RATIO_MIN");
            assert!(sqrt_price_limit_x96 < self.sqrt_price_x96, "RATIO_CURRENT");
        } else {
            assert!(sqrt_price_limit_x96 < max_sqrt_ratio(), "RATIO_MAX");
            assert!(sqrt_price_limit_x96 > self.sqrt_price_x96, "RATIO_CURRENT");
        }

        let exact_input = amount_specified >= I256::ZERO;
        let fee_amount_enum = FeeAmount::from_u32(self.fee);

        let mut state_amount_specified_remaining = amount_specified;
        let mut state_amount_calculated = I256::ZERO;
        let mut state_sqrt_price_x96 = self.sqrt_price_x96;
        let mut state_tick = self.tick_current;
        let mut state_liquidity = self.liquidity;
        let mut state_fee_growth_global_x128 = if zero_for_one {
            self.fee_growth_global_0_x128
        } else {
            self.fee_growth_global_1_x128
        };

        let do_trace = std::env::var("TRACE_SWAP_SQRT").ok()
            .map_or(false, |s| U256::from_dec_str(&s) == self.sqrt_price_x96);
        let mut loop_iter = 0u32;

        while !state_amount_specified_remaining.is_zero()
            && state_sqrt_price_x96 != sqrt_price_limit_x96
        {
            let sqrt_price_start_x96 = state_sqrt_price_x96;

            let (tick_next_raw, initialized) =
                self.tick_manager
                    .get_next_initialized_tick(state_tick, self.tick_spacing, zero_for_one);

            let tick_next = tick_next_raw.clamp(MIN_TICK, MAX_TICK);

            let sqrt_price_next_x96 = tick_math::get_sqrt_ratio_at_tick(tick_next);

            let target = if (zero_for_one && sqrt_price_next_x96 < sqrt_price_limit_x96)
                || (!zero_for_one && sqrt_price_next_x96 > sqrt_price_limit_x96)
            {
                sqrt_price_limit_x96
            } else {
                sqrt_price_next_x96
            };

            if do_trace {
                eprintln!("[QS_TRACE] iter={} cur={} tick={} tick_next={} init={} sqrt_next={} limit={} target={} liq={} remaining={} z4o={} fee={}",
                    loop_iter, state_sqrt_price_x96.to_dec_string(), state_tick, tick_next, initialized,
                    sqrt_price_next_x96.to_dec_string(), sqrt_price_limit_x96.to_dec_string(),
                    target.to_dec_string(), state_liquidity.to_dec_string(),
                    state_amount_specified_remaining, zero_for_one, self.fee);
            }

            let step = swap_math::compute_swap_step(
                state_sqrt_price_x96,
                target,
                state_liquidity,
                state_amount_specified_remaining,
                fee_amount_enum,
            );

            if do_trace {
                eprintln!("[QS_TRACE] iter={} step: next={} in={} out={} fee={}",
                    loop_iter, step.sqrt_ratio_next_x96.to_dec_string(),
                    step.amount_in.to_dec_string(), step.amount_out.to_dec_string(),
                    step.fee_amount.to_dec_string());
            }

            state_sqrt_price_x96 = step.sqrt_ratio_next_x96;
            loop_iter += 1;

            if exact_input {
                state_amount_specified_remaining = state_amount_specified_remaining
                    - I256(step.amount_in + step.fee_amount);
                state_amount_calculated = state_amount_calculated - I256(step.amount_out);
            } else {
                state_amount_specified_remaining =
                    state_amount_specified_remaining + I256(step.amount_out);
                state_amount_calculated =
                    state_amount_calculated + I256(step.amount_in + step.fee_amount);
            }

            if state_liquidity > ZERO {
                state_fee_growth_global_x128 = state_fee_growth_global_x128
                    + full_math::mul_div(step.fee_amount, Q128, state_liquidity);
            }

            if state_sqrt_price_x96 == sqrt_price_next_x96 {
                if initialized {
                    // Read-only: don't cross the tick, just read liquidityNet
                    let t = self.tick_manager.get_tick_readonly(tick_next);
                    let liquidity_net = t.liquidity_net;

                    let liquidity_net = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };

                    state_liquidity = liquidity_math::add_delta(state_liquidity, liquidity_net);
                    if do_trace {
                        eprintln!("[QS_TRACE] tick_cross tick={} liq_net={} new_liq={}", tick_next, liquidity_net, state_liquidity.to_dec_string());
                    }
                }

                state_tick = if zero_for_one {
                    tick_next - 1
                } else {
                    tick_next
                };
            } else if state_sqrt_price_x96 != sqrt_price_start_x96 {
                state_tick = tick_math::get_tick_at_sqrt_ratio(state_sqrt_price_x96);
            }
        }

        if do_trace {
            eprintln!("[QS_TRACE] final: iters={} remaining={} calculated={}", loop_iter, state_amount_specified_remaining, state_amount_calculated);
        }

        let (amount0, amount1) = if zero_for_one == exact_input {
            (
                I256(amount_specified.0 - state_amount_specified_remaining.0),
                state_amount_calculated,
            )
        } else {
            (
                state_amount_calculated,
                I256(amount_specified.0 - state_amount_specified_remaining.0),
            )
        };

        if do_trace {
            eprintln!("[QS_TRACE] result: amount0={} amount1={}", amount0, amount1);
        }

        SwapResult {
            amount0,
            amount1,
            sqrt_price_x96: state_sqrt_price_x96,
        }
    }

    // ─── Read accessors ───
    pub fn get_tick(&self, tick: i32) -> crate::tick::Tick {
        self.tick_manager.get_tick_readonly(tick)
    }

    pub fn get_position(&self, owner: &str, tick_lower: i32, tick_upper: i32) -> Position {
        self.position_manager
            .get_position_readonly(owner, tick_lower, tick_upper)
    }

    // ─── Internal: handleSwap ───
    fn handle_swap(
        &mut self,
        zero_for_one: bool,
        amount_specified: I256,
        sqrt_price_limit_x96: Option<U256>,
    ) -> SwapResult {
        let sqrt_price_limit_x96 = sqrt_price_limit_x96.unwrap_or_else(|| {
            if zero_for_one {
                min_sqrt_ratio() + ONE
            } else {
                max_sqrt_ratio() - ONE
            }
        });

        if zero_for_one {
            assert!(sqrt_price_limit_x96 > min_sqrt_ratio(), "RATIO_MIN");
            assert!(sqrt_price_limit_x96 < self.sqrt_price_x96, "RATIO_CURRENT");
        } else {
            assert!(sqrt_price_limit_x96 < max_sqrt_ratio(), "RATIO_MAX");
            assert!(sqrt_price_limit_x96 > self.sqrt_price_x96, "RATIO_CURRENT");
        }

        let exact_input = amount_specified >= I256::ZERO;
        let fee_amount_enum = FeeAmount::from_u32(self.fee);

        let mut state_amount_specified_remaining = amount_specified;
        let mut state_amount_calculated = I256::ZERO;
        let mut state_sqrt_price_x96 = self.sqrt_price_x96;
        let mut state_tick = self.tick_current;
        let mut state_liquidity = self.liquidity;
        let mut state_fee_growth_global_x128 = if zero_for_one {
            self.fee_growth_global_0_x128
        } else {
            self.fee_growth_global_1_x128
        };

        while !state_amount_specified_remaining.is_zero()
            && state_sqrt_price_x96 != sqrt_price_limit_x96
        {
            let sqrt_price_start_x96 = state_sqrt_price_x96;

            let (tick_next_raw, initialized) =
                self.tick_manager
                    .get_next_initialized_tick(state_tick, self.tick_spacing, zero_for_one);

            let tick_next = tick_next_raw.clamp(MIN_TICK, MAX_TICK);

            let sqrt_price_next_x96 = tick_math::get_sqrt_ratio_at_tick(tick_next);

            let target = if (zero_for_one && sqrt_price_next_x96 < sqrt_price_limit_x96)
                || (!zero_for_one && sqrt_price_next_x96 > sqrt_price_limit_x96)
            {
                sqrt_price_limit_x96
            } else {
                sqrt_price_next_x96
            };

            let step = swap_math::compute_swap_step(
                state_sqrt_price_x96,
                target,
                state_liquidity,
                state_amount_specified_remaining,
                fee_amount_enum,
            );

            state_sqrt_price_x96 = step.sqrt_ratio_next_x96;

            if exact_input {
                state_amount_specified_remaining = state_amount_specified_remaining
                    - I256(step.amount_in + step.fee_amount);
                state_amount_calculated = state_amount_calculated - I256(step.amount_out);
            } else {
                state_amount_specified_remaining =
                    state_amount_specified_remaining + I256(step.amount_out);
                state_amount_calculated =
                    state_amount_calculated + I256(step.amount_in + step.fee_amount);
            }

            if state_liquidity > ZERO {
                state_fee_growth_global_x128 = state_fee_growth_global_x128
                    + full_math::mul_div(step.fee_amount, Q128, state_liquidity);
            }

            if state_sqrt_price_x96 == sqrt_price_next_x96 {
                if initialized {
                    let fg0 = if zero_for_one {
                        state_fee_growth_global_x128
                    } else {
                        self.fee_growth_global_0_x128
                    };
                    let fg1 = if zero_for_one {
                        self.fee_growth_global_1_x128
                    } else {
                        state_fee_growth_global_x128
                    };
                    let tick_mut = self.tick_manager.get_tick_and_init_if_absent(tick_next);
                    let liquidity_net = tick_mut.cross(fg0, fg1);

                    let liquidity_net = if zero_for_one {
                        -liquidity_net
                    } else {
                        liquidity_net
                    };

                    state_liquidity = liquidity_math::add_delta(state_liquidity, liquidity_net);
                }

                state_tick = if zero_for_one {
                    tick_next - 1
                } else {
                    tick_next
                };
            } else if state_sqrt_price_x96 != sqrt_price_start_x96 {
                state_tick = tick_math::get_tick_at_sqrt_ratio(state_sqrt_price_x96);
            }
        }

        self.sqrt_price_x96 = state_sqrt_price_x96;
        if state_tick != self.tick_current {
            self.tick_current = state_tick;
        }
        if state_liquidity != self.liquidity {
            self.liquidity = state_liquidity;
        }
        if zero_for_one {
            self.fee_growth_global_0_x128 = state_fee_growth_global_x128;
        } else {
            self.fee_growth_global_1_x128 = state_fee_growth_global_x128;
        }

        let (amount0, amount1) = if zero_for_one == exact_input {
            (
                I256(amount_specified.0 - state_amount_specified_remaining.0),
                state_amount_calculated,
            )
        } else {
            (
                state_amount_calculated,
                I256(amount_specified.0 - state_amount_specified_remaining.0),
            )
        };

        SwapResult {
            amount0,
            amount1,
            sqrt_price_x96: state_sqrt_price_x96,
        }
    }

    // ─── Internal: modifyPosition ───
    fn modify_position(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: I256,
    ) -> (Position, I256, I256) {
        self.check_ticks(tick_lower, tick_upper);

        // Check underflow
        if liquidity_delta.is_negative() {
            let pos_view = self.position_manager.get_position_readonly(owner, tick_lower, tick_upper);
            assert!(
                pos_view.liquidity >= liquidity_delta.negate().0,
                "Liquidity Underflow"
            );
        }

        // Update ticks
        let mut flipped_lower = false;
        let mut flipped_upper = false;
        if !liquidity_delta.is_zero() {
            let tick_current = self.tick_current;
            let fg0 = self.fee_growth_global_0_x128;
            let fg1 = self.fee_growth_global_1_x128;
            let max_liq = self.max_liquidity_per_tick;

            let lower = self.tick_manager.get_tick_and_init_if_absent(tick_lower);
            flipped_lower = lower.update(liquidity_delta, tick_current, fg0, fg1, false, max_liq);

            let upper = self.tick_manager.get_tick_and_init_if_absent(tick_upper);
            flipped_upper = upper.update(liquidity_delta, tick_current, fg0, fg1, true, max_liq);
        }

        // Fee growth inside
        let (fg_inside0, fg_inside1) = self.tick_manager.get_fee_growth_inside(
            tick_lower,
            tick_upper,
            self.tick_current,
            self.fee_growth_global_0_x128,
            self.fee_growth_global_1_x128,
        );

        // Update position
        let pos = self
            .position_manager
            .get_position_and_init_if_absent(owner, tick_lower, tick_upper);
        pos.update(liquidity_delta, fg_inside0, fg_inside1);
        let pos_snapshot = pos.clone();

        // Clear flipped ticks on burn
        if liquidity_delta.is_negative() {
            if flipped_lower {
                self.tick_manager.clear(tick_lower);
            }
            if flipped_upper {
                self.tick_manager.clear(tick_upper);
            }
        }

        // Compute amounts
        let (amount0, amount1) = self.compute_position_amounts(tick_lower, tick_upper, liquidity_delta);

        // Update active liquidity
        if !liquidity_delta.is_zero()
            && self.tick_current >= tick_lower
            && self.tick_current < tick_upper
        {
            self.liquidity = liquidity_math::add_delta(self.liquidity, liquidity_delta);
        }

        (pos_snapshot, amount0, amount1)
    }

    fn phantom_modify_position(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: I256,
    ) -> (Position, I256, I256) {
        self.check_ticks(tick_lower, tick_upper);

        if liquidity_delta.is_negative() {
            let pos_view = self.position_manager.get_position_readonly(owner, tick_lower, tick_upper);
            assert!(
                pos_view.liquidity >= liquidity_delta.negate().0,
                "Liquidity Underflow"
            );
        }

        // Fee growth inside (no tick update)
        let (fg_inside0, fg_inside1) = self.tick_manager.get_fee_growth_inside(
            tick_lower,
            tick_upper,
            self.tick_current,
            self.fee_growth_global_0_x128,
            self.fee_growth_global_1_x128,
        );

        let pos = self
            .position_manager
            .get_position_and_init_if_absent(owner, tick_lower, tick_upper);
        pos.update(liquidity_delta, fg_inside0, fg_inside1);
        let pos_snapshot = pos.clone();

        let (amount0, amount1) = self.compute_position_amounts(tick_lower, tick_upper, liquidity_delta);

        (pos_snapshot, amount0, amount1)
    }

    fn rapid_modify_position(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: I256,
    ) -> (I256, I256) {
        self.check_ticks(tick_lower, tick_upper);

        let mut flipped_lower = false;
        let mut flipped_upper = false;
        if !liquidity_delta.is_zero() {
            let tick_current = self.tick_current;
            let fg0 = self.fee_growth_global_0_x128;
            let fg1 = self.fee_growth_global_1_x128;
            let max_liq = self.max_liquidity_per_tick;

            let lower = self.tick_manager.get_tick_and_init_if_absent(tick_lower);
            flipped_lower = lower.update(liquidity_delta, tick_current, fg0, fg1, false, max_liq);

            let upper = self.tick_manager.get_tick_and_init_if_absent(tick_upper);
            flipped_upper = upper.update(liquidity_delta, tick_current, fg0, fg1, true, max_liq);
        }

        if liquidity_delta.is_negative() {
            if flipped_lower {
                self.tick_manager.clear(tick_lower);
            }
            if flipped_upper {
                self.tick_manager.clear(tick_upper);
            }
        }

        let (amount0, amount1) = self.compute_position_amounts(tick_lower, tick_upper, liquidity_delta);

        // Update active liquidity
        if !liquidity_delta.is_zero()
            && self.tick_current >= tick_lower
            && self.tick_current < tick_upper
        {
            self.liquidity = liquidity_math::add_delta(self.liquidity, liquidity_delta);
        }

        (amount0, amount1)
    }

    fn compute_position_amounts(&self, tick_lower: i32, tick_upper: i32, liquidity_delta: I256) -> (I256, I256) {
        if liquidity_delta.is_zero() {
            return (I256::ZERO, I256::ZERO);
        }

        let mut amount0 = I256::ZERO;
        let mut amount1 = I256::ZERO;

        if self.tick_current < tick_lower {
            amount0 = sqrt_price_math::get_amount0_delta(
                tick_math::get_sqrt_ratio_at_tick(tick_lower),
                tick_math::get_sqrt_ratio_at_tick(tick_upper),
                liquidity_delta,
            );
        } else if self.tick_current < tick_upper {
            amount0 = sqrt_price_math::get_amount0_delta(
                self.sqrt_price_x96,
                tick_math::get_sqrt_ratio_at_tick(tick_upper),
                liquidity_delta,
            );
            amount1 = sqrt_price_math::get_amount1_delta(
                tick_math::get_sqrt_ratio_at_tick(tick_lower),
                self.sqrt_price_x96,
                liquidity_delta,
            );
        } else {
            amount1 = sqrt_price_math::get_amount1_delta(
                tick_math::get_sqrt_ratio_at_tick(tick_lower),
                tick_math::get_sqrt_ratio_at_tick(tick_upper),
                liquidity_delta,
            );
        }

        (amount0, amount1)
    }

    fn check_ticks(&self, tick_lower: i32, tick_upper: i32) {
        assert!(tick_lower < tick_upper, "tickLower should be lower than tickUpper");
        assert!(tick_lower >= MIN_TICK, "tickLower should NOT be lower than MIN_TICK");
        assert!(tick_upper <= MAX_TICK, "tickUpper should NOT be greater than MAX_TICK");
    }
}
