//! JavaScript values and coercions.

use std::fmt;

use crate::bigint::BigInt;

/// Identifier of a heap allocated object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub usize);

/// A JavaScript value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The `undefined` value.
    Undefined,
    /// The `null` value.
    Null,
    /// A boolean.
    Bool(bool),
    /// A double precision floating point number.
    Number(f64),
    /// An arbitrary precision integer. Boxed to keep `Value` small; the
    /// interpreter's recursive frames depend on it.
    BigInt(Box<BigInt>),
    /// A string.
    String(String),
    /// A heap allocated object.
    Object(ObjectId),
}

impl Value {
    /// JavaScript truthiness.
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::BigInt(value) => !value.is_zero(),
            Self::String(value) => !value.is_empty(),
            Self::Object(_) => true,
        }
    }

    /// JavaScript `ToNumber`, without invoking user code for objects.
    pub fn to_number(&self) -> f64 {
        match self {
            Self::Undefined => f64::NAN,
            Self::Null => 0.0,
            Self::Bool(value) => {
                if *value {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Number(value) => *value,
            Self::BigInt(value) => value.to_f64(),
            Self::String(value) => string_to_number(value),
            Self::Object(_) => f64::NAN,
        }
    }

    /// JavaScript `ToPrimitive` for primitives and a simple fallback for objects.
    pub fn to_string_value(&self) -> String {
        match self {
            Self::Undefined => "undefined".to_owned(),
            Self::Null => "null".to_owned(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) => number_to_string(*value),
            Self::BigInt(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Object(_) => "[object Object]".to_owned(),
        }
    }

    /// The result of the `typeof` operator. Objects report `object`; the
    /// interpreter refines this for functions.
    pub fn type_of(&self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Null => "object",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::BigInt(_) => "bigint",
            Self::String(_) => "string",
            Self::Object(_) => "object",
        }
    }

    /// JavaScript strict equality (`===`).
    pub fn strict_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::BigInt(a), Self::BigInt(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => a == b,
            _ => false,
        }
    }

    /// JavaScript loose equality (`==`) for the cases that do not need the runtime.
    pub fn loose_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Self::Undefined | Self::Null, Self::Undefined | Self::Null) => true,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::BigInt(a), Self::BigInt(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Bool(_), _) | (_, Self::Bool(_)) => self.to_number() == other.to_number(),
            (Self::Number(a), Self::String(b)) => *a == string_to_number(b),
            (Self::String(a), Self::Number(b)) => string_to_number(a) == *b,
            (Self::Object(a), Self::Object(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_value())
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<BigInt> for Value {
    fn from(value: BigInt) -> Self {
        Self::BigInt(Box::new(value))
    }
}

/// Format a number the way JavaScript's `String(number)` does for the common cases.
pub fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    let text = format!("{value}");
    // Rust prints exponent-less forms where JavaScript would use exponent notation
    // for very small and very large magnitudes; normalise the common boundaries.
    if value.abs() >= 1e21 || (value.abs() < 1e-6 && value.fract() != 0.0) {
        let formatted = format!("{value:e}");
        if formatted.contains('e') {
            return formatted;
        }
    }
    text
}

/// Convert a string to a number using JavaScript `Number` semantics for primitives.
pub fn string_to_number(text: &str) -> f64 {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    let (sign, rest) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if rest.eq_ignore_ascii_case("Infinity") {
        return sign * f64::INFINITY;
    }
    let lower = rest.to_ascii_lowercase();
    let radix_value = lower
        .strip_prefix("0x")
        .map(|digits| (digits, 16))
        .or_else(|| lower.strip_prefix("0b").map(|digits| (digits, 2)))
        .or_else(|| lower.strip_prefix("0o").map(|digits| (digits, 8)));
    if let Some((digits, radix)) = radix_value {
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        return match u64::from_str_radix(digits, radix) {
            Ok(number) => sign * number as f64,
            Err(_) => f64::NAN,
        };
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_matches_javascript() {
        assert!(!Value::Undefined.is_truthy());
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Number(0.0).is_truthy());
        assert!(!Value::Number(f64::NAN).is_truthy());
        assert!(!Value::String(String::new()).is_truthy());
        assert!(Value::String("0".to_owned()).is_truthy());
        assert!(Value::Object(ObjectId(0)).is_truthy());
    }

    #[test]
    fn converts_strings_to_numbers() {
        assert_eq!(Value::String("42".to_owned()).to_number(), 42.0);
        assert_eq!(Value::String("".to_owned()).to_number(), 0.0);
        assert_eq!(Value::String("0x10".to_owned()).to_number(), 16.0);
        assert_eq!(Value::String("  3.5 ".to_owned()).to_number(), 3.5);
        assert!(Value::String("nope".to_owned()).to_number().is_nan());
        assert_eq!(Value::Null.to_number(), 0.0);
        assert!(Value::Undefined.to_number().is_nan());
    }

    #[test]
    fn formats_numbers_like_javascript() {
        assert_eq!(number_to_string(1.0), "1");
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(1.5), "1.5");
        assert_eq!(number_to_string(f64::NAN), "NaN");
        assert_eq!(number_to_string(f64::INFINITY), "Infinity");
    }

    #[test]
    fn bigint_value_semantics() {
        let zero = Value::BigInt(Box::new(BigInt::ZERO));
        let one = Value::BigInt(Box::new(BigInt::one()));
        assert!(!zero.is_truthy());
        assert!(one.is_truthy());
        assert_eq!(one.type_of(), "bigint");
        assert_eq!(one.to_string_value(), "1");
        assert!(one.strict_equals(&Value::BigInt(Box::new(BigInt::one()))));
        assert!(!one.strict_equals(&Value::Number(1.0)));
        assert!(!one.loose_equals(&Value::Number(1.0)));
    }

    #[test]
    fn strict_and_loose_equality() {
        assert!(Value::Number(1.0).strict_equals(&Value::Number(1.0)));
        assert!(!Value::Number(1.0).strict_equals(&Value::String("1".to_owned())));
        assert!(Value::Number(1.0).loose_equals(&Value::String("1".to_owned())));
        assert!(Value::Null.loose_equals(&Value::Undefined));
        assert!(!Value::Null.strict_equals(&Value::Undefined));
    }
}
