//! Builtin objects and functions installed into the global environment.

use std::rc::Rc;

use browser_js_parser::{Regex, RegexMatch};

use crate::Runtime;
use crate::errors::JsError;
use crate::object::ObjectKind;
use crate::value::{Value, number_to_string};

impl Runtime {
    /// Install the standard library.
    pub(crate) fn bootstrap(&mut self) {
        let object_prototype = self.bootstrap_object(None);
        self.prototypes.object = object_prototype;
        self.prototypes.array = self.bootstrap_object(Some(object_prototype));
        self.prototypes.function = self.bootstrap_object(Some(object_prototype));
        self.prototypes.string = self.bootstrap_object(Some(object_prototype));
        self.prototypes.number = self.bootstrap_object(Some(object_prototype));
        self.prototypes.boolean = self.bootstrap_object(Some(object_prototype));
        self.prototypes.error = self.bootstrap_object(Some(object_prototype));
        self.prototypes.promise = self.bootstrap_object(Some(object_prototype));
        self.prototypes.regexp = self.bootstrap_object(Some(object_prototype));

        let global = self.bootstrap_object(Some(object_prototype));
        self.global = global;
        let global_scope = self.create_scope(None, true);
        self.global_scope = global_scope;
        self.env_stack.push(global_scope);
        self.var_scope_stack.push(global_scope);

        self.install_object_prototype();
        self.install_array_prototype();
        self.install_function_prototype();
        self.install_string_prototype();
        self.install_number_prototype();
        self.install_boolean_prototype();
        self.install_error();
        self.install_regexp();
        self.install_globals();
        self.install_math();
        self.install_json();
        self.install_console();
        self.install_promise();

        self.declare_binding(
            global_scope,
            "this",
            crate::scope::BindingKind::This,
            Value::Object(global),
            true,
        );
    }

    fn install_globals(&mut self) {
        self.declare_global("undefined", Value::Undefined);
        self.declare_global("NaN", Value::Number(f64::NAN));
        self.declare_global("Infinity", Value::Number(f64::INFINITY));
        let global = Value::Object(self.global);
        self.declare_global("globalThis", global);

        let parse_int = self.new_builtin_value("parseInt", 2, global_parse_int);
        let parse_float = self.new_builtin_value("parseFloat", 1, global_parse_float);
        let is_nan = self.new_builtin_value("isNaN", 1, global_is_nan);
        let is_finite = self.new_builtin_value("isFinite", 1, global_is_finite);
        self.declare_global("parseInt", parse_int);
        self.declare_global("parseFloat", parse_float);
        self.declare_global("isNaN", is_nan);
        self.declare_global("isFinite", is_finite);

        let string = self.new_builtin_value("String", 1, global_string);
        let number = self.new_builtin_value("Number", 1, global_number);
        let boolean = self.new_builtin_value("Boolean", 1, global_boolean);
        self.declare_global("String", string.clone());
        self.declare_global("Number", number.clone());
        self.declare_global("Boolean", boolean.clone());
        if let Value::Object(id) = &string {
            self.define_data_property(*id, "prototype", Value::Object(self.prototypes.string));
        }
        if let Value::Object(id) = &number {
            self.define_data_property(*id, "prototype", Value::Object(self.prototypes.number));
        }
        if let Value::Object(id) = &boolean {
            self.define_data_property(*id, "prototype", Value::Object(self.prototypes.boolean));
        }

        let object = self.new_builtin_value("Object", 1, global_object);
        if let Value::Object(id) = &object {
            self.define_data_property(*id, "prototype", Value::Object(self.prototypes.object));
            let keys = self.new_builtin_value("keys", 1, object_keys);
            let create = self.new_builtin_value("create", 1, object_create);
            self.define_data_property(*id, "keys", keys);
            self.define_data_property(*id, "create", create);
        }
        self.declare_global("Object", object);

        let array = self.new_builtin_value("Array", 1, global_array);
        if let Value::Object(id) = &array {
            self.define_data_property(*id, "prototype", Value::Object(self.prototypes.array));
            let is_array = self.new_builtin_value("isArray", 1, array_is_array);
            self.define_data_property(*id, "isArray", is_array);
        }
        self.declare_global("Array", array);
    }

    fn install_console(&mut self) {
        let console = self.new_object();
        if let Value::Object(id) = console {
            let log = self.new_builtin_value("log", 0, console_log);
            let warn = self.new_builtin_value("warn", 0, console_log);
            let error = self.new_builtin_value("error", 0, console_log);
            self.define_data_property(id, "log", log);
            self.define_data_property(id, "warn", warn);
            self.define_data_property(id, "error", error);
            let value = Value::Object(id);
            self.declare_global("console", value);
        }
    }

    fn install_object_prototype(&mut self) {
        let prototype = self.prototypes.object;
        self.define_builtin_method(prototype, "hasOwnProperty", 1, object_has_own_property);
        self.define_builtin_method(prototype, "toString", 0, object_to_string);
        self.define_builtin_method(prototype, "valueOf", 0, object_value_of);
    }

    fn install_array_prototype(&mut self) {
        let prototype = self.prototypes.array;
        self.define_builtin_method(prototype, "push", 1, array_push);
        self.define_builtin_method(prototype, "pop", 0, array_pop);
        self.define_builtin_method(prototype, "shift", 0, array_shift);
        self.define_builtin_method(prototype, "unshift", 1, array_unshift);
        self.define_builtin_method(prototype, "join", 1, array_join);
        self.define_builtin_method(prototype, "indexOf", 1, array_index_of);
        self.define_builtin_method(prototype, "slice", 2, array_slice);
        self.define_builtin_method(prototype, "map", 1, array_map);
        self.define_builtin_method(prototype, "forEach", 1, array_for_each);
        self.define_builtin_method(prototype, "filter", 1, array_filter);
        self.define_builtin_method(prototype, "reduce", 1, array_reduce);
        self.define_builtin_method(prototype, "concat", 1, array_concat);
        self.define_builtin_method(prototype, "reverse", 0, array_reverse);
        self.define_builtin_method(prototype, "toString", 0, array_join);
    }

