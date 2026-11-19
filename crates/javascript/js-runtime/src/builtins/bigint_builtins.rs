//! The `BigInt` constructor, its static helpers and prototype methods.

use crate::Runtime;
use crate::bigint::{BigInt, BigIntError, MAX_BITS};
use crate::errors::JsError;
use crate::value::Value;

impl Runtime {
    pub(crate) fn install_bigint(&mut self) {
        let prototype = self.prototypes.bigint;
        self.define_builtin_method(prototype, "toString", 0, bigint_to_string);
        self.define_builtin_method(prototype, "toLocaleString", 0, bigint_to_string);
        self.define_builtin_method(prototype, "valueOf", 0, bigint_value_of);

        let constructor = self.new_builtin_value("BigInt", 1, global_bigint);
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
            let as_int_n = self.new_builtin_value("asIntN", 2, bigint_as_int_n);
            let as_uint_n = self.new_builtin_value("asUintN", 2, bigint_as_uint_n);
            self.define_data_property(*id, "asIntN", as_int_n);
            self.define_data_property(*id, "asUintN", as_uint_n);
        }
        self.declare_global("BigInt", constructor);
    }
}

/// Convert a JavaScript value to a BigInt with the `BigInt()` semantics.
pub(crate) fn to_bigint(runtime: &mut Runtime, value: &Value) -> Result<BigInt, JsError> {
    let primitive = runtime.to_primitive(value)?;
    match &primitive {
        Value::BigInt(integer) => Ok((**integer).clone()),
        Value::Number(number) => BigInt::from_f64(*number).map_err(bigint_error),
        Value::String(text) => BigInt::parse_string(text).map_err(bigint_error),
        Value::Bool(boolean) => Ok(BigInt::from_i64(i64::from(*boolean))),
        // Web-compatibility: some site bundles call BigInt() on a missing
        // config value; treat nullish as zero instead of aborting the script.
        Value::Undefined | Value::Null => Ok(BigInt::from_i64(0)),
        Value::Object(_) => Err(JsError::type_error(format!(
            "Cannot convert {} to a BigInt",
            primitive.type_of()
        ))),
    }
}

fn bigint_error(error: BigIntError) -> JsError {
    match error {
        BigIntError::InvalidString => JsError::syntax_error(error.to_string()),
        BigIntError::NonInteger | BigIntError::InvalidRadix | BigIntError::LimitExceeded => {
            JsError::range_error(error.to_string())
        }
        _ => JsError::range_error(error.to_string()),
    }
}

fn this_bigint(runtime: &mut Runtime, this: &Value) -> Result<BigInt, JsError> {
    match this {
        Value::BigInt(integer) => Ok((**integer).clone()),
        Value::Object(_) => {
            let value_of = runtime.get_property_checked(this, "valueOf")?;
            if !runtime.is_callable(&value_of) {
                return Err(JsError::type_error(
                    "BigInt.prototype.toString called on incompatible receiver",
                ));
            }
            let unwrapped = runtime.call_function(&value_of, this.clone(), Vec::new())?;
            match unwrapped {
                Value::BigInt(integer) => Ok(*integer),
                _ => Err(JsError::type_error(
                    "BigInt.prototype method called on incompatible receiver",
                )),
            }
        }
        _ => Err(JsError::type_error(
            "BigInt.prototype method called on incompatible receiver",
        )),
    }
}

fn width_arg(runtime: &mut Runtime, value: Option<&Value>) -> Result<u64, JsError> {
    let number = match value {
        Some(value) => runtime.to_number_rt(value)?,
        None => 0.0,
    };
    if !number.is_finite() || number.fract() != 0.0 || number < 0.0 || number > MAX_BITS as f64 {
        return Err(JsError::range_error(format!(
            "BigInt width must be an integer between 0 and {MAX_BITS}"
        )));
    }
    Ok(number as u64)
}

fn global_bigint(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Number(0.0));
    Ok(Value::BigInt(Box::new(to_bigint(runtime, &value)?)))
}

