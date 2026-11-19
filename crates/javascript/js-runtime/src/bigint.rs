//! Arbitrary precision integers backing the JavaScript `BigInt` primitive.
//!
//! Values are stored as a sign plus a little-endian vector of base 2^32
//! limbs. All the operators needed by the interpreter are implemented here so
//! that the engine has no dependency on an external bignum crate. This module
//! is deliberately independent of `Runtime`: the interpreter translates the
//! errors below into JavaScript `RangeError`/`SyntaxError` values.

use std::cmp::Ordering;
use std::fmt;

/// Upper bound on the size of a single BigInt, in bits.
///
/// ECMAScript BigInts are unbounded in principle. To keep script evaluation
/// bounded (the engine also limits execution steps and call depth) operations
/// that would produce a value wider than this fail with a `RangeError`.
pub const MAX_BITS: usize = 1_000_000;

/// Errors raised by BigInt operations, mapped to JavaScript errors by callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BigIntError {
    /// A non-integral or non-finite number cannot be converted to a BigInt.
    NonInteger,
    /// The string does not contain a valid integer literal.
    InvalidString,
    /// Division or remainder by zero.
    DivisionByZero,
    /// A negative exponent was supplied to `**`.
    NegativeExponent,
    /// A negative shift count or a bit width outside `0..=MAX_BITS`.
    NegativeShift,
    /// The result would exceed [`MAX_BITS`] bits.
    LimitExceeded,
    /// `toString` was called with a radix outside `2..=36`.
    InvalidRadix,
}

impl fmt::Display for BigIntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NonInteger => {
                "The number cannot be converted to a BigInt because it is not an integer"
            }
            Self::InvalidString => "Cannot convert the string to a BigInt",
            Self::DivisionByZero => "Division by zero",
            Self::NegativeExponent => "BigInt exponent must be non-negative",
            Self::NegativeShift => "BigInt shift count must be non-negative",
            Self::LimitExceeded => "BigInt result exceeds the maximum supported size",
            Self::InvalidRadix => "toString() radix must be between 2 and 36",
        };
        f.write_str(message)
    }
}

impl std::error::Error for BigIntError {}

/// A signed arbitrary precision integer.
///
/// Zero is always stored as a positive magnitude with no limbs, which makes
/// equality and sign checks straightforward.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BigInt {
    negative: bool,
    /// Little-endian base 2^32 limbs with no trailing zeros.
    digits: Vec<u32>,
}

impl Default for BigInt {
    fn default() -> Self {
        Self::ZERO
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_radix(10))
    }
}

fn trim_zeros(digits: &mut Vec<u32>) {
    while digits.last() == Some(&0) {
        digits.pop();
    }
}

fn bit_length(digits: &[u32]) -> usize {
    match digits.last() {
        Some(&last) => (digits.len() - 1) * 32 + (32 - last.leading_zeros() as usize),
        None => 0,
    }
}

fn cmp_mag(left: &[u32], right: &[u32]) -> Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.iter().rev().cmp(right.iter().rev()))
}

fn add_mag(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(left.len().max(right.len()) + 1);
    let mut carry = 0u64;
    for index in 0..left.len().max(right.len()) {
        let a = u64::from(*left.get(index).unwrap_or(&0));
        let b = u64::from(*right.get(index).unwrap_or(&0));
        let sum = a + b + carry;
        result.push(sum as u32);
        carry = sum >> 32;
    }
    if carry != 0 {
        result.push(carry as u32);
    }
    result
}