    fn install_function_prototype(&mut self) {
        let prototype = self.prototypes.function;
        self.define_builtin_method(prototype, "call", 1, function_call);
        self.define_builtin_method(prototype, "apply", 2, function_apply);
        self.define_builtin_method(prototype, "toString", 0, function_to_string);
    }

    fn install_string_prototype(&mut self) {
        let prototype = self.prototypes.string;
        self.define_builtin_method(prototype, "charAt", 1, string_char_at);
        self.define_builtin_method(prototype, "charCodeAt", 1, string_char_code_at);
        self.define_builtin_method(prototype, "indexOf", 1, string_index_of);
        self.define_builtin_method(prototype, "lastIndexOf", 1, string_last_index_of);
        self.define_builtin_method(prototype, "slice", 2, string_slice);
        self.define_builtin_method(prototype, "substring", 2, string_substring);
        self.define_builtin_method(prototype, "toUpperCase", 0, string_to_upper);
        self.define_builtin_method(prototype, "toLowerCase", 0, string_to_lower);
        self.define_builtin_method(prototype, "split", 2, string_split);
        self.define_builtin_method(prototype, "trim", 0, string_trim);
        self.define_builtin_method(prototype, "replace", 2, string_replace);
        self.define_builtin_method(prototype, "match", 1, string_match);
        self.define_builtin_method(prototype, "includes", 1, string_includes);
        self.define_builtin_method(prototype, "startsWith", 1, string_starts_with);
        self.define_builtin_method(prototype, "endsWith", 1, string_ends_with);
        self.define_builtin_method(prototype, "concat", 1, string_concat);
        self.define_builtin_method(prototype, "toString", 0, string_to_string);
        self.define_builtin_method(prototype, "valueOf", 0, string_to_string);
    }

    fn install_number_prototype(&mut self) {
        let prototype = self.prototypes.number;
        self.define_builtin_method(prototype, "toString", 0, number_to_string_method);
        self.define_builtin_method(prototype, "valueOf", 0, number_value_of);
        self.define_builtin_method(prototype, "toFixed", 1, number_to_fixed);
    }

    fn install_boolean_prototype(&mut self) {
        let prototype = self.prototypes.boolean;
        self.define_builtin_method(prototype, "toString", 0, boolean_to_string);
        self.define_builtin_method(prototype, "valueOf", 0, boolean_value_of);
    }

    fn install_error(&mut self) {
        let prototype = self.prototypes.error;
        self.define_data_property(prototype, "name", Value::String("Error".to_owned()));
        self.define_data_property(prototype, "message", Value::String(String::new()));
        self.define_builtin_method(prototype, "toString", 0, error_to_string);

        let error = self.new_builtin_value("Error", 1, global_error);
        if let Value::Object(id) = &error {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
        }
        self.declare_global("Error", error);

        let names = [
            ("TypeError", global_type_error as crate::object::BuiltinFn),
            ("ReferenceError", global_reference_error),
            ("RangeError", global_range_error),
            ("SyntaxError", global_syntax_error),
        ];
        for (name, implementation) in names {
            let constructor = self.new_builtin_value(name, 1, implementation);
            if let Value::Object(id) = &constructor {
                self.define_data_property(*id, "prototype", Value::Object(prototype));
            }
            self.declare_global(name, constructor);
        }
    }

    fn install_regexp(&mut self) {
        let prototype = self.prototypes.regexp;
        self.define_builtin_method(prototype, "test", 1, regexp_test);
        self.define_builtin_method(prototype, "exec", 1, regexp_exec);
        self.define_builtin_method(prototype, "toString", 0, regexp_to_string);

        let constructor = self.new_builtin_value("RegExp", 2, global_regexp);
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
        }
        self.declare_global("RegExp", constructor);
    }

    fn install_math(&mut self) {
        let math = self.new_object();
        let Value::Object(id) = math else {
            return;
        };
        self.define_data_property(id, "PI", Value::Number(std::f64::consts::PI));
        self.define_data_property(id, "E", Value::Number(std::f64::consts::E));
        self.define_builtin_method(id, "abs", 1, math_abs);
        self.define_builtin_method(id, "floor", 1, math_floor);
        self.define_builtin_method(id, "ceil", 1, math_ceil);
        self.define_builtin_method(id, "round", 1, math_round);
        self.define_builtin_method(id, "max", 2, math_max);
        self.define_builtin_method(id, "min", 2, math_min);
        self.define_builtin_method(id, "pow", 2, math_pow);
        self.define_builtin_method(id, "sqrt", 1, math_sqrt);
        self.define_builtin_method(id, "random", 0, math_random);
        let value = Value::Object(id);
        self.declare_global("Math", value);
    }

    fn install_json(&mut self) {
        let json = self.new_object();
        let Value::Object(id) = json else {
            return;
        };
        self.define_builtin_method(id, "stringify", 1, json_stringify);
        self.define_builtin_method(id, "parse", 1, json_parse);
        let value = Value::Object(id);
        self.declare_global("JSON", value);
    }
}

// ---------------------------------------------------------------------------
// Global functions
// ---------------------------------------------------------------------------

fn console_log(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut parts = Vec::with_capacity(args.len());
    for argument in args {
        let text = match &argument {
            Value::String(text) => text.clone(),
            Value::Object(_) => runtime
                .to_string_rt(&argument)
                .unwrap_or_else(|_| "[object Object]".to_owned()),
            other => other.to_string_value(),
        };
        parts.push(text);
    }
    let line = parts.join(" ");
    (runtime.console)(&line);
    Ok(Value::Undefined)
}

fn global_parse_int(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let radix = args
        .get(1)
        .map(|value| value.to_number())
        .filter(|value| value.is_finite() && *value != 0.0)
        .map(|value| value.abs() as u32)
        .filter(|value| (2..=36).contains(value));
    Ok(Value::Number(parse_int_text(&text, radix)))
}

fn global_parse_float(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(parse_float_text(&text)))
}

