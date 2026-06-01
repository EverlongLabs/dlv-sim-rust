// types.rs — U256 / I256 arithmetic for Uniswap V3 math
// We use a pair of u128 (lo, hi) and implement the operations inline
// to avoid external crate dependencies and keep control over overflow semantics.

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, BitAnd, BitOr, Div, Mul, Neg, Not, Rem, Shl, Shr, Sub};

// ═══════════════════════════  U256  ═══════════════════════════

#[derive(Copy, Clone, Eq, PartialEq, Hash, Default)]
pub struct U256 {
    pub lo: u128,
    pub hi: u128,
}

impl U256 {
    pub const ZERO: U256 = U256 { lo: 0, hi: 0 };
    pub const ONE: U256 = U256 { lo: 1, hi: 0 };
    pub const MAX: U256 = U256 {
        lo: u128::MAX,
        hi: u128::MAX,
    };

    #[inline]
    pub const fn new(lo: u128, hi: u128) -> Self {
        U256 { lo, hi }
    }

    #[inline]
    pub const fn from_u128(v: u128) -> Self {
        U256 { lo: v, hi: 0 }
    }

    #[inline]
    pub const fn from_u64(v: u64) -> Self {
        U256 {
            lo: v as u128,
            hi: 0,
        }
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        self.lo == 0 && self.hi == 0
    }

    /// Count leading zeros
    pub fn leading_zeros(&self) -> u32 {
        if self.hi != 0 {
            self.hi.leading_zeros()
        } else {
            128 + self.lo.leading_zeros()
        }
    }

    /// Count trailing zeros (256 for zero)
    #[inline]
    pub fn trailing_zeros(&self) -> u32 {
        if self.lo != 0 {
            self.lo.trailing_zeros()
        } else if self.hi != 0 {
            128 + self.hi.trailing_zeros()
        } else {
            256
        }
    }

    /// True for a non-zero exact power of two (single bit set)
    #[inline]
    pub fn is_power_of_two(&self) -> bool {
        let (m1, _) = self.overflowing_sub(U256::ONE);
        !self.is_zero() && (self.lo & m1.lo) == 0 && (self.hi & m1.hi) == 0
    }

    /// Most significant bit position (0-indexed), panics on zero
    pub fn msb(&self) -> u32 {
        assert!(!self.is_zero(), "ZERO");
        255 - self.leading_zeros()
    }

    /// Overflowing add returning (result, carry)
    #[inline]
    pub const fn overflowing_add(self, rhs: U256) -> (U256, bool) {
        let (lo, carry0) = self.lo.overflowing_add(rhs.lo);
        let (hi, carry1) = self.hi.overflowing_add(rhs.hi);
        let (hi, carry2) = hi.overflowing_add(carry0 as u128);
        (U256 { lo, hi }, carry1 | carry2)
    }

    /// Overflowing sub returning (result, borrow)
    #[inline]
    pub const fn overflowing_sub(self, rhs: U256) -> (U256, bool) {
        let (lo, borrow0) = self.lo.overflowing_sub(rhs.lo);
        let (hi, borrow1) = self.hi.overflowing_sub(rhs.hi);
        let (hi, borrow2) = hi.overflowing_sub(borrow0 as u128);
        (U256 { lo, hi }, borrow1 | borrow2)
    }

    /// wrapping_mul — full 256-bit result (truncated to 256 bits)
    pub fn wrapping_mul(self, rhs: U256) -> U256 {
        // Split each into (a_lo, a_hi) each u128, then into 64-bit limbs for 512-bit product
        // but we only need the lower 256 bits.
        let a0 = self.lo as u64 as u128;
        let a1 = (self.lo >> 64) as u64 as u128;
        let a2 = self.hi as u64 as u128;
        let a3 = (self.hi >> 64) as u64 as u128;

        let b0 = rhs.lo as u64 as u128;
        let b1 = (rhs.lo >> 64) as u64 as u128;
        let b2 = rhs.hi as u64 as u128;
        let b3 = (rhs.hi >> 64) as u64 as u128;

        // We need limbs 0..3 of the product (each 64-bit) = 256 bits
        let mut carry: u128;
        let p0 = a0 * b0;
        let r0 = p0 as u64 as u128;
        carry = p0 >> 64;

        let p1 = a1 * b0;
        let p2 = a0 * b1;
        let s1 = carry + p1 + p2;
        let r1 = s1 as u64 as u128;
        carry = s1 >> 64;

        let p3 = a2 * b0;
        let p4 = a1 * b1;
        let p5 = a0 * b2;
        let s2 = carry + p3 + p4 + p5;
        let r2 = s2 as u64 as u128;
        carry = s2 >> 64;

        let p6 = a3 * b0;
        let p7 = a2 * b1;
        let p8 = a1 * b2;
        let p9 = a0 * b3;
        let s3 = carry + p6 + p7 + p8 + p9;
        let r3 = s3 as u64 as u128;

        let lo = r0 | (r1 << 64);
        let hi = r2 | (r3 << 64);
        U256 { lo, hi }
    }

