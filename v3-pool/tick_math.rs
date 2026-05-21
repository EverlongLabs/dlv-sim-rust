// tick_math.rs — TickMath: getSqrtRatioAtTick, getTickAtSqrtRatio

use crate::types::*;

/// Magic constants for getSqrtRatioAtTick (Q128.128 format)
const MAGIC: [U256; 20] = [
    // bit 0 (0x1)
    U256::new(0xfffcb933bd6fad37aa2d162d1a594001, 0),
    // bit 1 (0x2)
    U256::new(0xfff97272373d413259a46990580e213a, 0),
    // bit 2 (0x4)
    U256::new(0xfff2e50f5f656932ef12357cf3c7fdcc, 0),
    // bit 3 (0x8)
    U256::new(0xffe5caca7e10e4e61c3624eaa0941cd0, 0),
    // bit 4 (0x10)
    U256::new(0xffcb9843d60f6159c9db58835c926644, 0),
    // bit 5 (0x20)
    U256::new(0xff973b41fa98c081472e6896dfb254c0, 0),
    // bit 6 (0x40)
    U256::new(0xff2ea16466c96a3843ec78b326b52861, 0),
    // bit 7 (0x80)
    U256::new(0xfe5dee046a99a2a811c461f1969c3053, 0),
    // bit 8 (0x100)
    U256::new(0xfcbe86c7900a88aedcffc83b479aa3a4, 0),
    // bit 9 (0x200)
    U256::new(0xf987a7253ac413176f2b074cf7815e54, 0),
    // bit 10 (0x400)
    U256::new(0xf3392b0822b70005940c7a398e4b70f3, 0),
    // bit 11 (0x800)
    U256::new(0xe7159475a2c29b7443b29c7fa6e889d9, 0),
    // bit 12 (0x1000)
    U256::new(0xd097f3bdfd2022b8845ad8f792aa5825, 0),
    // bit 13 (0x2000)
    U256::new(0xa9f746462d870fdf8a65dc1f90e061e5, 0),
    // bit 14 (0x4000)
    U256::new(0x70d869a156d2a1b890bb3df62baf32f7, 0),
    // bit 15 (0x8000)
    U256::new(0x31be135f97d08fd981231505542fcfa6, 0),
    // bit 16 (0x10000)
    U256::new(0x9aa508b5b7a84e1c677de54f3e99bc9, 0),
    // bit 17 (0x20000)
    U256::new(0x5d6af8dedb81196699c329225ee604, 0),
    // bit 18 (0x40000)
    U256::new(0x2216e584f5fa1ea926041bedfe98, 0),
    // bit 19 (0x80000)
    U256::new(0x48a170391f7dc42444e8fa2, 0),
];

const BASE_RATIO: U256 = U256::new(0, 1); // 2^128 = Q128

/// (val * mulBy) >> 128
#[inline]
fn mul_shift(val: U256, mul_by: U256) -> U256 {
    let (lo, hi) = val.full_mul(mul_by);
    // Shift 512-bit result right by 128
    U256::new(
        (lo.hi) | (hi.lo.wrapping_shl(0)), // lo bits [128..255] become lo[0..127]
        hi.lo,                               // hi bits [0..127] become hi[0..127]
    )
}

/// Returns the sqrt ratio as a Q64.96 for the given tick
pub fn get_sqrt_ratio_at_tick(tick: i32) -> U256 {
    assert!(
        tick >= MIN_TICK && tick <= MAX_TICK,
        "TICK out of range: {}", tick
    );

    let abs_tick = tick.unsigned_abs();

    let mut ratio = if abs_tick & 0x1 != 0 {
        MAGIC[0]
    } else {
        BASE_RATIO
    };

    for i in 1..20u32 {
        if abs_tick & (1 << i) != 0 {
            ratio = mul_shift(ratio, MAGIC[i as usize]);
        }
    }

    if tick > 0 {
        ratio = MAX_UINT256 / ratio;
    }

    // back to Q96: divide by Q32, rounding up if there's a remainder
    let rem = ratio % Q32;
    let result = ratio / Q32;
    if rem > ZERO {
        result + ONE
    } else {
        result
    }
}