fn global_is_nan(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(runtime.to_number_rt(&value)?.is_nan()))
}

fn global_is_finite(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(runtime.to_number_rt(&value)?.is_finite()))
}

fn global_string(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    match args.first() {
        Some(value) => Ok(Value::String(runtime.to_string_rt(value)?)),
        None => Ok(Value::String(String::new())),
    }
}

fn global_number(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    match args.first() {
        Some(value) => Ok(Value::Number(runtime.to_number_rt(value)?)),
        None => Ok(Value::Number(0.0)),
    }
}

fn global_boolean(
    _runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::Bool(args.first().is_some_and(Value::is_truthy)))
}

fn global_object(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    match args.first() {
        Some(Value::Object(_)) => Ok(args[0].clone()),
        _ => Ok(runtime.new_object()),
    }
}

fn global_array(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    if args.len() == 1
        && let Value::Number(length) = args[0]
    {
        if length < 0.0 || length.fract() != 0.0 || !length.is_finite() {
            return Err(JsError::range_error("invalid array length"));
        }
        let items = vec![Value::Undefined; length as usize];
        return Ok(runtime.new_array(items));
    }
    Ok(runtime.new_array(args))
}

fn global_error(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let message = args
        .first()
        .map(|value| value.to_string_value())
        .unwrap_or_default();
    Ok(runtime.new_error_value("Error", &message))
}

fn global_type_error(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let message = args
        .first()
        .map(|value| value.to_string_value())
        .unwrap_or_default();
    Ok(runtime.new_error_value("TypeError", &message))
}

fn global_reference_error(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let message = args
        .first()
        .map(|value| value.to_string_value())
        .unwrap_or_default();
    Ok(runtime.new_error_value("ReferenceError", &message))
}

fn global_range_error(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let message = args
        .first()
        .map(|value| value.to_string_value())
        .unwrap_or_default();
    Ok(runtime.new_error_value("RangeError", &message))
}

fn global_syntax_error(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let message = args
        .first()
        .map(|value| value.to_string_value())
        .unwrap_or_default();
    Ok(runtime.new_error_value("SyntaxError", &message))
}

fn parse_int_text(text: &str, radix: Option<u32>) -> f64 {
    let trimmed = text.trim_start();
    let (sign, rest) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let mut radix = radix.unwrap_or(0);
    let rest = if radix == 16 || radix == 0 {
        if let Some(digits) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            radix = 16;
            digits
        } else {
            rest
        }
    } else {
        rest
    };
    if radix == 0 {
        radix = 10;
    }
    let mut value = 0.0;
    let mut any = false;
    for ch in rest.chars() {
        match ch.to_digit(radix) {
            Some(digit) => {
                value = value * f64::from(radix) + f64::from(digit);
                any = true;
            }
            None => break,
        }
    }
    if any { sign * value } else { f64::NAN }
}

fn parse_float_text(text: &str) -> f64 {
    let trimmed = text.trim_start();
    let chars: Vec<char> = trimmed.chars().collect();
    let mut index = 0;
    if matches!(chars.first(), Some('+') | Some('-')) {
        index += 1;
    }
    let mut seen_digit = false;
    while chars.get(index).is_some_and(|ch| ch.is_ascii_digit()) {
        index += 1;
        seen_digit = true;
    }
    if chars.get(index) == Some(&'.') {
        index += 1;
        while chars.get(index).is_some_and(|ch| ch.is_ascii_digit()) {
            index += 1;
            seen_digit = true;
        }
    }
    if seen_digit && matches!(chars.get(index), Some('e') | Some('E')) {
        let save = index;
        index += 1;
        if matches!(chars.get(index), Some('+') | Some('-')) {
            index += 1;
        }
        if chars.get(index).is_some_and(|ch| ch.is_ascii_digit()) {
            while chars.get(index).is_some_and(|ch| ch.is_ascii_digit()) {
                index += 1;
            }
        } else {
            index = save;
        }
    }
    if !seen_digit {
        return f64::NAN;
    }
    let prefix: String = chars[..index].iter().collect();
    prefix.parse::<f64>().unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------
// Object, Array, Function and primitive prototypes
// ---------------------------------------------------------------------------

fn object_has_own_property(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let key = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    match &this {
        Value::Object(id) => Ok(Value::Bool(runtime.has_own_property(*id, &key))),
        _ => Ok(Value::Bool(false)),
    }
}

fn object_to_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let name = match &this {
        Value::Undefined => "Undefined",
        Value::Null => "Null",
        Value::Object(id) => match runtime.object(*id).map(|object| &object.kind) {
            Some(ObjectKind::Array(_)) => "Array",
            Some(ObjectKind::Function(_)) => "Function",
            Some(ObjectKind::Promise(_)) => "Promise",
            Some(ObjectKind::RegExp(_)) => "RegExp",
            _ => "Object",
        },
        _ => "Object",
    };
    Ok(Value::String(format!("[object {name}]")))
}

fn object_value_of(
    _runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(this)
}

fn object_keys(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let keys = runtime.object_keys(&target);
    Ok(runtime.new_array(keys.into_iter().map(Value::String).collect()))
}

fn object_create(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let prototype = match args.first() {
        Some(Value::Object(id)) => Some(*id),
        Some(Value::Null) | None => None,
        _ => {
            return Err(JsError::type_error(
                "Object prototype may only be an object or null",
            ));
        }
    };
    Ok(Value::Object(
        runtime.alloc_object(ObjectKind::Ordinary, prototype),
    ))
}

fn array_is_array(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    Ok(Value::Bool(runtime.object_is_array(&value)))
}

fn require_array(runtime: &mut Runtime, this: &Value) -> Result<Vec<Value>, JsError> {
    runtime
        .object_array_items(this)
        .ok_or_else(|| JsError::type_error("array method called on a non-array"))
}

fn array_push(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut items = require_array(runtime, &this)?;
    items.extend(args);
    let length = items.len();
    runtime.set_array_items(&this, items);
    Ok(Value::Number(length as f64))
}