    /// Full 512-bit multiply returning (lo256, hi256)
    pub fn full_mul(self, rhs: U256) -> (U256, U256) {
        let a0 = self.lo as u64 as u128;
        let a1 = (self.lo >> 64) as u64 as u128;
        let a2 = self.hi as u64 as u128;
        let a3 = (self.hi >> 64) as u64 as u128;

        let b0 = rhs.lo as u64 as u128;
        let b1 = (rhs.lo >> 64) as u64 as u128;
        let b2 = rhs.hi as u64 as u128;
        let b3 = (rhs.hi >> 64) as u64 as u128;

        // 8 limbs of 64 bits each
        let mut limbs = [0u128; 8];

        // accumulate products
        for (i, &a) in [a0, a1, a2, a3].iter().enumerate() {
            let mut carry: u128 = 0;
            for (j, &b) in [b0, b1, b2, b3].iter().enumerate() {
                let k = i + j;
                let prod = a * b + limbs[k] + carry;
                limbs[k] = prod & 0xFFFFFFFFFFFFFFFF;
                carry = prod >> 64;
            }
            if i + 4 < 8 {
                limbs[i + 4] += carry;
            }
        }

        let lo = U256::new(
            limbs[0] | (limbs[1] << 64),
            limbs[2] | (limbs[3] << 64),
        );
        let hi = U256::new(
            limbs[4] | (limbs[5] << 64),
            limbs[6] | (limbs[7] << 64),
        );
        (lo, hi)
    }

    /// Divide 512-bit (num_hi:num_lo) / divisor, returning quotient.
    /// Only the low 256 bits of quotient are returned.
    /// Uses 64-bit limb-based long division to avoid stack overflow in debug builds.
    pub fn div_512(num_lo: U256, num_hi: U256, divisor: U256) -> U256 {
        if divisor.is_zero() {
            panic!("division by zero");
        }
        if num_hi.is_zero() {
            if divisor.hi == 0 && num_lo.hi == 0 {
                return U256::from_u128(num_lo.lo / divisor.lo);
            }
            return Self::div_256_by_256(num_lo, divisor);
        }

        // Division by a power of two (e.g. Q96, Q128) is a right shift of the
        // 512-bit numerator; the low 256 bits of (num_hi:num_lo) >> k match what
        // the limb long-division below would truncate to.
        if divisor.is_power_of_two() {
            let k = divisor.trailing_zeros();
            if k == 0 {
                return num_lo;
            }
            return (num_lo >> k) | (num_hi << (256 - k));
        }

        // Convert to 64-bit limb arrays for the division
        // Numerator: 8 limbs (512 bits)
        let n = [
            num_lo.lo as u64,
            (num_lo.lo >> 64) as u64,
            num_lo.hi as u64,
            (num_lo.hi >> 64) as u64,
            num_hi.lo as u64,
            (num_hi.lo >> 64) as u64,
            num_hi.hi as u64,
            (num_hi.hi >> 64) as u64,
        ];

        // Divisor: 4 limbs (256 bits)
        let d = [
            divisor.lo as u64,
            (divisor.lo >> 64) as u64,
            divisor.hi as u64,
            (divisor.hi >> 64) as u64,
        ];

        let q = Self::div_limbs(&n, &d);
        U256::new(
            q[0] as u128 | ((q[1] as u128) << 64),
            q[2] as u128 | ((q[3] as u128) << 64),
        )
    }