fn bigint_as_int_n(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let width = width_arg(runtime, args.first())?;
    let integer = match args.get(1) {
        Some(value) => to_bigint(runtime, value)?,
        None => BigInt::from_i64(0),
    };
    integer
        .as_int_n(width)
        .map(Box::new)
        .map(Value::BigInt)
        .map_err(bigint_error)
}

fn bigint_as_uint_n(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let width = width_arg(runtime, args.first())?;
    let integer = match args.get(1) {
        Some(value) => to_bigint(runtime, value)?,
        None => BigInt::from_i64(0),
    };
    integer
        .as_uint_n(width)
        .map(Box::new)
        .map(Value::BigInt)
        .map_err(bigint_error)
}

fn bigint_to_string(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let integer = this_bigint(runtime, &this)?;
    let radix = match args.first() {
        Some(value) => {
            let number = runtime.to_number_rt(value)?;
            if !number.is_finite() || number.fract() != 0.0 || !(2.0..=36.0).contains(&number) {
                return Err(JsError::range_error(
                    "toString() radix must be an integer between 2 and 36",
                ));
            }
            number as u32
        }
        None => 10,
    };
    Ok(Value::String(integer.to_string_radix(radix)))
}

fn bigint_value_of(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::BigInt(Box::new(this_bigint(runtime, &this)?)))
}

#[cfg(test)]
mod tests {
    use crate::Runtime;
    use crate::value::Value;