fn array_pop(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let mut items = require_array(runtime, &this)?;
    let value = items.pop().unwrap_or(Value::Undefined);
    runtime.set_array_items(&this, items);
    Ok(value)
}

fn array_shift(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let mut items = require_array(runtime, &this)?;
    let value = if items.is_empty() {
        Value::Undefined
    } else {
        items.remove(0)
    };
    runtime.set_array_items(&this, items);
    Ok(value)
}

fn array_unshift(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut items = args;
    let existing = require_array(runtime, &this)?;
    items.extend(existing);
    let length = items.len();
    runtime.set_array_items(&this, items);
    Ok(Value::Number(length as f64))
}

fn array_join(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let separator = match args.first() {
        None | Some(Value::Undefined) => ",".to_owned(),
        Some(value) => runtime.to_string_rt(value)?,
    };
    let mut parts = Vec::with_capacity(items.len());
    for item in &items {
        match item {
            Value::Undefined | Value::Null => parts.push(String::new()),
            other => parts.push(runtime.to_string_rt(other)?),
        }
    }
    Ok(Value::String(parts.join(&separator)))
}

fn array_index_of(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let needle = args.first().cloned().unwrap_or(Value::Undefined);
    let start = match args.get(1).map(Value::to_number) {
        Some(number) if number.is_finite() => {
            if number < 0.0 {
                (items.len() as f64 + number).max(0.0) as usize
            } else {
                number as usize
            }
        }
        _ => 0,
    };
    for (index, item) in items.iter().enumerate().skip(start) {
        if item.strict_equals(&needle) {
            return Ok(Value::Number(index as f64));
        }
    }
    Ok(Value::Number(-1.0))
}

fn slice_bounds(length: usize, start: Option<&Value>, end: Option<&Value>) -> (usize, usize) {
    let normalise = |value: Option<&Value>, default: usize| -> usize {
        match value {
            Some(Value::Undefined) | None => default,
            Some(value) => {
                let number = value.to_number();
                if number.is_nan() {
                    return 0;
                }
                if number < 0.0 {
                    (length as f64 + number).max(0.0) as usize
                } else {
                    (number as usize).min(length)
                }
            }
        }
    };
    let start = normalise(start, 0);
    let end = normalise(end, length);
    (start.min(length), end)
}

fn array_slice(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let (start, end) = slice_bounds(items.len(), args.first(), args.get(1));
    let sliced = if start < end {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    Ok(runtime.new_array(sliced))
}

fn array_map(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&callback) {
        return Err(JsError::type_error("map callback is not a function"));
    }
    let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    let mark = runtime.temp_roots.len();
    let mut mapped = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let result = runtime.call_function(
            &callback,
            this_arg.clone(),
            vec![item.clone(), Value::Number(index as f64), this.clone()],
        );
        match result {
            Ok(value) => {
                runtime.temp_roots.push(value.clone());
                mapped.push(value);
            }
            Err(error) => {
                runtime.temp_roots.truncate(mark);
                return Err(error);
            }
        }
    }
    let array = runtime.new_array(mapped);
    runtime.temp_roots.truncate(mark);
    Ok(array)
}

fn array_for_each(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&callback) {
        return Err(JsError::type_error("forEach callback is not a function"));
    }
    let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    for (index, item) in items.iter().enumerate() {
        runtime.call_function(
            &callback,
            this_arg.clone(),
            vec![item.clone(), Value::Number(index as f64), this.clone()],
        )?;
    }
    Ok(Value::Undefined)
}

fn array_filter(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&callback) {
        return Err(JsError::type_error("filter callback is not a function"));
    }
    let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
    let mark = runtime.temp_roots.len();
    let mut kept = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let result = runtime.call_function(
            &callback,
            this_arg.clone(),
            vec![item.clone(), Value::Number(index as f64), this.clone()],
        );
        match result {
            Ok(value) => {
                if value.is_truthy() {
                    runtime.temp_roots.push(item.clone());
                    kept.push(item.clone());
                }
            }
            Err(error) => {
                runtime.temp_roots.truncate(mark);
                return Err(error);
            }
        }
    }
    let array = runtime.new_array(kept);
    runtime.temp_roots.truncate(mark);
    Ok(array)
}

fn array_reduce(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let items = require_array(runtime, &this)?;
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !runtime.is_callable(&callback) {
        return Err(JsError::type_error("reduce callback is not a function"));
    }
    let mut index = 0;
    let mut accumulator = if let Some(initial) = args.get(1) {
        initial.clone()
    } else if let Some(first) = items.first() {
        index = 1;
        first.clone()
    } else {
        return Err(JsError::type_error(
            "reduce of empty array with no initial value",
        ));
    };
    let mark = runtime.temp_roots.len();
    while index < items.len() {
        runtime.temp_roots.push(accumulator.clone());
        let result = runtime.call_function(
            &callback,
            Value::Undefined,
            vec![
                accumulator,
                items[index].clone(),
                Value::Number(index as f64),
                this.clone(),
            ],
        );
        match result {
            Ok(value) => accumulator = value,
            Err(error) => {
                runtime.temp_roots.truncate(mark);
                return Err(error);
            }
        }
        index += 1;
    }
    runtime.temp_roots.truncate(mark);
    Ok(accumulator)
}

fn array_concat(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut items = require_array(runtime, &this)?;
    for argument in args {
        match runtime.object_array_items(&argument) {
            Some(elements) => items.extend(elements),
            None => items.push(argument),
        }
    }
    Ok(runtime.new_array(items))
}

fn array_reverse(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let mut items = require_array(runtime, &this)?;
    items.reverse();
    let length = items.len();
    runtime.set_array_items(&this, items);
    Ok(Value::Number(length as f64))
}

fn function_call(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    if !runtime.is_callable(&this) {
        return Err(JsError::type_error(
            "Function.prototype.call on non-function",
        ));
    }
    let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
    let rest: Vec<Value> = args.into_iter().skip(1).collect();
    runtime.call_function(&this, this_arg, rest)
}

