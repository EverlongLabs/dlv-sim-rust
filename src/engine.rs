use v3_pool::core_pool::CorePool;
use v3_pool::types::{I256, U256};

pub struct Engine<'a> {
    pool: &'a mut CorePool,
}

#[derive(Debug, Clone)]
pub struct MintBurnResult {
    pub amount0: I256,
    pub amount1: I256,
}

#[derive(Debug, Clone)]
pub struct CollectResult {
    pub amount0: U256,
    pub amount1: U256,
}

impl<'a> Engine<'a> {
    pub fn new(pool: &'a mut CorePool) -> Self {
        Engine { pool }
    }

    pub fn mint(
        &mut self,
        recipient: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> MintBurnResult {
        let (a0, a1) = self.pool.mint(recipient, tick_lower, tick_upper, amount);
        MintBurnResult { amount0: a0, amount1: a1 }
    }

    pub fn burn(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount: I256,
    ) -> MintBurnResult {
        let (a0, a1) = self.pool.burn(owner, tick_lower, tick_upper, amount);
        MintBurnResult { amount0: a0, amount1: a1 }
    }

    pub fn collect(
        &mut self,
        recipient: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount0_requested: U256,
        amount1_requested: U256,
    ) -> CollectResult {
        let (a0, a1) = self.pool.collect(
            recipient,
            tick_lower,
            tick_upper,
            amount0_requested,
            amount1_requested,
        );
        CollectResult { amount0: a0, amount1: a1 }
    }

    pub fn swap(
        &mut self,
        zero_for_one: bool,
        amount_specified: I256,
        sqrt_price_limit_x96: Option<U256>,
    ) -> MintBurnResult {
        let (a0, a1) = self.pool.swap(zero_for_one, amount_specified, sqrt_price_limit_x96);
        MintBurnResult { amount0: a0, amount1: a1 }
    }

    pub fn pool(&self) -> &CorePool {
        self.pool
    }

    pub fn pool_mut(&mut self) -> &mut CorePool {
        self.pool
    }
}