    /// Simple 256-bit / 256-bit division using 64-bit limbs
    fn div_256_by_256(num: U256, divisor: U256) -> U256 {
        let n = [
            num.lo as u64,
            (num.lo >> 64) as u64,
            num.hi as u64,
            (num.hi >> 64) as u64,
            0u64, 0, 0, 0,
        ];
        let d = [
            divisor.lo as u64,
            (divisor.lo >> 64) as u64,
            divisor.hi as u64,
            (divisor.hi >> 64) as u64,
        ];
        let q = Self::div_limbs(&n, &d);
        U256::new(
            q[0] as u128 | ((q[1] as u128) << 64),
            q[2] as u128 | ((q[3] as u128) << 64),
        )
    }

    /// Divides an 8-limb (512-bit) numerator by a 4-limb (256-bit) divisor
    /// using Knuth's Algorithm D (schoolbook division on 64-bit limbs).
    /// Returns the lower 4 limbs (256 bits) of the quotient.
    fn div_limbs(n: &[u64; 8], d: &[u64; 4]) -> [u64; 4] {
        // Find the effective length of the divisor
        let mut m_d: usize = 4;
        while m_d > 1 && d[m_d - 1] == 0 {
            m_d -= 1;
        }

        if m_d == 1 {
            // Simple case: divide by a single limb
            return Self::div_by_single_limb(n, d[0]);
        }

        // Knuth Algorithm D
        // Normalize: shift so that the top bit of d[m_d-1] is set
        let shift = d[m_d - 1].leading_zeros();

        // Shifted divisor
        let mut v = [0u64; 4];
        if shift > 0 {
            for i in (1..m_d).rev() {
                v[i] = (d[i] << shift) | (d[i - 1] >> (64 - shift));
            }
            v[0] = d[0] << shift;
        } else {
            v[..m_d].copy_from_slice(&d[..m_d]);
        }

        // Shifted numerator (9 limbs to handle carry)
        let mut u = [0u64; 9];
        if shift > 0 {
            u[8] = n[7] >> (64 - shift);
            for i in (1..8).rev() {
                u[i] = (n[i] << shift) | (n[i - 1] >> (64 - shift));
            }
            u[0] = n[0] << shift;
        } else {
            u[..8].copy_from_slice(&n[..8]);
        }

        let m_n = 8; // numerator limbs (before shift, the 9th might be nonzero)

        let mut q = [0u64; 4];

        for j in (0..=(m_n - m_d)).rev() {
            if j >= 4 {
                // Quotient limb beyond 256 bits, skip (we only need lower 4)
                // But we still need to update u for correctness of lower quotient limbs
                // Actually, we need to perform the subtraction step.
            }

            // Estimate q_hat = (u[j+m_d]*B + u[j+m_d-1]) / v[m_d-1]
            let u_hi = u[j + m_d] as u128;
            let u_mid = u[j + m_d - 1] as u128;
            let v_hi = v[m_d - 1] as u128;

            let mut q_hat = if u_hi == v_hi {
                u64::MAX as u128
            } else {
                (u_hi * (1u128 << 64) + u_mid) / v_hi
            };

            // Refine with v[m_d-2]
            if m_d >= 2 {
                let v_lo = v[m_d - 2] as u128;
                loop {
                    let rhat = u_hi * (1u128 << 64) + u_mid - q_hat * v_hi;
                    if rhat >= (1u128 << 64) {
                        break;
                    }
                    if q_hat * v_lo > rhat * (1u128 << 64) + u[j + m_d - 2] as u128 {
                        q_hat -= 1;
                    } else {
                        break;
                    }
                }
            }

            // Multiply and subtract: u[j..j+m_d+1] -= q_hat * v[0..m_d]
            let mut borrow: i128 = 0;
            for i in 0..m_d {
                let prod = q_hat * v[i] as u128;
                let diff = u[j + i] as i128 - borrow - (prod as u64) as i128;
                u[j + i] = diff as u64;
                borrow = (prod >> 64) as i128 - (diff >> 64);
            }
            let diff = u[j + m_d] as i128 - borrow;
            u[j + m_d] = diff as u64;

            if diff < 0 {
                // q_hat was too large, add back
                q_hat -= 1;
                let mut carry: u128 = 0;
                for i in 0..m_d {
                    let s = u[j + i] as u128 + v[i] as u128 + carry;
                    u[j + i] = s as u64;
                    carry = s >> 64;
                }
                u[j + m_d] = u[j + m_d].wrapping_add(carry as u64);
            }

            if j < 4 {
                q[j] = q_hat as u64;
            }
        }

        q
    }