fn function_apply(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    if !runtime.is_callable(&this) {
        return Err(JsError::type_error(
            "Function.prototype.apply on non-function",
        ));
    }
    let this_arg = args.first().cloned().unwrap_or(Value::Undefined);
    let arguments = match args.get(1) {
        Some(value) => runtime.object_array_items(value).unwrap_or_default(),
        None => Vec::new(),
    };
    runtime.call_function(&this, this_arg, arguments)
}

fn function_to_string(
    _runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::String(match this {
        Value::Object(_) => "function () { [native code] }".to_owned(),
        other => other.to_string_value(),
    }))
}

fn this_string(runtime: &mut Runtime, this: &Value) -> Result<String, JsError> {
    runtime.to_string_rt(this)
}

fn string_char_at(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let index = args.first().map(Value::to_number).unwrap_or(0.0);
    if !index.is_finite() || index < 0.0 {
        return Ok(Value::String(String::new()));
    }
    Ok(match text.chars().nth(index as usize) {
        Some(ch) => Value::String(ch.to_string()),
        None => Value::String(String::new()),
    })
}

fn string_char_code_at(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let index = args.first().map(Value::to_number).unwrap_or(0.0);
    if !index.is_finite() || index < 0.0 {
        return Ok(Value::Number(f64::NAN));
    }
    Ok(match text.chars().nth(index as usize) {
        Some(ch) => Value::Number(f64::from(ch as u32)),
        None => Value::Number(f64::NAN),
    })
}

fn find_subsequence(haystack: &[char], needle: &[char], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(haystack.len()));
    }
    if needle.len() > haystack.len() {
        return None;
    }
    (start..=haystack.len() - needle.len())
        .find(|&index| haystack[index..index + needle.len()] == *needle)
}

fn string_index_of(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let needle = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let start = match args.get(1).map(Value::to_number) {
        Some(number) if number.is_finite() => number.max(0.0) as usize,
        _ => 0,
    };
    let haystack: Vec<char> = text.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    Ok(match find_subsequence(&haystack, &needle_chars, start) {
        Some(index) => Value::Number(index as f64),
        None => Value::Number(-1.0),
    })
}

fn string_last_index_of(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let needle = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let haystack: Vec<char> = text.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.len() > haystack.len() {
        return Ok(Value::Number(-1.0));
    }
    let limit = match args.get(1).map(Value::to_number) {
        Some(number) if number.is_finite() => (number.max(0.0) as usize).min(haystack.len()),
        _ => haystack.len(),
    };
    if let Some(index) = find_subsequence(&haystack[..limit], &needle_chars, 0) {
        let mut last = index;
        let mut cursor = index;
        while let Some(found) = find_subsequence(&haystack[..limit], &needle_chars, cursor + 1) {
            last = found;
            cursor = found;
        }
        Ok(Value::Number(last as f64))
    } else {
        Ok(Value::Number(-1.0))
    }
}

fn string_slice(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = slice_bounds(chars.len(), args.first(), args.get(1));
    let end = end.max(start);
    Ok(Value::String(chars[start..end].iter().collect()))
}

fn string_substring(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let chars: Vec<char> = text.chars().collect();
    let length = chars.len();
    let normalise = |value: Option<&Value>, default: usize| -> usize {
        match value {
            None | Some(Value::Undefined) => default,
            Some(value) => {
                let number = value.to_number();
                if number.is_nan() || number < 0.0 {
                    0
                } else {
                    (number as usize).min(length)
                }
            }
        }
    };
    let first = normalise(args.first(), 0);
    let second = normalise(args.get(1), length);
    let (start, end) = if first <= second {
        (first, second)
    } else {
        (second, first)
    };
    Ok(Value::String(chars[start..end].iter().collect()))
}

fn string_to_upper(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::String(this_string(runtime, &this)?.to_uppercase()))
}

fn string_to_lower(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::String(this_string(runtime, &this)?.to_lowercase()))
}

fn string_split(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let limit = match args.get(1).map(Value::to_number) {
        Some(number) if number.is_finite() && number >= 0.0 => number as usize,
        _ => usize::MAX,
    };
    if let Some(regex) = args
        .first()
        .and_then(|separator| runtime.regex_of(separator))
    {
        return Ok(split_with_regex(runtime, &regex, &text, limit));
    }
    let pieces: Vec<String> = match args.first() {
        None | Some(Value::Undefined) => vec![text],
        Some(separator) => {
            let separator = runtime.to_string_rt(separator)?;
            if separator.is_empty() {
                text.chars().map(|ch| ch.to_string()).collect()
            } else {
                text.split(separator.as_str())
                    .map(ToString::to_string)
                    .collect()
            }
        }
    };
    Ok(runtime.new_array(pieces.into_iter().take(limit).map(Value::String).collect()))
}

fn split_with_regex(runtime: &mut Runtime, regex: &Regex, text: &str, limit: usize) -> Value {
    let chars: Vec<char> = text.chars().collect();
    let mut pieces: Vec<Value> = Vec::new();
    let mut last = 0usize;
    let mut cursor = 0usize;
    while cursor <= chars.len() && pieces.len() < limit {
        let Some(found) = regex.find_from(&chars, cursor) else {
            break;
        };
        pieces.push(Value::String(chars[last..found.start].iter().collect()));
        for group in found.groups.iter().skip(1) {
            pieces.push(match group {
                Some(value) => Value::String(value.clone()),
                None => Value::Undefined,
            });
        }
        last = found.end;
        cursor = if found.end == found.start {
            found.end + 1
        } else {
            found.end
        };
    }
    if pieces.len() < limit {
        pieces.push(Value::String(
            chars[last.min(chars.len())..].iter().collect(),
        ));
    }
    pieces.truncate(limit);
    runtime.new_array(pieces)
}

fn string_trim(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    Ok(Value::String(
        this_string(runtime, &this)?.trim().to_owned(),
    ))
}