/// Subtract `right` from `left`. `left` must be greater than or equal to `right`.
fn sub_mag(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(left.len());
    let mut borrow = 0i64;
    for index in 0..left.len() {
        let a = i64::from(*left.get(index).unwrap_or(&0));
        let b = i64::from(*right.get(index).unwrap_or(&0));
        let mut value = a - b - borrow;
        if value < 0 {
            value += 1i64 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        result.push(value as u32);
    }
    trim_zeros(&mut result);
    result
}

fn mul_mag(left: &[u32], right: &[u32]) -> Vec<u32> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    let mut result = vec![0u32; left.len() + right.len()];
    for (i, &a) in left.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &b) in right.iter().enumerate() {
            let current = u64::from(result[i + j]) + u64::from(a) * u64::from(b) + carry;
            result[i + j] = current as u32;
            carry = current >> 32;
        }
        let mut index = i + right.len();
        while carry != 0 {
            let current = u64::from(result[index]) + carry;
            result[index] = current as u32;
            carry = current >> 32;
            index += 1;
        }
    }
    trim_zeros(&mut result);
    result
}

fn mul_small(digits: &[u32], factor: u32) -> Vec<u32> {
    let mut result = Vec::with_capacity(digits.len() + 1);
    let mut carry = 0u64;
    for &digit in digits {
        let value = u64::from(digit) * u64::from(factor) + carry;
        result.push(value as u32);
        carry = value >> 32;
    }
    if carry != 0 {
        result.push(carry as u32);
    }
    trim_zeros(&mut result);
    result
}