    /// Divide 8-limb number by a single 64-bit limb
    fn div_by_single_limb(n: &[u64; 8], d: u64) -> [u64; 4] {
        let mut q = [0u64; 4];
        let mut rem: u128 = 0;
        let d128 = d as u128;
        for i in (0..8).rev() {
            let cur = (rem << 64) | n[i] as u128;
            let qi = cur / d128;
            rem = cur % d128;
            if i < 4 {
                q[i] = qi as u64;
            }
        }
        q
    }

    /// Parse from decimal string
    pub fn from_dec_str(s: &str) -> Self {
        let s = s.trim();
        let mut result = U256::ZERO;
        let ten = U256::from_u128(10);
        for c in s.chars() {
            let digit = c.to_digit(10).expect("invalid digit") as u128;
            result = result.wrapping_mul(ten);
            result = result + U256::from_u128(digit);
        }
        result
    }

    /// Parse from hex string (with or without 0x prefix)
    pub fn from_hex_str(s: &str) -> Self {
        let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
        let mut result = U256::ZERO;
        let sixteen = U256::from_u128(16);
        for c in s.chars() {
            let digit = c.to_digit(16).expect("invalid hex digit") as u128;
            result = result.wrapping_mul(sixteen);
            result = result + U256::from_u128(digit);
        }
        result
    }

    /// Convert to decimal string
    pub fn to_dec_string(&self) -> String {
        // Fast path: values that fit in u128 (the common case for idle/fees/ticks)
        // use native formatting with no big-int division at all.
        if self.hi == 0 {
            return self.lo.to_string();
        }
        // Peel off 19 decimal digits per division (10^19 fits in u64), instead of
        // one digit per division. The old loop did two 256-bit divisions *per digit*
        // (`%` is itself div+mul+sub, plus a separate `/`) — ~156 div_limbs calls for
        // a 78-digit number; this does ~5.
        const CHUNK: u128 = 10_000_000_000_000_000_000; // 10^19
        let chunk = U256::from_u128(CHUNK);
        let mut val = *self;
        let mut parts: Vec<u64> = Vec::new(); // little-endian, each < 10^19
        while !val.is_zero() {
            let q = val / chunk;
            let r = val - q.wrapping_mul(chunk); // remainder < 10^19, fits in lo
            parts.push(r.lo as u64);
            val = q;
        }
        let mut it = parts.iter().rev();
        let mut s = it.next().unwrap().to_string(); // top chunk: no leading zeros
        for p in it {
            s.push_str(&format!("{:019}", p)); // lower chunks zero-padded
        }
        s
    }

    /// Serialize as 32-byte little-endian
    pub fn to_le_bytes(&self) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(&self.lo.to_le_bytes());
        bytes[16..].copy_from_slice(&self.hi.to_le_bytes());
        bytes
    }

    /// Deserialize from 32-byte little-endian slice
    pub fn from_le_bytes(bytes: &[u8]) -> Self {
        let lo = u128::from_le_bytes(bytes[..16].try_into().unwrap());
        let hi = u128::from_le_bytes(bytes[16..32].try_into().unwrap());
        U256 { lo, hi }
    }

    /// Convert to signed I256 (reinterpret bits)
    pub fn as_i256(self) -> I256 {
        I256(self)
    }

    /// Convert to u128, panics if > u128::MAX
    pub fn as_u128(&self) -> u128 {
        assert!(self.hi == 0, "U256 too large for u128");
        self.lo
    }

    /// Convert to i32 (from low bits)
    pub fn low_u32(&self) -> u32 {
        self.lo as u32
    }
}

impl fmt::Debug for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "U256({})", self.to_dec_string())
    }
}

impl fmt::Display for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_dec_string())
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.hi.cmp(&other.hi).then(self.lo.cmp(&other.lo))
    }
}

impl Add for U256 {
    type Output = U256;
    fn add(self, rhs: U256) -> U256 {
        let (result, _) = self.overflowing_add(rhs);
        result
    }
}