fn string_replace(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let replacement = match args.get(1) {
        Some(Value::Object(_)) => {
            return Err(JsError::type_error(
                "function replacements are not supported",
            ));
        }
        Some(value) => runtime.to_string_rt(value)?,
        None => "undefined".to_owned(),
    };
    if let Some(regex) = args.first().and_then(|pattern| runtime.regex_of(pattern)) {
        let replaced = replace_with_regex(&regex, &text, &replacement);
        if let Some(pattern) = args.first()
            && regex.flags().global
        {
            runtime.set_property(pattern.clone(), "lastIndex", Value::Number(0.0));
        }
        return Ok(Value::String(replaced));
    }
    let pattern = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    if pattern.is_empty() {
        return Ok(Value::String(format!("{replacement}{text}")));
    }
    Ok(Value::String(match text.find(&pattern) {
        Some(index) => {
            let mut result = String::with_capacity(text.len());
            result.push_str(&text[..index]);
            result.push_str(&replacement);
            result.push_str(&text[index + pattern.len()..]);
            result
        }
        None => text,
    }))
}

/// Replace every (or the first) match of `regex` inside `text`, expanding
/// `$1`, `$&` and `$$` in `replacement`.
fn replace_with_regex(regex: &Regex, text: &str, replacement: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let global = regex.flags().global;
    let mut out = String::new();
    let mut last = 0usize;
    let mut cursor = 0usize;
    while cursor <= chars.len() {
        let Some(found) = regex.find_from(&chars, cursor) else {
            break;
        };
        out.extend(chars[last..found.start].iter());
        expand_replacement(&mut out, replacement, &found);
        last = found.end;
        if !global {
            break;
        }
        cursor = if found.end == found.start {
            found.end + 1
        } else {
            found.end
        };
    }
    out.extend(chars[last..].iter());
    out
}

fn expand_replacement(out: &mut String, replacement: &str, found: &RegexMatch) {
    let chars: Vec<char> = replacement.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch != '$' {
            out.push(ch);
            index += 1;
            continue;
        }
        let Some(next) = chars.get(index + 1).copied() else {
            out.push('$');
            break;
        };
        match next {
            '$' => {
                out.push('$');
                index += 2;
            }
            '&' => {
                out.push_str(
                    found
                        .groups
                        .first()
                        .and_then(|group| group.as_deref())
                        .unwrap_or(""),
                );
                index += 2;
            }
            '`' => {
                out.push_str("");
                index += 2;
            }
            digit if digit.is_ascii_digit() => {
                let group = digit.to_digit(10).unwrap_or(0) as usize;
                if let Some(value) = found.groups.get(group).and_then(|group| group.as_deref()) {
                    out.push_str(value);
                    index += 2;
                } else {
                    out.push('$');
                    index += 1;
                }
            }
            _ => {
                out.push('$');
                index += 1;
            }
        }
    }
}

fn string_match(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    if let Some(regex) = args.first().and_then(|pattern| runtime.regex_of(pattern)) {
        let chars: Vec<char> = text.chars().collect();
        if regex.flags().global {
            let mut matches = Vec::new();
            let mut cursor = 0usize;
            while cursor <= chars.len() {
                let Some(found) = regex.find_from(&chars, cursor) else {
                    break;
                };
                matches.push(Value::String(
                    found.groups.first().cloned().flatten().unwrap_or_default(),
                ));
                cursor = if found.end == found.start {
                    found.end + 1
                } else {
                    found.end
                };
            }
            if matches.is_empty() {
                return Ok(Value::Null);
            }
            return Ok(runtime.new_array(matches));
        }
        return Ok(match regex.find_from(&chars, 0) {
            Some(found) => match_value(runtime, &text, &found),
            None => Value::Null,
        });
    }
    let needle = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(match text.find(&needle) {
        Some(index) => {
            let array = runtime.new_array(vec![Value::String(needle)]);
            runtime.set_property(array.clone(), "index", Value::Number(index as f64));
            runtime.set_property(array.clone(), "input", Value::String(text));
            array
        }
        None => Value::Null,
    })
}

fn match_value(runtime: &mut Runtime, text: &str, found: &RegexMatch) -> Value {
    let items: Vec<Value> = found
        .groups
        .iter()
        .map(|group| match group {
            Some(value) => Value::String(value.clone()),
            None => Value::Undefined,
        })
        .collect();
    let array = runtime.new_array(items);
    runtime.set_property(array.clone(), "index", Value::Number(found.start as f64));
    runtime.set_property(array.clone(), "input", Value::String(text.to_owned()));
    array
}

fn global_regexp(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let pattern = args.first().cloned().unwrap_or(Value::Undefined);
    if let Some(regex) = runtime.regex_of(&pattern) {
        if args
            .get(1)
            .is_none_or(|flags| matches!(flags, Value::Undefined))
            && matches!(pattern, Value::Object(_))
        {
            return Ok(pattern);
        }
        let flags = match args.get(1) {
            Some(Value::Undefined) | None => regex.flags().as_string(),
            Some(value) => runtime.to_string_rt(value)?,
        };
        return runtime.new_regexp_value(regex.source(), &flags);
    }
    let source = match &pattern {
        Value::Undefined => String::new(),
        value => runtime.to_string_rt(value)?,
    };
    let flags = match args.get(1) {
        Some(Value::Undefined) | None => String::new(),
        Some(value) => runtime.to_string_rt(value)?,
    };
    runtime.new_regexp_value(&source, &flags)
}

fn require_regexp(runtime: &mut Runtime, this: &Value) -> Result<Rc<Regex>, JsError> {
    runtime
        .regex_of(this)
        .ok_or_else(|| JsError::type_error("RegExp method called on an incompatible receiver"))
}

fn regexp_test(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let regex = require_regexp(runtime, &this)?;
    let text = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let chars: Vec<char> = text.chars().collect();
    let global = regex.flags().global;
    let start = last_index(runtime, &this, global);
    match regex.find_from(&chars, start) {
        Some(found) => {
            if global {
                advance_last_index(runtime, &this, &found);
            }
            Ok(Value::Bool(true))
        }
        None => {
            if global {
                runtime.set_property(this, "lastIndex", Value::Number(0.0));
            }
            Ok(Value::Bool(false))
        }
    }
}

