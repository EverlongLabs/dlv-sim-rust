// position_manager.rs — PositionManager

use crate::position::Position;
use crate::types::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct PositionManager {
    positions: HashMap<String, Position>,
}

impl PositionManager {
    pub fn new() -> Self {
        PositionManager {
            positions: HashMap::new(),
        }
    }

    pub fn get_key(owner: &str, tick_lower: i32, tick_upper: i32) -> String {
        format!("{}_{}", owner, format!("{}_{}", tick_lower, tick_upper))
    }

    pub fn set(&mut self, key: String, position: Position) {
        self.positions.insert(key, position);
    }

    pub fn clear(&mut self, key: &str) {
        self.positions.remove(key);
    }

    pub fn get_position_and_init_if_absent(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
    ) -> &mut Position {
        let key = Self::get_key(owner, tick_lower, tick_upper);
        self.positions.entry(key).or_insert_with(Position::new)
    }

    pub fn get_position_readonly(
        &self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
    ) -> Position {
        let key = Self::get_key(owner, tick_lower, tick_upper);
        self.positions
            .get(&key)
            .cloned()
            .unwrap_or_else(Position::new)
    }

    pub fn collect_position(
        &mut self,
        owner: &str,
        tick_lower: i32,
        tick_upper: i32,
        amount0_requested: U256,
        amount1_requested: U256,
    ) -> (U256, U256) {
        assert!(
            amount0_requested >= ZERO && amount1_requested >= ZERO,
            "amounts requested should be positive"
        );
        let key = Self::get_key(owner, tick_lower, tick_upper);
        if let Some(pos) = self.positions.get_mut(&key) {
            let amount0 = if amount0_requested > pos.tokens_owed_0 {
                pos.tokens_owed_0
            } else {
                amount0_requested
            };
            let amount1 = if amount1_requested > pos.tokens_owed_1 {
                pos.tokens_owed_1
            } else {
                amount1_requested
            };

            if amount0 > ZERO || amount1 > ZERO {
                pos.update_burn(pos.tokens_owed_0 - amount0, pos.tokens_owed_1 - amount1);
                if pos.is_empty() {
                    self.positions.remove(&key);
                }
            }

            (amount0, amount1)
        } else {
            (ZERO, ZERO)
        }
    }

    pub fn positions(&self) -> &HashMap<String, Position> {
        &self.positions
    }

    /// Get all position keys as a Vec
    pub fn position_keys(&self) -> Vec<String> {
        self.positions.keys().cloned().collect()
    }
}