/// Long division of magnitudes. `divisor` must be non-zero.
fn div_rem_mag(dividend: &[u32], divisor: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut quotient = vec![0u32; dividend.len()];
    let mut remainder = Vec::new();
    for index in (0..dividend.len()).rev() {
        remainder.insert(0, dividend[index]);
        if cmp_mag(&remainder, divisor) == Ordering::Less {
            continue;
        }
        // Find the largest limb `digit` such that `digit * divisor` fits in
        // the running remainder.
        let mut low = 0u32;
        let mut high = u32::MAX;
        while low < high {
            let mid = (u64::from(low) + u64::from(high)).div_ceil(2) as u32;
            if cmp_mag(&mul_small(divisor, mid), &remainder) != Ordering::Greater {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        quotient[index] = low;
        remainder = sub_mag(&remainder, &mul_small(divisor, low));
    }
    trim_zeros(&mut quotient);
    trim_zeros(&mut remainder);
    (quotient, remainder)
}

fn shl_mag(digits: &[u32], bits: usize) -> Vec<u32> {
    if digits.is_empty() || bits == 0 {
        return digits.to_vec();
    }
    let limb_shift = bits / 32;
    let bit_shift = bits % 32;
    let mut result = vec![0u32; digits.len() + limb_shift + usize::from(bit_shift != 0)];
    for (index, &digit) in digits.iter().enumerate() {
        let value = (u64::from(digit)) << bit_shift;
        result[index + limb_shift] |= value as u32;
        if bit_shift != 0 {
            result[index + limb_shift + 1] |= (value >> 32) as u32;
        }
    }
    trim_zeros(&mut result);
    result
}

fn shr_mag(digits: &[u32], bits: usize) -> Vec<u32> {
    if digits.is_empty() || bits == 0 {
        return digits.to_vec();
    }
    let limb_shift = bits / 32;
    if limb_shift >= digits.len() {
        return Vec::new();
    }
    let bit_shift = bits % 32;
    let mut result = digits[limb_shift..].to_vec();
    if bit_shift != 0 {
        for index in 0..result.len() {
            let high = if index + 1 < result.len() {
                u64::from(result[index + 1])
            } else {
                0
            };
            result[index] =
                ((u64::from(result[index]) >> bit_shift) | (high << (32 - bit_shift))) as u32;
        }
    }
    trim_zeros(&mut result);
    result
}

fn ensure_bits(digits: &[u32]) -> Result<(), BigIntError> {
    if bit_length(digits) > MAX_BITS {
        Err(BigIntError::LimitExceeded)
    } else {
        Ok(())
    }
}

fn check_shift(bits: u64) -> Result<usize, BigIntError> {
    if bits > MAX_BITS as u64 {
        Err(BigIntError::LimitExceeded)
    } else {
        Ok(bits as usize)
    }
}

impl BigInt {
    /// The integer zero.
    pub const ZERO: Self = Self {
        negative: false,
        digits: Vec::new(),
    };
    /// The integer one.
    pub fn one() -> Self {
        Self {
            negative: false,
            digits: vec![1u32],
        }
    }

    /// Create a BigInt from an `i64`.
    pub fn from_i64(value: i64) -> Self {
        if value == 0 {
            return Self::ZERO;
        }
        let magnitude = value.unsigned_abs();
        Self {
            negative: value < 0,
            digits: vec![magnitude as u32, (magnitude >> 32) as u32],
        }
        .normalized()
    }

    /// Create a BigInt from a `u64`.
    pub fn from_u64(value: u64) -> Self {
        if value == 0 {
            return Self::ZERO;
        }
        Self {
            negative: false,
            digits: vec![value as u32, (value >> 32) as u32],
        }
        .normalized()
    }

    fn normalized(mut self) -> Self {
        trim_zeros(&mut self.digits);
        if self.digits.is_empty() {
            self.negative = false;
        }
        self
    }

    /// Convert a finite integral Number to a BigInt.
    pub fn from_f64(value: f64) -> Result<Self, BigIntError> {
        if !value.is_finite() || value.fract() != 0.0 {
            return Err(BigIntError::NonInteger);
        }
        if value == 0.0 {
            return Ok(Self::ZERO);
        }
        let bits = value.to_bits();
        let negative = bits >> 63 != 0;
        let exponent = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1u64 << 52) - 1);
        let (mantissa, power) = if exponent == 0 {
            (fraction, -1074)
        } else {
            ((1u64 << 52) | fraction, exponent - 1023 - 52)
        };
        let mut result = Self::from_u64(mantissa);
        if power > 0 {
            result.digits = shl_mag(&result.digits, check_shift(power as u64)?);
        } else if power < 0 {
            result.digits = shr_mag(&result.digits, (-power) as usize);
        }
        result.negative = negative && !result.digits.is_empty();
        ensure_bits(&result.digits)?;
        Ok(result)
    }

    /// Parse an integer string using the given radix (2..=36).
    pub fn from_str_radix(text: &str, radix: u32) -> Result<Self, BigIntError> {
        if !(2..=36).contains(&radix) {
            return Err(BigIntError::InvalidRadix);
        }
        if text.is_empty() {
            return Err(BigIntError::InvalidString);
        }
        let (negative, digits) = match text.as_bytes()[0] {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        if digits.is_empty() {
            return Err(BigIntError::InvalidString);
        }
        let mut limbs = Vec::from(&[0u32][..]);
        for ch in digits.chars() {
            let digit = ch.to_digit(radix).ok_or(BigIntError::InvalidString)?;
            let mut carry = u64::from(digit);
            for limb in &mut limbs {
                let value = u64::from(*limb) * u64::from(radix) + carry;
                *limb = value as u32;
                carry = value >> 32;
            }
            if carry != 0 {
                limbs.push(carry as u32);
            }
            ensure_bits(&limbs)?;
        }
        let mut result = Self {
            negative,
            digits: limbs,
        }
        .normalized();
        if result.digits.is_empty() {
            result.negative = false;
        }
        Ok(result)
    }

    /// Parse a decimal string, allowing `0x`, `0b` and `0o` prefixes.
    pub fn parse_string(text: &str) -> Result<Self, BigIntError> {
        let text = text.trim();
        let (negative, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let (radix, digits) =
            if let Some(rest) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
                (16, rest)
            } else if let Some(rest) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
                (2, rest)
            } else if let Some(rest) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
                (8, rest)
            } else {
                (10, rest)
            };
        if digits.is_empty() {
            return Err(BigIntError::InvalidString);
        }
        let sign = if negative { "-" } else { "" };
        Self::from_str_radix(&format!("{sign}{digits}"), radix)
    }

    /// Whether the value is zero.
    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// Whether the value is negative.
    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// Number of bits needed for the magnitude.
    pub fn bit_length(&self) -> usize {
        bit_length(&self.digits)
    }

    fn required_bits(&self) -> usize {
        self.bit_length() + usize::from(self.negative)
    }

    /// Convert to `f64`, rounding when the value is too large to represent.
    pub fn to_f64(&self) -> f64 {
        let mut value = 0.0f64;
        for &digit in self.digits.iter().rev() {
            value = value * 4294967296.0 + f64::from(digit);
        }
        if self.negative { -value } else { value }
    }

    /// Convert to `u64` when the value is a non-negative integer that fits.
    pub fn to_u64(&self) -> Option<u64> {
        if self.negative || self.digits.len() > 2 {
            return None;
        }
        let low = self.digits.first().copied().unwrap_or(0) as u64;
        let high = self.digits.get(1).copied().unwrap_or(0) as u64;
        Some(low | (high << 32))
    }

    /// Render the value in radix 2 through 36.
    pub fn to_string_radix(&self, radix: u32) -> String {
        if !(2..=36).contains(&radix) {
            return "0".to_owned();
        }
        if self.is_zero() {
            return "0".to_owned();
        }
        let mut digits = self.digits.clone();
        let divisor = u64::from(radix);
        let mut characters = Vec::new();
        while !digits.is_empty() {
            let mut remainder = 0u64;
            for digit in digits.iter_mut().rev() {
                let current = (remainder << 32) | u64::from(*digit);
                *digit = (current / divisor) as u32;
                remainder = current % divisor;
            }
            characters.push(char::from_digit(remainder as u32, radix).unwrap_or('0'));
            trim_zeros(&mut digits);
        }
        if self.negative {
            characters.push('-');
        }
        characters.iter().rev().collect()
    }

    /// Ordering between this BigInt and a Number, exact for integral Numbers.
    pub fn cmp_with_f64(&self, other: f64) -> Option<Ordering> {
        if other.is_nan() {
            return None;
        }
        if other == 0.0 {
            return Some(match self.is_zero() {
                true => Ordering::Equal,
                false if self.negative => Ordering::Less,
                false => Ordering::Greater,
            });
        }
        if other.is_infinite() {
            return Some(if other > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        if self.is_zero() {
            return Some(if other > 0.0 {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        if self.negative != (other < 0.0) {
            return Some(if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        let magnitude = other.abs().to_bits();
        let exponent = ((magnitude >> 52) & 0x7ff) as i32;
        let fraction = magnitude & ((1u64 << 52) - 1);
        let (mantissa, power) = if exponent == 0 {
            (fraction, -1074)
        } else {
            ((1u64 << 52) | fraction, exponent - 1023 - 52)
        };
        let ordering = if power >= 0 {
            let shifted = shl_mag(&self.digits, power as usize);
            cmp_mag(&shifted, &[mantissa as u32, (mantissa >> 32) as u32])
        } else if -power >= 64 {
            Ordering::Greater
        } else {
            let shifted = shl_mag(&self.digits, (-power) as usize);
            cmp_mag(&shifted, &[mantissa as u32, (mantissa >> 32) as u32])
        };
        Some(if self.negative {
            ordering.reverse()
        } else {
            ordering
        })
    }

    /// The negation of this value.
    pub fn neg(&self) -> Self {
        if self.is_zero() {
            Self::ZERO
        } else {
            Self {
                negative: !self.negative,
                digits: self.digits.clone(),
            }
        }
    }

    /// Absolute value.
    pub fn abs(&self) -> Self {
        Self {
            negative: false,
            digits: self.digits.clone(),
        }
    }

    /// Addition.
    pub fn add(&self, other: &Self) -> Result<Self, BigIntError> {
        let digits = if self.negative == other.negative {
            add_mag(&self.digits, &other.digits)
        } else {
            match cmp_mag(&self.digits, &other.digits) {
                Ordering::Less => sub_mag(&other.digits, &self.digits),
                Ordering::Greater => sub_mag(&self.digits, &other.digits),
                Ordering::Equal => Vec::new(),
            }
        };
        let negative = if self.negative == other.negative {
            self.negative
        } else {
            match cmp_mag(&self.digits, &other.digits) {
                Ordering::Less => other.negative,
                _ => self.negative,
            }
        };
        ensure_bits(&digits)?;
        Ok(Self { negative, digits }.normalized())
    }

    /// Subtraction.
    pub fn sub(&self, other: &Self) -> Result<Self, BigIntError> {
        self.add(&other.neg())
    }

    /// Multiplication.
    pub fn mul(&self, other: &Self) -> Result<Self, BigIntError> {
        let digits = mul_mag(&self.digits, &other.digits);
        ensure_bits(&digits)?;
        Ok(Self {
            negative: self.negative != other.negative,
            digits,
        }
        .normalized())
    }

    /// Integer division, truncated toward zero.
    pub fn div(&self, other: &Self) -> Result<Self, BigIntError> {
        self.div_rem(other).map(|(quotient, _)| quotient)
    }

    /// Remainder, with the sign of the dividend.
    pub fn rem(&self, other: &Self) -> Result<Self, BigIntError> {
        self.div_rem(other).map(|(_, remainder)| remainder)
    }

    fn div_rem(&self, other: &Self) -> Result<(Self, Self), BigIntError> {
        if other.is_zero() {
            return Err(BigIntError::DivisionByZero);
        }
        let (quotient_digits, remainder_digits) = div_rem_mag(&self.digits, &other.digits);
        Ok((
            Self {
                negative: self.negative != other.negative,
                digits: quotient_digits,
            }
            .normalized(),
            Self {
                negative: self.negative,
                digits: remainder_digits,
            }
            .normalized(),
        ))
    }

    /// Exponentiation by a non-negative BigInt exponent.
    pub fn pow(&self, exponent: &Self) -> Result<Self, BigIntError> {
        if exponent.negative {
            return Err(BigIntError::NegativeExponent);
        }
        let mut remaining = exponent.to_u64().ok_or(BigIntError::LimitExceeded)?;
        if remaining > MAX_BITS as u64 {
            return Err(BigIntError::LimitExceeded);
        }
        let mut base = self.clone();
        let mut result = Self::one();
        while remaining > 0 {
            if remaining & 1 == 1 {
                result = result.mul(&base)?;
            }
            remaining >>= 1;
            if remaining > 0 {
                base = base.mul(&base)?;
            }
        }
        Ok(result)
    }

    fn twos_complement(&self, width: usize) -> Vec<u32> {
        let limbs = width.div_ceil(32);
        let mut value = self.digits.clone();
        value.resize(limbs, 0);
        if self.negative {
            for digit in &mut value {
                *digit = !*digit;
            }
            let mut carry = 1u64;
            for digit in &mut value {
                let current = u64::from(*digit) + carry;
                *digit = current as u32;
                carry = current >> 32;
                if carry == 0 {
                    break;
                }
            }
        }
        value
    }

    fn from_twos_complement(value: Vec<u32>, width: usize) -> Self {
        let negative = width > 0
            && value
                .get((width - 1) / 32)
                .is_some_and(|digit| digit >> ((width - 1) % 32) & 1 == 1);
        if negative {
            let mut digits = value;
            for digit in &mut digits {
                *digit = !*digit;
            }
            let mut carry = 1u64;
            for digit in &mut digits {
                let current = u64::from(*digit) + carry;
                *digit = current as u32;
                carry = current >> 32;
                if carry == 0 {
                    break;
                }
            }
            Self {
                negative: true,
                digits,
            }
            .normalized()
        } else {
            let mut digits = value;
            trim_zeros(&mut digits);
            Self {
                negative: false,
                digits,
            }
        }
    }

    /// Bitwise AND, using two's complement semantics.
    pub fn bitand(&self, other: &Self) -> Self {
        let width = self.required_bits().max(other.required_bits()) + 1;
        let left = self.twos_complement(width);
        let right = other.twos_complement(width);
        let value = left.iter().zip(right.iter()).map(|(a, b)| a & b).collect();
        Self::from_twos_complement(value, width)
    }

    /// Bitwise OR, using two's complement semantics.
    pub fn bitor(&self, other: &Self) -> Self {
        let width = self.required_bits().max(other.required_bits()) + 1;
        let left = self.twos_complement(width);
        let right = other.twos_complement(width);
        let value = left.iter().zip(right.iter()).map(|(a, b)| a | b).collect();
        Self::from_twos_complement(value, width)
    }

    /// Bitwise XOR, using two's complement semantics.
    pub fn bitxor(&self, other: &Self) -> Self {
        let width = self.required_bits().max(other.required_bits()) + 1;
        let left = self.twos_complement(width);
        let right = other.twos_complement(width);
        let value = left.iter().zip(right.iter()).map(|(a, b)| a ^ b).collect();
        Self::from_twos_complement(value, width)
    }

    /// Bitwise NOT, using two's complement semantics.
    pub fn not(&self) -> Self {
        let width = self.required_bits() + 1;
        let value = self
            .twos_complement(width)
            .into_iter()
            .map(|digit| !digit)
            .collect();
        Self::from_twos_complement(value, width)
    }

    /// Left shift by a non-negative number of bits.
    pub fn shl(&self, bits: u64) -> Result<Self, BigIntError> {
        let bits = check_shift(bits)?;
        let digits = shl_mag(&self.digits, bits);
        ensure_bits(&digits)?;
        Ok(Self {
            negative: self.negative,
            digits,
        }
        .normalized())
    }

    /// Arithmetic (sign propagating) right shift by a non-negative number of bits.
    pub fn shr(&self, bits: u64) -> Result<Self, BigIntError> {
        let bits = check_shift(bits)?;
        if self.negative && !self.is_zero() {
            // floor(-x / 2^bits) == -((x + 2^bits - 1) >> bits)
            let rounded_up = Self {
                negative: false,
                digits: add_mag(&self.digits, &shl_mag(&Self::one().digits, bits)),
            }
            .normalized();
            let shifted = shr_mag(&rounded_up.digits, bits);
            Ok(Self {
                negative: true,
                digits: shifted,
            }
            .normalized())
        } else {
            Ok(Self {
                negative: false,
                digits: shr_mag(&self.digits, bits),
            }
            .normalized())
        }
    }

    /// Truncate to the low `bits` bits, interpreting them as unsigned.
    pub fn as_uint_n(&self, bits: u64) -> Result<Self, BigIntError> {
        let bits = check_shift(bits)?;
        if bits == 0 || self.is_zero() {
            return Ok(Self::ZERO);
        }
        let whole = bits / 32;
        let remainder = bits % 32;
        let mut digits = self.digits.clone();
        if remainder == 0 {
            digits.truncate(whole);
        } else {
            if digits.len() > whole {
                digits[whole] &= (1u32 << remainder) - 1;
            }
            digits.truncate(whole + 1);
        }
        trim_zeros(&mut digits);
        if self.negative && !digits.is_empty() {
            // Two's complement modulo 2^bits: 2^bits - magnitude.
            let modulus = Self::one().shl(bits as u64)?;
            modulus.sub(&Self {
                negative: false,
                digits,
            })
        } else {
            Ok(Self {
                negative: false,
                digits,
            })
        }
    }

    /// Truncate to the low `bits` bits, interpreting them as signed.
    pub fn as_int_n(&self, bits: u64) -> Result<Self, BigIntError> {
        if bits == 0 {
            return Ok(Self::ZERO);
        }
        let width = check_shift(bits)?;
        let unsigned = self.as_uint_n(bits)?;
        let sign_index = (width - 1) / 32;
        let sign_bit = unsigned
            .digits
            .get(sign_index)
            .is_some_and(|digit| digit >> ((width - 1) % 32) & 1 == 1);
        if sign_bit {
            let modulus = Self::one().shl(bits)?;
            unsigned.sub(&modulus)
        } else {
            Ok(unsigned)
        }
    }
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => {
                return if other.negative {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (false, true) => {
                return if self.negative {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (false, false) => {}
        }
        match (self.negative, other.negative) {
            (false, false) => cmp_mag(&self.digits, &other.digits),
            (true, true) => cmp_mag(&other.digits, &self.digits),
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(text: &str) -> BigInt {
        BigInt::from_str_radix(text, 10).unwrap()
    }

    #[test]
    fn parses_and_renders_decimal_and_prefixed_strings() {
        assert_eq!(
            big("123456789012345678901234567890").to_string(),
            "123456789012345678901234567890"
        );
        assert_eq!(big("-42").to_string(), "-42");
        assert_eq!(big("+7").to_string(), "7");
        assert_eq!(BigInt::parse_string("0xff").unwrap().to_string(), "255");
        assert_eq!(BigInt::parse_string("-0b1010").unwrap().to_string(), "-10");
        assert_eq!(BigInt::parse_string("  0o17 ").unwrap().to_string(), "15");
        assert_eq!(BigInt::parse_string("01").unwrap().to_string(), "1");
        assert!(BigInt::parse_string("12n").is_err());
        assert!(BigInt::parse_string("").is_err());
    }

    #[test]
    fn converts_f64_integers() {
        assert_eq!(BigInt::from_f64(42.0).unwrap().to_string(), "42");
        assert_eq!(BigInt::from_f64(-0.0).unwrap().to_string(), "0");
        assert_eq!(
            BigInt::from_f64(1e30).unwrap().to_string(),
            "1000000000000000019884624838656"
        );
        assert!(BigInt::from_f64(1.5).is_err());
        assert!(BigInt::from_f64(f64::NAN).is_err());
        assert!(BigInt::from_f64(f64::INFINITY).is_err());
    }

    #[test]
    fn arithmetic_handles_signs_and_large_values() {
        let a = big("9007199254740993");
        let b = big("987654321098765432109876543210");
        assert_eq!(a.add(&big("7")).unwrap().to_string(), "9007199254741000");
        assert_eq!(big("5").sub(&big("12")).unwrap().to_string(), "-7");
        assert_eq!(big("-5").sub(&big("-12")).unwrap().to_string(), "7");
        assert_eq!(big("-123").mul(&big("456")).unwrap().to_string(), "-56088");
        assert_eq!(b.mul(&big("0")).unwrap().to_string(), "0");
        assert_eq!(b.add(&big("1")).unwrap().sub(&b).unwrap().to_string(), "1");
    }

    #[test]
    fn division_truncates_toward_zero_and_remainder_keeps_sign() {
        assert_eq!(big("7").div(&big("2")).unwrap().to_string(), "3");
        assert_eq!(big("-7").div(&big("2")).unwrap().to_string(), "-3");
        assert_eq!(big("7").div(&big("-2")).unwrap().to_string(), "-3");
        assert_eq!(big("-7").rem(&big("2")).unwrap().to_string(), "-1");
        assert_eq!(big("7").rem(&big("-2")).unwrap().to_string(), "1");
        assert_eq!(
            big("100000000000000000000")
                .div(&big("10000000000000000000"))
                .unwrap()
                .to_string(),
            "10"
        );
        assert!(big("1").div(&BigInt::ZERO).is_err());
    }

    #[test]
    fn exponentiation_and_shift() {
        assert_eq!(
            big("2").pow(&big("100")).unwrap().to_string(),
            "1267650600228229401496703205376"
        );
        assert_eq!(BigInt::ZERO.pow(&BigInt::ZERO).unwrap().to_string(), "1");
        assert_eq!(big("-3").pow(&big("3")).unwrap().to_string(), "-27");
        assert!(big("2").pow(&big("-1")).is_err());
        assert_eq!(
            big("1").shl(130).unwrap(),
            big("2").pow(&big("130")).unwrap()
        );
        assert_eq!(big("-7").shr(1).unwrap().to_string(), "-4");
        assert_eq!(big("7").shr(1).unwrap().to_string(), "3");
    }

    #[test]
    fn bitwise_uses_twos_complement() {
        assert_eq!(big("-1").bitand(&big("5")).to_string(), "5");
        assert_eq!(big("-1").bitor(&big("5")).to_string(), "-1");
        assert_eq!(big("-1").bitxor(&big("5")).to_string(), "-6");
        assert_eq!(big("0").not().to_string(), "-1");
        assert_eq!(big("5").not().to_string(), "-6");
        assert_eq!(big("-6").not().to_string(), "5");
    }

    #[test]
    fn wrapping_matches_as_int_n_and_as_uint_n() {
        assert_eq!(big("-1").as_uint_n(8).unwrap().to_string(), "255");
        assert_eq!(big("-1").as_int_n(8).unwrap().to_string(), "-1");
        assert_eq!(big("255").as_int_n(8).unwrap().to_string(), "-1");
        assert_eq!(big("256").as_uint_n(8).unwrap().to_string(), "0");
        assert_eq!(big("123456789").as_uint_n(0).unwrap().to_string(), "0");
    }

    #[test]
    fn radix_round_trip() {
        for radix in 2..=36 {
            let value = big("12345678901234567890").mul(&big("987654321")).unwrap();
            assert_eq!(
                BigInt::from_str_radix(&value.to_string_radix(radix), radix).unwrap(),
                value
            );
        }
        assert_eq!(big("255").to_string_radix(16), "ff");
        assert_eq!(big("-255").to_string_radix(2), "-11111111");
    }

    #[test]
    fn random_small_values_match_i128_semantics() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 32) as i64
        };
        for _ in 0..500 {
            let a = next() % 1_000_000;
            let b = (next() % 1_000_000).max(1);
            let (left, right) = (BigInt::from_i64(a), BigInt::from_i64(b));
            assert_eq!(
                left.add(&right).unwrap().to_string(),
                (a as i128 + b as i128).to_string()
            );
            assert_eq!(
                left.sub(&right).unwrap().to_string(),
                (a as i128 - b as i128).to_string()
            );
            assert_eq!(
                left.mul(&right).unwrap().to_string(),
                (a as i128 * b as i128).to_string()
            );
            assert_eq!(
                left.div(&right).unwrap().to_string(),
                (a as i128 / b as i128).to_string()
            );
            assert_eq!(
                left.rem(&right).unwrap().to_string(),
                (a as i128 % b as i128).to_string()
            );
            assert_eq!(left.bitand(&right).to_string(), (a & b).to_string());
            assert_eq!(left.bitor(&right).to_string(), (a | b).to_string());
            assert_eq!(left.bitxor(&right).to_string(), (a ^ b).to_string());
            let shift = (next() % 60).unsigned_abs();
            assert_eq!(
                left.shl(shift).unwrap().to_string(),
                ((a as i128) << shift).to_string()
            );
            assert_eq!(
                left.shr(shift).unwrap().to_string(),
                (a >> shift).to_string()
            );
        }
    }

    #[test]
    fn compares_exactly_with_numbers() {
        assert_eq!(
            big("9007199254740993").cmp_with_f64(9007199254740992.0),
            Some(Ordering::Greater)
        );
        assert_eq!(big("1").cmp_with_f64(1.0), Some(Ordering::Equal));
        assert_eq!(big("-1").cmp_with_f64(1.0), Some(Ordering::Less));
        assert_eq!(big("0").cmp_with_f64(-0.5), Some(Ordering::Greater));
        assert_eq!(big("1").cmp_with_f64(f64::NAN), None);
        assert_eq!(big("2").cmp_with_f64(f64::INFINITY), Some(Ordering::Less));
    }
}