impl Sub for U256 {
    type Output = U256;
    fn sub(self, rhs: U256) -> U256 {
        let (result, _) = self.overflowing_sub(rhs);
        result
    }
}

impl Mul for U256 {
    type Output = U256;
    fn mul(self, rhs: U256) -> U256 {
        self.wrapping_mul(rhs)
    }
}

impl Div for U256 {
    type Output = U256;
    fn div(self, rhs: U256) -> U256 {
        assert!(!rhs.is_zero(), "division by zero");
        if self.hi == 0 && rhs.hi == 0 {
            return U256::from_u128(self.lo / rhs.lo);
        }
        // Division by a power of two (e.g. Q96, Q128) is just a right shift —
        // far cheaper than the 64-bit-limb long division below.
        if rhs.is_power_of_two() {
            return self >> rhs.trailing_zeros();
        }
        U256::div_256_by_256(self, rhs)
    }
}

impl Rem for U256 {
    type Output = U256;
    fn rem(self, rhs: U256) -> U256 {
        let q = self / rhs;
        self - q * rhs
    }
}

impl Shl<u32> for U256 {
    type Output = U256;
    fn shl(self, rhs: u32) -> U256 {
        if rhs == 0 {
            return self;
        }
        if rhs >= 256 {
            return U256::ZERO;
        }
        if rhs >= 128 {
            return U256 {
                lo: 0,
                hi: self.lo << (rhs - 128),
            };
        }
        U256 {
            lo: self.lo << rhs,
            hi: (self.hi << rhs) | (self.lo >> (128 - rhs)),
        }
    }
}

impl Shr<u32> for U256 {
    type Output = U256;
    fn shr(self, rhs: u32) -> U256 {
        if rhs == 0 {
            return self;
        }
        if rhs >= 256 {
            return U256::ZERO;
        }
        if rhs >= 128 {
            return U256 {
                lo: self.hi >> (rhs - 128),
                hi: 0,
            };
        }
        U256 {
            lo: (self.lo >> rhs) | (self.hi << (128 - rhs)),
            hi: self.hi >> rhs,
        }
    }
}

impl BitAnd for U256 {
    type Output = U256;
    fn bitand(self, rhs: U256) -> U256 {
        U256 {
            lo: self.lo & rhs.lo,
            hi: self.hi & rhs.hi,
        }
    }
}

impl BitOr for U256 {
    type Output = U256;
    fn bitor(self, rhs: U256) -> U256 {
        U256 {
            lo: self.lo | rhs.lo,
            hi: self.hi | rhs.hi,
        }
    }
}

impl Not for U256 {
    type Output = U256;
    fn not(self) -> U256 {
        U256 {
            lo: !self.lo,
            hi: !self.hi,
        }
    }
}

impl From<u128> for U256 {
    fn from(v: u128) -> Self {
        U256::from_u128(v)
    }
}

impl From<u64> for U256 {
    fn from(v: u64) -> Self {
        U256::from_u64(v)
    }
}

impl From<u32> for U256 {
    fn from(v: u32) -> Self {
        U256::from_u128(v as u128)
    }
}

impl From<i32> for U256 {
    fn from(v: i32) -> Self {
        U256::from_u128(v as u128)
    }
}

// ═══════════════════════════  I256  ═══════════════════════════
// Two's complement view of U256

#[derive(Copy, Clone, Eq, PartialEq, Hash, Default)]
pub struct I256(pub U256);

impl I256 {
    pub const ZERO: I256 = I256(U256::ZERO);
    pub const ONE: I256 = I256(U256::ONE);
    pub const NEGATIVE_ONE: I256 = I256(U256::MAX); // twos complement -1