    fn eval(source: &str) -> Value {
        let mut runtime = Runtime::new();
        runtime
            .eval(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
    }

    fn text(source: &str) -> String {
        match eval(source) {
            Value::String(value) => value,
            other => panic!("{source}: expected string, got {other:?}"),
        }
    }

    fn truth(source: &str) -> bool {
        match eval(source) {
            Value::Bool(value) => value,
            other => panic!("{source}: expected boolean, got {other:?}"),
        }
    }

    fn error(source: &str) -> String {
        let mut runtime = Runtime::new();
        runtime.eval(source).expect_err(source).message
    }

    #[test]
    fn global_constructor_and_typeof() {
        truth("typeof BigInt === 'function'");
        truth("typeof BigInt(1) === 'bigint'");
        truth("typeof BigInt('123') === 'bigint'");
        truth("typeof BigInt(true) === 'bigint'");
        truth("typeof BigInt(0) === 'bigint'");
        assert_eq!(text("typeof BigInt(1)"), "bigint");
        truth("var x = BigInt(5); typeof x === 'bigint' && x == 5");
    }

    #[test]
    fn construction_from_primitives_and_strings() {
        assert_eq!(text("BigInt(42).toString()"), "42");
        assert_eq!(text("BigInt(-7).toString()"), "-7");
        assert_eq!(text("BigInt(true).toString()"), "1");
        assert_eq!(text("BigInt(false).toString()"), "0");
        assert_eq!(text("BigInt().toString()"), "0");
        assert_eq!(
            text("BigInt('9007199254740993').toString()"),
            "9007199254740993"
        );
        assert_eq!(text("BigInt('0xff').toString()"), "255");
        assert_eq!(text("BigInt('-0b1010').toString()"), "-10");
        assert_eq!(text("BigInt('0o17').toString()"), "15");
        assert_eq!(text("BigInt('  12  ').toString()"), "12");
        assert_eq!(text("BigInt(255).toString(16)"), "ff");
        assert_eq!(text("BigInt(-255).toString(2)"), "-11111111");
        assert_eq!(text("BigInt(1).valueOf().toString()"), "1");
        assert_eq!(text("BigInt(12) + 'px'"), "12px");
        assert_eq!(text("'id=' + BigInt(34)"), "id=34");
        assert_eq!(
            text("Object.prototype.toString.call(BigInt(1))"),
            "[object BigInt]"
        );
        truth("'valueOf' in BigInt(7) && 'toString' in BigInt(7)");
    }

    #[test]
    fn invalid_conversions_throw() {
        assert!(error("BigInt('abc')").contains("SyntaxError"));
        assert!(error("BigInt(1.5)").contains("RangeError"));
        assert!(error("BigInt(NaN)").contains("RangeError"));
        assert!(error("BigInt(Infinity)").contains("RangeError"));
        assert!(error("BigInt(1).toString(1)").contains("RangeError"));
        assert!(error("BigInt(1).toString(37)").contains("RangeError"));
    }

    #[test]
    fn arithmetic_and_bitwise_operations() {
        assert_eq!(
            text("(BigInt('9007199254740993') + BigInt(7)).toString()"),
            "9007199254741000"
        );
        assert_eq!(text("(BigInt(5) - BigInt(12)).toString()"), "-7");
        assert_eq!(text("(BigInt(-123) * BigInt(456)).toString()"), "-56088");
        assert_eq!(text("(BigInt(-7) / BigInt(2)).toString()"), "-3");
        assert_eq!(text("(BigInt(-7) % BigInt(2)).toString()"), "-1");
        assert_eq!(
            text("(BigInt(2) ** BigInt(100)).toString()"),
            "1267650600228229401496703205376"
        );
        assert_eq!(text("(-BigInt(5)).toString()"), "-5");
        assert_eq!(text("(~BigInt(0)).toString()"), "-1");
        assert_eq!(text("(BigInt(-1) & BigInt(5)).toString()"), "5");
        assert_eq!(text("(BigInt(-1) | BigInt(5)).toString()"), "-1");
        assert_eq!(text("(BigInt(-1) ^ BigInt(5)).toString()"), "-6");
        assert_eq!(
            text("(BigInt(1) << BigInt(130)).toString()"),
            "1361129467683753853853498429727072845824"
        );
        assert_eq!(text("(BigInt(-7) >> BigInt(1)).toString()"), "-4");
        let mut runtime = Runtime::new();
        runtime
            .eval("var n = BigInt(10); n += BigInt(5); n++; --n;")
            .unwrap();
        assert_eq!(text_at(&mut runtime, "n.toString()"), "15");
    }

    fn text_at(runtime: &mut Runtime, source: &str) -> String {
        match runtime.eval(source).unwrap() {
            Value::String(value) => value,
            other => panic!("{source}: expected string, got {other:?}"),
        }
    }

    #[test]
    fn comparisons_and_mixing_rules() {
        truth("BigInt(1) === BigInt(1)");
        truth("BigInt(1) !== BigInt('2')");
        truth("BigInt(1) !== 1");
        truth("BigInt(1) == 1");
        truth("BigInt(1) == '1'");
        truth("BigInt(1) == true");
        truth("BigInt('9007199254740993') > 9007199254740992");
        truth("BigInt(-1) < 0");
        truth("BigInt(1) <= BigInt(1) && BigInt(2) >= BigInt(1)");
    }

    #[test]
    fn truthiness_and_mixed_arithmetic_errors() {
        truth("!BigInt(0) && !BigInt('0')");
        truth("!!BigInt(1) && !!BigInt(-1)");
        truth("if (BigInt(0)) { false } else { true }");
        assert!(error("BigInt(1) + 1").contains("TypeError"));
        assert!(error("1 - BigInt(1)").contains("TypeError"));
        assert!(error("BigInt(1) * 2").contains("TypeError"));
        assert!(error("BigInt(1) >>> BigInt(1)").contains("TypeError"));
        assert!(error("+BigInt(1)").contains("TypeError"));
        assert!(error("BigInt(1) / BigInt(0)").contains("RangeError"));
        assert!(error("JSON.stringify(BigInt(1))").contains("TypeError"));
        assert!(error("BigInt(1) ** BigInt(-1)").contains("RangeError"));
        assert!(error("BigInt(1) << BigInt(-1)").contains("RangeError"));
    }

    #[test]
    fn static_wrapping_helpers() {
        assert_eq!(text("BigInt.asUintN(8, BigInt(-1)).toString()"), "255");
        assert_eq!(text("BigInt.asIntN(8, BigInt(255)).toString()"), "-1");
        assert_eq!(text("BigInt.asIntN(8, BigInt(-1)).toString()"), "-1");
        assert_eq!(text("BigInt.asUintN(8, BigInt(256)).toString()"), "0");
        assert_eq!(text("BigInt.asUintN(0, BigInt(123)).toString()"), "0");
        assert!(error("BigInt.asIntN(-1, BigInt(1))").contains("RangeError"));
    }
}