fn regexp_exec(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let regex = require_regexp(runtime, &this)?;
    let text = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let chars: Vec<char> = text.chars().collect();
    let global = regex.flags().global;
    let start = last_index(runtime, &this, global);
    match regex.find_from(&chars, start) {
        Some(found) => {
            if global {
                advance_last_index(runtime, &this, &found);
            }
            Ok(match_value(runtime, &text, &found))
        }
        None => {
            if global {
                runtime.set_property(this, "lastIndex", Value::Number(0.0));
            }
            Ok(Value::Null)
        }
    }
}

fn last_index(runtime: &mut Runtime, this: &Value, global: bool) -> usize {
    if !global {
        return 0;
    }
    let value = runtime.get_property(this, "lastIndex").to_number();
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value as usize
    }
}

fn advance_last_index(runtime: &mut Runtime, this: &Value, found: &RegexMatch) {
    let next = if found.end == found.start {
        found.end + 1
    } else {
        found.end
    };
    runtime.set_property(this.clone(), "lastIndex", Value::Number(next as f64));
}

fn regexp_to_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let regex = require_regexp(runtime, &this)?;
    Ok(Value::String(format!(
        "/{}/{}",
        regex.source(),
        regex.flags().as_string()
    )))
}

fn string_includes(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let needle = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Bool(text.contains(&needle)))
}

fn string_starts_with(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let needle = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Bool(text.starts_with(&needle)))
}

fn string_ends_with(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let text = this_string(runtime, &this)?;
    let needle = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Bool(text.ends_with(&needle)))
}

fn string_concat(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut text = this_string(runtime, &this)?;
    for argument in args {
        text.push_str(&runtime.to_string_rt(&argument)?);
    }
    Ok(Value::String(text))
}

fn string_to_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::String(this_string(runtime, &this)?))
}

fn number_to_string_method(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let number = runtime.to_number_rt(&this)?;
    match args.first().map(Value::to_number) {
        Some(radix) if radix.is_finite() && (2.0..=36.0).contains(&radix) => {
            Ok(Value::String(to_radix_string(number, radix as u32)))
        }
        _ => Ok(Value::String(number_to_string(number))),
    }
}

fn to_radix_string(number: f64, radix: u32) -> String {
    if radix == 10 || !number.is_finite() || number.fract() != 0.0 {
        return number_to_string(number);
    }
    let mut value = number.abs() as u64;
    if value == 0 {
        return "0".to_owned();
    }
    let negative = number < 0.0;
    let mut digits = Vec::new();
    while value > 0 {
        let digit = (value % u64::from(radix)) as u32;
        digits.push(char::from_digit(digit, radix).unwrap_or('0'));
        value /= u64::from(radix);
    }
    let mut text: String = digits.into_iter().rev().collect();
    if negative {
        text.insert(0, '-');
    }
    text
}

fn number_value_of(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::Number(runtime.to_number_rt(&this)?))
}

fn number_to_fixed(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let number = runtime.to_number_rt(&this)?;
    let digits = args.first().map(Value::to_number).unwrap_or(0.0);
    let digits = digits.clamp(0.0, 20.0) as usize;
    if !number.is_finite() {
        return Ok(Value::String(number_to_string(number)));
    }
    Ok(Value::String(format!("{number:.digits$}")))
}

fn boolean_to_string(
    _runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::String(this.is_truthy().to_string()))
}

fn boolean_value_of(
    _runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    Ok(Value::Bool(this.is_truthy()))
}

fn error_to_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let name = runtime.get_property(&this, "name").to_string_value();
    let message = runtime.get_property(&this, "message").to_string_value();
    let name = if name == "undefined" {
        "Error".to_owned()
    } else {
        name
    };
    Ok(Value::String(if message.is_empty() {
        name
    } else {
        format!("{name}: {message}")
    }))
}

// ---------------------------------------------------------------------------
// Math
// ---------------------------------------------------------------------------

fn math_abs(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(value.abs()))
}

fn math_floor(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(value.floor()))
}

fn math_ceil(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(value.ceil()))
}

fn math_round(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    if !value.is_finite() {
        return Ok(Value::Number(value));
    }
    Ok(Value::Number((value + 0.5).floor()))
}

fn math_max(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut result = f64::NEG_INFINITY;
    for argument in args {
        let value = runtime.to_number_rt(&argument)?;
        if value.is_nan() {
            return Ok(Value::Number(f64::NAN));
        }
        if value > result {
            result = value;
        }
    }
    Ok(Value::Number(result))
}

fn math_min(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let mut result = f64::INFINITY;
    for argument in args {
        let value = runtime.to_number_rt(&argument)?;
        if value.is_nan() {
            return Ok(Value::Number(f64::NAN));
        }
        if value < result {
            result = value;
        }
    }
    Ok(Value::Number(result))
}

fn math_pow(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let base = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let exponent = runtime.to_number_rt(&args.get(1).cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(base.powf(exponent)))
}

fn math_sqrt(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(value.sqrt()))
}