    pub fn is_negative(&self) -> bool {
        self.0.hi >> 127 == 1
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    pub fn is_positive(&self) -> bool {
        !self.is_negative() && !self.is_zero()
    }

    /// Negate (two's complement)
    pub fn negate(self) -> I256 {
        if self.is_zero() {
            return self;
        }
        I256((!self.0) + U256::ONE)
    }

    /// Absolute value as U256
    pub fn abs(self) -> U256 {
        if self.is_negative() {
            self.negate().0
        } else {
            self.0
        }
    }

    pub fn from_i64(v: i64) -> Self {
        if v >= 0 {
            I256(U256::from_u128(v as u128))
        } else {
            I256(U256::from_u128(v as u128)) // works because of 2's complement in u128 from i64
        }
    }

    pub fn from_i128(v: i128) -> Self {
        I256(U256::from_u128(v as u128))
    }

    /// Convert to U256 (just reinterpret)
    pub fn as_u256(self) -> U256 {
        self.0
    }

    /// Sign-extend i32 to I256
    pub fn from_i32(v: i32) -> Self {
        if v >= 0 {
            I256(U256::from_u128(v as u128))
        } else {
            // Sign extend: set all upper bits
            let lo = v as i128 as u128; // sign extends correctly
            I256(U256::new(lo, u128::MAX))
        }
    }

    /// To i32 (truncate)
    pub fn as_i32(&self) -> i32 {
        self.0.lo as i32
    }

    /// To i128
    pub fn to_i128(&self) -> i128 {
        self.0.lo as i128
    }

    /// Wrapping mul
    pub fn wrapping_mul(self, rhs: I256) -> I256 {
        I256(self.0.wrapping_mul(rhs.0))
    }

    pub fn to_dec_string(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        if self.is_negative() {
            format!("-{}", self.negate().0.to_dec_string())
        } else {
            self.0.to_dec_string()
        }
    }

    /// Serialize as 32-byte little-endian (two's complement)
    pub fn to_le_bytes(&self) -> [u8; 32] {
        self.0.to_le_bytes()
    }

    /// Deserialize from 32-byte little-endian (two's complement)
    pub fn from_le_bytes(bytes: &[u8]) -> Self {
        I256(U256::from_le_bytes(bytes))
    }
}

impl fmt::Debug for I256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "I256({})", self.to_dec_string())
    }
}

impl fmt::Display for I256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_dec_string())
    }
}

impl PartialOrd for I256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for I256 {
    fn cmp(&self, other: &Self) -> Ordering {
        let a_neg = self.is_negative();
        let b_neg = other.is_negative();
        match (a_neg, b_neg) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => self.0.cmp(&other.0), // same sign — raw comparison works
        }
    }
}

impl Add for I256 {
    type Output = I256;
    fn add(self, rhs: I256) -> I256 {
        I256(self.0 + rhs.0)
    }
}

impl Sub for I256 {
    type Output = I256;
    fn sub(self, rhs: I256) -> I256 {
        I256(self.0 - rhs.0)
    }
}

impl Mul for I256 {
    type Output = I256;
    fn mul(self, rhs: I256) -> I256 {
        I256(self.0.wrapping_mul(rhs.0))
    }
}

impl Neg for I256 {
    type Output = I256;
    fn neg(self) -> I256 {
        self.negate()
    }
}

// Signed division
impl Div for I256 {
    type Output = I256;
    fn div(self, rhs: I256) -> I256 {
        if self.is_zero() {
            return I256::ZERO;
        }
        let a_neg = self.is_negative();
        let b_neg = rhs.is_negative();
        let q = self.abs() / rhs.abs();
        if a_neg != b_neg {
            I256(q).negate()
        } else {
            I256(q)
        }
    }
}

impl From<i32> for I256 {
    fn from(v: i32) -> Self {
        I256::from_i32(v)
    }
}

impl From<U256> for I256 {
    fn from(v: U256) -> Self {
        I256(v)
    }
}

// ═══════════════════════════  Constants  ═══════════════════════════

pub const ZERO: U256 = U256::ZERO;
pub const ONE: U256 = U256::ONE;
pub const TWO: U256 = U256::new(2, 0);

pub const MAX_UINT128: U256 = U256::new(u128::MAX, 0);
pub const MAX_UINT160: U256 = U256::new(u128::MAX, (1u128 << 32) - 1);
pub const MAX_UINT256: U256 = U256::MAX;

pub const Q32: U256 = U256::new(1u128 << 32, 0);
pub const Q96: U256 = U256::new(1u128 << 96, 0);
pub const Q128: U256 = U256::new(0, 1);
pub const Q192: U256 = U256::new(0, 1u128 << 64); // 2^192 = 2^(128+64)

pub const MAX_FEE: U256 = U256::new(1_000_000, 0);