/// Most significant bit of a U256 (0-indexed)
pub fn most_significant_bit(x: U256) -> u32 {
    assert!(!x.is_zero(), "ZERO");
    assert!(x <= MAX_UINT256, "MAX");
    x.msb()
}

/// Returns the tick corresponding to a given sqrt ratio
pub fn get_tick_at_sqrt_ratio(sqrt_ratio_x96: U256) -> i32 {
    let min_sr = min_sqrt_ratio();
    let max_sr = max_sqrt_ratio();
    assert!(
        sqrt_ratio_x96 >= min_sr && sqrt_ratio_x96 < max_sr,
        "SQRT_RATIO"
    );

    let sqrt_ratio_x128 = sqrt_ratio_x96 << 32;
    let msb = most_significant_bit(sqrt_ratio_x128);

    let mut r: U256 = if msb >= 128 {
        sqrt_ratio_x128 >> (msb - 127)
    } else {
        sqrt_ratio_x128 << (127 - msb)
    };

    let mut log_2: I256 = I256::from_i32((msb as i32) - 128);
    log_2 = I256(log_2.0 << 64);

    for i in 0..14u32 {
        let (lo, hi) = r.full_mul(r);
        // (hi:lo) is a 512-bit value. We need bits [127..382] as a 256-bit value.
        // Shift right by 127: result = (hi << (256-127)) | (lo >> 127) = (hi << 129) | (lo >> 127)
        // But we only keep the lower 256 bits of the result.
        r = (lo >> 127) | (hi << 129);

        let f = r >> 128;
        log_2 = I256(log_2.0 | (f << (63 - i)));
        r = r >> f.lo as u32; // f is 0 or 1
    }

    let log_sqrt10001 = I256(log_2.0).wrapping_mul(I256(U256::from_dec_str(
        "255738958999603826347141",
    )));

    let tick_low_i256 = log_sqrt10001
        - I256(U256::from_dec_str(
            "3402992956809132418596140100660247210",
        ));
    let tick_low = I256(tick_low_i256.0 >> 128).as_i32();

    let tick_high_i256 = log_sqrt10001
        + I256(U256::from_dec_str(
            "291339464771989622907027621153398088495",
        ));
    let tick_high = I256(tick_high_i256.0 >> 128).as_i32();

    if tick_low == tick_high {
        tick_low
    } else if get_sqrt_ratio_at_tick(tick_high) <= sqrt_ratio_x96 {
        tick_high
    } else {
        tick_low
    }
}

/// Compute max liquidity per tick for a given tick spacing
pub fn tick_spacing_to_max_liquidity_per_tick(tick_spacing: i32) -> U256 {
    let ts = tick_spacing as i64;
    let min_tick = (MIN_TICK as i64 / ts) * ts;
    let max_tick = (MAX_TICK as i64 / ts) * ts;
    let num_ticks = ((max_tick - min_tick) / ts) + 1;
    MAX_UINT128 / U256::from_u128(num_ticks as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_sqrt_ratio_at_tick_zero() {
        let result = get_sqrt_ratio_at_tick(0);
        // sqrt(1.0001^0) = 1, in Q64.96 = 2^96 = Q96
        assert_eq!(result, Q96);
    }

    #[test]
    fn test_get_sqrt_ratio_at_min_tick() {
        let result = get_sqrt_ratio_at_tick(MIN_TICK);
        assert_eq!(result, min_sqrt_ratio());
    }

    #[test]
    fn test_get_sqrt_ratio_at_max_tick() {
        let result = get_sqrt_ratio_at_tick(MAX_TICK);
        assert_eq!(result, max_sqrt_ratio());
    }

    #[test]
    fn test_get_tick_at_sqrt_ratio_q96() {
        // At Q96 (price = 1.0), tick should be 0
        let tick = get_tick_at_sqrt_ratio(Q96);
        assert_eq!(tick, 0);
    }

    #[test]
    fn test_tick_spacing_to_max_liquidity_per_tick_60() {
        let result = tick_spacing_to_max_liquidity_per_tick(60);
        // Known value: 11505743598341114571880798222544994
        assert_eq!(
            result.to_dec_string(),
            "11505743598341114571880798222544994"
        );
    }
}