fn math_random(runtime: &mut Runtime, _this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let mut state = runtime.rng_state;
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    runtime.rng_state = state;
    let value = (state >> 11) as f64 / (1u64 << 53) as f64;
    Ok(Value::Number(value))
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

fn json_stringify(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    match stringify(runtime, &value, 0)? {
        Some(text) => Ok(Value::String(text)),
        None => Ok(Value::Undefined),
    }
}

fn stringify(
    runtime: &mut Runtime,
    value: &Value,
    depth: usize,
) -> Result<Option<String>, JsError> {
    if depth > 64 {
        return Err(JsError::range_error("JSON.stringify nesting is too deep"));
    }
    Ok(match value {
        Value::Undefined => None,
        Value::Null => Some("null".to_owned()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        Value::Number(number) => {
            if number.is_finite() {
                Some(number_to_string(*number))
            } else {
                Some("null".to_owned())
            }
        }
        Value::String(text) => Some(json_escape(text)),
        Value::Object(id) => {
            if runtime.is_callable(value) {
                return Ok(None);
            }
            match runtime.object(*id).map(|object| &object.kind) {
                Some(ObjectKind::Array(_)) => {
                    let items = runtime.object_array_items(value).unwrap_or_default();
                    let mut parts = Vec::with_capacity(items.len());
                    for item in &items {
                        parts.push(
                            stringify(runtime, item, depth + 1)?
                                .unwrap_or_else(|| "null".to_owned()),
                        );
                    }
                    Some(format!("[{}]", parts.join(",")))
                }
                _ => {
                    let keys = runtime.object_keys(value);
                    let mut parts = Vec::new();
                    for key in keys {
                        let item = runtime.get_property(value, &key);
                        if let Some(text) = stringify(runtime, &item, depth + 1)? {
                            parts.push(format!("{}:{}", json_escape(&key), text));
                        }
                    }
                    Some(format!("{{{}}}", parts.join(",")))
                }
            }
        }
    })
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn json_parse(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    let mut parser = JsonParser::new(&text);
    let parsed = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos < parser.chars.len() {
        return Err(JsError::syntax_error(
            "unexpected trailing characters in JSON",
        ));
    }
    Ok(json_to_value(runtime, &parsed))
}

enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

fn json_to_value(runtime: &mut Runtime, value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(boolean) => Value::Bool(*boolean),
        JsonValue::Number(number) => Value::Number(*number),
        JsonValue::String(text) => Value::String(text.clone()),
        JsonValue::Array(items) => {
            let values = items
                .iter()
                .map(|item| json_to_value(runtime, item))
                .collect();
            runtime.new_array(values)
        }
        JsonValue::Object(entries) => {
            let object = runtime.new_object();
            for (key, entry) in entries {
                let converted = json_to_value(runtime, entry);
                runtime.set_property(object.clone(), key, converted);
            }
            object
        }
    }
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|ch| ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r')
        {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), JsError> {
        self.skip_whitespace();
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(JsError::syntax_error(format!(
                "expected '{expected}' in JSON at offset {}",
                self.pos
            )))
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, JsError> {
        self.skip_whitespace();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => Ok(JsonValue::String(self.parse_string()?)),
            Some('t') => {
                self.expect_word("true")?;
                Ok(JsonValue::Bool(true))
            }
            Some('f') => {
                self.expect_word("false")?;
                Ok(JsonValue::Bool(false))
            }
            Some('n') => {
                self.expect_word("null")?;
                Ok(JsonValue::Null)
            }
            Some(ch) if ch == '-' || ch.is_ascii_digit() => self.parse_number(),
            _ => Err(JsError::syntax_error("unexpected token in JSON")),
        }
    }

    fn expect_word(&mut self, word: &str) -> Result<(), JsError> {
        for expected in word.chars() {
            if self.peek() != Some(expected) {
                return Err(JsError::syntax_error(format!(
                    "invalid JSON literal, expected '{word}'"
                )));
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn parse_object(&mut self) -> Result<JsonValue, JsError> {
        self.expect('{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(JsonValue::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.expect(':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(entries));
                }
                _ => return Err(JsError::syntax_error("expected ',' or '}' in JSON object")),
            }
        }
    }

    fn parse_array(&mut self) -> Result<JsonValue, JsError> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(items));
                }
                _ => return Err(JsError::syntax_error("expected ',' or ']' in JSON array")),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, JsError> {
        self.skip_whitespace();
        if self.peek() != Some('"') {
            return Err(JsError::syntax_error("expected string in JSON"));
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(ch) = self.peek() else {
                return Err(JsError::syntax_error("unterminated JSON string"));
            };
            self.pos += 1;
            match ch {
                '"' => return Ok(out),
                '\\' => {
                    let Some(escape) = self.peek() else {
                        return Err(JsError::syntax_error("unterminated JSON escape"));
                    };
                    self.pos += 1;
                    match escape {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{0008}'),
                        'f' => out.push('\u{000C}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let digit = self.peek().and_then(|ch| ch.to_digit(16)).ok_or_else(
                                    || JsError::syntax_error("invalid unicode escape in JSON"),
                                )?;
                                code = code * 16 + digit;
                                self.pos += 1;
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err(JsError::syntax_error("invalid JSON escape")),
                    }
                }
                ch if (ch as u32) < 0x20 => {
                    return Err(JsError::syntax_error("control character in JSON string"));
                }
                ch => out.push(ch),
            }
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsError> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.pos += 1;
            }
            while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(JsonValue::Number)
            .map_err(|_| JsError::syntax_error("invalid JSON number"))
    }
}

#[cfg(test)]
mod tests {
    use crate::value::Value;

    #[test]
    fn parses_int_and_float_text() {
        assert_eq!(super::parse_int_text("42abc", None), 42.0);
        assert_eq!(super::parse_int_text("0x1f", None), 31.0);
        assert_eq!(super::parse_int_text("-12", Some(10)), -12.0);
        assert!(super::parse_int_text("abc", None).is_nan());
        assert_eq!(super::parse_float_text("3.5px"), 3.5);
        assert!(super::parse_float_text("x3").is_nan());
    }

    #[test]
    fn radix_strings() {
        assert_eq!(super::to_radix_string(255.0, 16), "ff");
        assert_eq!(super::to_radix_string(-8.0, 2), "-1000");
    }

    #[test]
    fn json_round_trip() {
        let mut runtime = crate::Runtime::new();
        let value = runtime
            .eval("JSON.parse('{\"a\":[1,2,true,null],\"b\":\"x\\\\ny\"}')")
            .expect("eval");
        assert_eq!(
            runtime.get_property(&value, "b"),
            Value::String("x\ny".to_owned())
        );
        let text = runtime.eval("JSON.stringify({a:1,b:[2,3]})").expect("eval");
        assert_eq!(text, Value::String("{\"a\":1,\"b\":[2,3]}".to_owned()));
    }
}