pub const MIN_TICK: i32 = -887272;
pub const MAX_TICK: i32 = 887272;

// MIN_SQRT_RATIO = 4295128739
pub const MIN_SQRT_RATIO: U256 = U256::new(4295128739, 0);
// MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342
// In hex: 0xFFFD8963EFD1FC6A506488495D951D5263988D26 (fits in 160 bits)
pub const MAX_SQRT_RATIO: U256 = U256::new(
    0xEFD1FC6A506488495D951D5263988D26u128,
    0xFFFD8963u128,
);

/// Fee amounts
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeAmount {
    ExtraLow = 100,
    Low = 500,
    Medium = 3000,
    High = 10000,
}

impl FeeAmount {
    pub fn tick_spacing(&self) -> i32 {
        match self {
            FeeAmount::ExtraLow => 1,
            FeeAmount::Low => 10,
            FeeAmount::Medium => 60,
            FeeAmount::High => 200,
        }
    }

    pub fn from_u32(v: u32) -> Self {
        match v {
            100 => FeeAmount::ExtraLow,
            500 => FeeAmount::Low,
            3000 => FeeAmount::Medium,
            10000 => FeeAmount::High,
            _ => panic!("Invalid fee amount: {}", v),
        }
    }

    pub fn as_u256(&self) -> U256 {
        U256::from_u128(*self as u128)
    }
}

// We need to define MAX_SQRT_RATIO properly. Let's compute it from the hex string
// 1461446703485210103287273052203988822378723970342
// Hex: 0xFFFD8963EFD1FC6A506488495D951D5263988D26
// This is 160 bits, so hi = upper 32 bits, lo = lower 128 bits
// 0xFFFD8963EFD1FC6A506488495D951D5263988D26
// hi32 = 0xFFFD8963E >> ... let me just hardcode it properly
// Actually, let's define it via from_hex_str in a lazy_static or const fn.
// Since we can't call from_hex_str in const, let's manually compute:
// 0xFFFD8963EFD1FC6A506488495D951D5263988D26
// Split at 128 bits (32 hex digits from the right):
// lo (32 hex) = 506488495D951D5263988D26 (only 24 hex = 96 bits, need to pad)
// Full hex: FFFD8963EFD1FC6A506488495D951D5263988D26
// 40 hex chars = 160 bits
// lo 128 bits = last 32 hex chars = 6A506488495D951D5263988D26000000 no...
// Let me split properly:
// hex = FFFD8963EFD1FC6A506488495D951D5263988D26
// That's 40 hex digits = 160 bits
// Lower 128 bits (32 hex digits from right):
// EFD1FC6A506488495D951D5263988D26
// Upper 32 bits (8 hex digits):
// FFFD8963

// We redefine via a function that gets called and replaces the placeholder:
pub fn max_sqrt_ratio() -> U256 {
    U256::new(
        0xEFD1FC6A506488495D951D5263988D26u128,
        0xFFFD8963u128,
    )
}

pub fn min_sqrt_ratio() -> U256 {
    U256::from_u128(4295128739u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u256_basic_ops() {
        let a = U256::from_u128(100);
        let b = U256::from_u128(200);
        assert_eq!((a + b).lo, 300);
        assert_eq!((b - a).lo, 100);
        assert_eq!((a * b).lo, 20000);
        assert_eq!((b / a).lo, 2);
    }

    #[test]
    fn test_u256_shifts() {
        let one = U256::ONE;
        let result = one << 128;
        assert_eq!(result, Q128);
        assert_eq!((result >> 128), U256::ONE);
    }

    #[test]
    fn test_i256_negation() {
        let one = I256::ONE;
        let neg_one = one.negate();
        assert!(neg_one.is_negative());
        assert_eq!(neg_one.negate(), one);
        assert_eq!((one + neg_one), I256::ZERO);
    }

    #[test]
    fn test_max_sqrt_ratio() {
        let v = max_sqrt_ratio();
        assert_eq!(
            v.to_dec_string(),
            "1461446703485210103287273052203988822378723970342"
        );
    }

    #[test]
    fn test_from_dec_str() {
        let v = U256::from_dec_str("1461446703485210103287273052203988822378723970342");
        assert_eq!(v, max_sqrt_ratio());
    }
}
