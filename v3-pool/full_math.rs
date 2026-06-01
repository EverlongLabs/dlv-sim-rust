// full_math.rs — FullMath operations (mulDiv, mulDivRoundingUp, etc.)

use crate::types::{U256, MAX_UINT256, ZERO, ONE};

/// (a * b) / denominator — full precision via 512-bit intermediate
pub fn mul_div(a: U256, b: U256, denominator: U256) -> U256 {
    assert!(!denominator.is_zero(), "mul_div: division by zero");
    let (lo, hi) = a.full_mul(b);
    if hi.is_zero() {
        return lo / denominator;
    }
    U256::div_512(lo, hi, denominator)
}

/// (a * b) / denominator, rounded up
pub fn mul_div_rounding_up(a: U256, b: U256, denominator: U256) -> U256 {
    assert!(!denominator.is_zero(), "mul_div: division by zero");
    // Compute the 512-bit product once and reuse it for both the quotient and the
    // remainder test (the old code called mul_div — recomputing full_mul — and also
    // built an unused wrapping_mul product).
    let (lo, hi) = a.full_mul(b);
    let result = if hi.is_zero() { lo / denominator } else { U256::div_512(lo, hi, denominator) };
    // Round up when (hi:lo) > result * denominator, i.e. there was a fraction.
    let (check_lo, check_hi) = result.full_mul(denominator);
    if hi > check_hi || (hi == check_hi && lo > check_lo) {
        assert!(result < MAX_UINT256, "OVERFLOW");
        result + ONE
    } else {
        result
    }
}

/// Simulates EVM uint256 `a - b` with wrapping (underflow wraps around)
pub fn mod256_sub(a: U256, b: U256) -> U256 {
    let (result, _) = a.overflowing_sub(b);
    result
}

/// floor(sqrt(value)) — Newton's method
pub fn sqrt(value: U256) -> U256 {
    if value.is_zero() {
        return ZERO;
    }
    if value <= U256::from_u128(u64::MAX as u128) {
        let v = value.lo as u64;
        return U256::from_u128((v as f64).sqrt().floor() as u128);
    }
    // Seed Newton's method with a power-of-two just above the true root instead of
    // value/2. For a ~192-bit input (root ~96 bits) value/2 needed ~95 iterations,
    // each a full 256-bit division; 2^(msb/2 + 1) is >= sqrt(value) and only a few
    // bits off, so it converges in a handful of iterations. The loop and its
    // termination are unchanged, so the floor(sqrt) result is identical.
    let mut x = U256::ONE << (value.msb() / 2 + 1);
    let mut z = value;
    while x < z {
        z = x;
        let div = value / x;
        x = (div + x) >> 1;
    }
    z
}

// Shift right by 1 helper for U256
impl U256 {
    /// Efficient right-shift by 1
    #[inline]
    pub fn shr1(self) -> U256 {
        U256 {
            lo: (self.lo >> 1) | (self.hi << 127),
            hi: self.hi >> 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mul_div_basic() {
        let a = U256::from_u128(100);
        let b = U256::from_u128(200);
        let d = U256::from_u128(50);
        assert_eq!(mul_div(a, b, d), U256::from_u128(400));
    }

    #[test]
    fn test_mul_div_rounding_up() {
        let a = U256::from_u128(10);
        let b = U256::from_u128(3);
        let d = U256::from_u128(7);
        // 30 / 7 = 4.28... → 5
        assert_eq!(mul_div_rounding_up(a, b, d), U256::from_u128(5));
        // exact division: 30 / 6 = 5 → 5
        assert_eq!(
            mul_div_rounding_up(a, b, U256::from_u128(6)),
            U256::from_u128(5)
        );
    }

    #[test]
    fn test_mod256_sub_underflow() {
        let a = U256::from_u128(0);
        let b = U256::from_u128(1);
        let result = mod256_sub(a, b);
        assert_eq!(result, MAX_UINT256);
    }

    #[test]
    fn test_sqrt_basic() {
        assert_eq!(sqrt(U256::from_u128(144)), U256::from_u128(12));
        assert_eq!(sqrt(U256::from_u128(2)), U256::from_u128(1));
        assert_eq!(sqrt(U256::from_u128(0)), U256::ZERO);
    }
}
