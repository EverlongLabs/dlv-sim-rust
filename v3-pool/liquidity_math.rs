// liquidity_math.rs — LiquidityMath

use crate::types::*;

/// Safe add of unsigned x + signed y with overflow/underflow checks
pub fn add_delta(x: U256, y: I256) -> U256 {
    assert!(x <= MAX_UINT128, "OVERFLOW: x > MaxUint128");
    if y.is_negative() {
        let neg_y = y.negate().0;
        assert!(x >= neg_y, "UNDERFLOW: x < -y");
        x - neg_y
    } else {
        let result = x + y.0;
        assert!(result <= MAX_UINT128, "OVERFLOW: x + y > MaxUint128");
        result
    }
}
