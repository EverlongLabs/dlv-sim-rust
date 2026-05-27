// tick_manager.rs — TickManager with BTreeMap

use crate::tick::Tick;
use crate::types::*;
use crate::full_math;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct TickManager {
    ticks: BTreeMap<i32, Tick>,
}

impl TickManager {
    pub fn new() -> Self {
        TickManager {
            ticks: BTreeMap::new(),
        }
    }

    pub fn from_ticks(ticks: BTreeMap<i32, Tick>) -> Self {
        TickManager { ticks }
    }

    pub fn get_tick_and_init_if_absent(&mut self, tick_index: i32) -> &mut Tick {
        if !self.ticks.contains_key(&tick_index) {
            if std::env::var("TRACE_GHOST_TICK").ok().map_or(false, |t| t == tick_index.to_string()) {
                let bt = std::backtrace::Backtrace::force_capture();
                eprintln!("[GHOST_TICK] Creating tick {} — backtrace:\n{}", tick_index, bt);
            }
            self.ticks.insert(tick_index, Tick::new(tick_index));
        }
        self.ticks.get_mut(&tick_index).unwrap()
    }

    pub fn get_tick_readonly(&self, tick_index: i32) -> Tick {
        self.ticks
            .get(&tick_index)
            .cloned()
            .unwrap_or_else(|| Tick::new(tick_index))
    }

    pub fn set(&mut self, tick: Tick) {
        self.ticks.insert(tick.tick_index, tick);
    }

    pub fn clear(&mut self, tick_index: i32) {
        if std::env::var("TRACE_GHOST_TICK").ok().map_or(false, |t| t == tick_index.to_string()) {
            let bt = std::backtrace::Backtrace::force_capture();
            eprintln!("[GHOST_TICK_CLEAR] Clearing tick {} — backtrace:\n{}", tick_index, bt);
        }
        self.ticks.remove(&tick_index);
    }

    pub fn ticks(&self) -> &BTreeMap<i32, Tick> {
        &self.ticks
    }

    /// Get sorted tick indices as a Vec
    pub fn tick_indices(&self) -> Vec<i32> {
        self.ticks.keys().copied().collect()
    }

    /// Get the next initialized tick. Equivalent to TS getNextInitializedTick.
    pub fn get_next_initialized_tick(
        &self,
        tick: i32,
        tick_spacing: i32,
        lte: bool,
    ) -> (i32, bool) {
        let compressed = tick.div_euclid(tick_spacing);
        // Align with the TS logic: Math.floor(tick / tickSpacing)
        // div_euclid gives floor for positive, ceil-toward-zero otherwise
        // Actually for negative: JS Math.floor(-1/10) = -1, Rust (-1i32).div_euclid(10) = -1
        // They match for div_euclid.

        if lte {
            let word_pos = compressed >> 8;
            let minimum = (word_pos << 8) * tick_spacing;

            // Find the greatest initialized tick <= tick
            // BTreeMap::range(..=tick) gives all ticks <= tick
            if let Some((&idx, _)) = self.ticks.range(..=tick).next_back() {
                let next_initialized_tick = std::cmp::max(minimum, idx);
                (next_initialized_tick, next_initialized_tick == idx)
            } else {
                (minimum, false)
            }
        } else {
            let word_pos = (compressed + 1) >> 8;
            let maximum = (((word_pos + 1) << 8) - 1) * tick_spacing;

            // Find the smallest initialized tick > tick
            if let Some((&idx, _)) = self.ticks.range((tick + 1)..) .next() {
                let next_initialized_tick = std::cmp::min(maximum, idx);
                (next_initialized_tick, next_initialized_tick == idx)
            } else {
                (maximum, false)
            }
        }
    }

    pub fn get_fee_growth_inside_readonly(
        &self,
        tick_lower: i32,
        tick_upper: i32,
        tick_current: i32,
        fee_growth_global_0_x128: U256,
        fee_growth_global_1_x128: U256,
    ) -> (U256, U256) {
        let lower = self.get_tick_readonly(tick_lower);
        let upper = self.get_tick_readonly(tick_upper);

        let (fee_growth_below_0, fee_growth_below_1) = if tick_current >= tick_lower {
            (lower.fee_growth_outside_0_x128, lower.fee_growth_outside_1_x128)
        } else {
            (
                full_math::mod256_sub(fee_growth_global_0_x128, lower.fee_growth_outside_0_x128),
                full_math::mod256_sub(fee_growth_global_1_x128, lower.fee_growth_outside_1_x128),
            )
        };
        let (fee_growth_above_0, fee_growth_above_1) = if tick_current < tick_upper {
            (upper.fee_growth_outside_0_x128, upper.fee_growth_outside_1_x128)
        } else {
            (
                full_math::mod256_sub(fee_growth_global_0_x128, upper.fee_growth_outside_0_x128),
                full_math::mod256_sub(fee_growth_global_1_x128, upper.fee_growth_outside_1_x128),
            )
        };

        (
            full_math::mod256_sub(full_math::mod256_sub(fee_growth_global_0_x128, fee_growth_below_0), fee_growth_above_0),
            full_math::mod256_sub(full_math::mod256_sub(fee_growth_global_1_x128, fee_growth_below_1), fee_growth_above_1),
        )
    }

    /// Get fee growth inside a tick range
    pub fn get_fee_growth_inside(
        &mut self,
        tick_lower: i32,
        tick_upper: i32,
        tick_current: i32,
        fee_growth_global_0_x128: U256,
        fee_growth_global_1_x128: U256,
    ) -> (U256, U256) {
        let lower = self.get_tick_readonly(tick_lower);
        let upper = self.get_tick_readonly(tick_upper);

        let (fee_growth_below_0, fee_growth_below_1) = if tick_current >= tick_lower {
            (
                lower.fee_growth_outside_0_x128,
                lower.fee_growth_outside_1_x128,
            )
        } else {
            (
                full_math::mod256_sub(fee_growth_global_0_x128, lower.fee_growth_outside_0_x128),
                full_math::mod256_sub(fee_growth_global_1_x128, lower.fee_growth_outside_1_x128),
            )
        };

        let (fee_growth_above_0, fee_growth_above_1) = if tick_current < tick_upper {
            (
                upper.fee_growth_outside_0_x128,
                upper.fee_growth_outside_1_x128,
            )
        } else {
            (
                full_math::mod256_sub(fee_growth_global_0_x128, upper.fee_growth_outside_0_x128),
                full_math::mod256_sub(fee_growth_global_1_x128, upper.fee_growth_outside_1_x128),
            )
        };

        (
            full_math::mod256_sub(full_math::mod256_sub(fee_growth_global_0_x128, fee_growth_below_0), fee_growth_above_0),
            full_math::mod256_sub(full_math::mod256_sub(fee_growth_global_1_x128, fee_growth_below_1), fee_growth_above_1),
        )
    }
}
