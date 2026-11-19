//! The JavaScript `Date` builtin.
//!
//! Everything in this module is implemented with plain Rust arithmetic: there
//! is no `chrono` dependency and no dependency on the host's timezone database.
//! `TimeClip` limits timestamps to +/- 8.64e15 milliseconds, exactly like the
//! ECMAScript specification, and civil dates are converted to/from day counts
//! with Howard Hinnant's proleptic-Gregorian algorithms.
//!
//! Local time is deliberately defined to be UTC (`getTimezoneOffset()` is
//! always `0`). A browser engine normally consults the host timezone here, but
//! this runtime has no such dependency; keeping the two clocks identical makes
//! every local getter/setter behave predictably and keeps `toISOString()`
//! round-trippable through every local getter.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::Runtime;
use crate::errors::JsError;
use crate::value::Value;

/// Private own-property key that holds a Date's `[[DateValue]]` (milliseconds
/// since the Unix epoch). The NUL prefix follows the engine's convention for
/// internal slots so normal JavaScript code cannot collide with it by accident.
const DATE_VALUE_SLOT: &str = "\0Date[[DateValue]]";

/// Largest magnitude, in milliseconds, of a valid ECMAScript timestamp.
const MAX_TIME: f64 = 8.64e15;
const MS_PER_DAY: i64 = 86_400_000;
const MS_PER_HOUR: i64 = 3_600_000;
const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_SECOND: i64 = 1_000;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

// ---------------------------------------------------------------------------
// Pure civil-date arithmetic
// ---------------------------------------------------------------------------

/// ECMAScript `TimeClip`: discard fractions and invalid/out-of-range values.
fn time_clip(value: f64) -> f64 {
    if !value.is_finite() || value.abs() > MAX_TIME {
        f64::NAN
    } else {
        value.trunc()
    }
}

/// Current wall-clock time in milliseconds since the Unix epoch.
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_millis() as f64)
}

/// Days since 1970-01-01 for a proleptic Gregorian civil date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + i64::from(day_of_year);
    era * 146097 + day_of_era - 719468
}

/// Inverse of [`days_from_civil`]: `(year, month, day)` with a 1-based month.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146096
    } / 146097;
    let day_of_era = shifted - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = shifted_month + if shifted_month < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// A broken-down UTC timestamp. Local fields are identical (see module docs).
#[derive(Clone, Copy)]
struct DateParts {
    year: i64,
    /// 1-based month, matching [`civil_from_days`].
    month: u32,
    day: u32,
    weekday: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millisecond: u32,
}

/// Split a valid millisecond timestamp into calendar fields.
fn parts_from_ms(ms: f64) -> Option<DateParts> {
    let ms = time_clip(ms);
    if ms.is_nan() {
        return None;
    }
    let total = ms as i64;
    let days = total.div_euclid(MS_PER_DAY);
    let time_of_day = total.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Some(DateParts {
        year,
        month,
        day,
        // 1970-01-01 was a Thursday (Sunday = 0).
        weekday: (days + 4).rem_euclid(7) as u32,
        hour: (time_of_day / MS_PER_HOUR) as u32,
        minute: ((time_of_day % MS_PER_HOUR) / MS_PER_MINUTE) as u32,
        second: ((time_of_day % MS_PER_MINUTE) / MS_PER_SECOND) as u32,
        millisecond: (time_of_day % MS_PER_SECOND) as u32,
    })
}

/// Rebuild a timestamp from calendar fields. `month` is 0-based and
/// out-of-range days are normalised, matching the ECMAScript `MakeDay`
/// algorithm (`new Date(2020, 1, 30)` is 1 March 2020).
fn ms_from_parts(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millisecond: i64,
) -> f64 {
    if !(-31..=31).contains(&day) {
        return f64::NAN;
    }
    let normalized_year = year + month.div_euclid(12);
    let normalized_month = month.rem_euclid(12) + 1;
    let days = days_from_civil(normalized_year, normalized_month as u32, 1) + day - 1;
    let time = (hour as f64) * (MS_PER_HOUR as f64)
        + (minute as f64) * (MS_PER_MINUTE as f64)
        + (second as f64) * (MS_PER_SECOND as f64)
        + millisecond as f64;
    time_clip((days as f64) * (MS_PER_DAY as f64) + time)
}

/// Convert an array of JS numbers into a timestamp, filling missing arguments
/// with `defaults`. Years 0-99 are mapped to 1900-1999 when requested.
fn components_to_ms(
    runtime: &mut Runtime,
    args: &[Value],
    defaults: [f64; 7],
    map_year: bool,
) -> Result<f64, JsError> {
    let mut values = [f64::NAN; 7];
    for (index, value) in values.iter_mut().enumerate() {
        *value = match args.get(index) {
            Some(argument) => runtime.to_number_rt(argument)?,
            None => defaults[index],
        };
    }
    if !values.iter().all(|value| value.is_finite()) {
        return Ok(f64::NAN);
    }
    let integers: Vec<i64> = values.iter().map(|value| value.trunc() as i64).collect();
    let year = if map_year && (0..=99).contains(&integers[0]) {
        integers[0] + 1900
    } else {
        integers[0]
    };
    Ok(ms_from_parts(
        year,
        integers[1],
        integers[2],
        integers[3],
        integers[4],
        integers[5],
        integers[6],
    ))
}

// ---------------------------------------------------------------------------
// String parsing
// ---------------------------------------------------------------------------

/// A tiny cursor used by the ISO 8601 parser.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            position: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn eat(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn digits(&mut self, count: usize) -> Option<i64> {
        let end = self.position.checked_add(count)?;
        let slice = self.bytes.get(self.position..end)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.position = end;
        let mut value = 0_i64;
        for digit in slice {
            value = value * 10 + i64::from(digit - b'0');
        }
        Some(value)
    }

    fn at_end(&self) -> bool {
        self.position == self.bytes.len()
    }
}

fn parse_year(cursor: &mut Cursor<'_>) -> Option<i64> {
    let sign = match cursor.peek() {
        Some(b'+') => {
            cursor.position += 1;
            1
        }
        Some(b'-') => {
            cursor.position += 1;
            -1
        }
        _ => 1,
    };
    // Count the available digits to distinguish four-digit years from the
    // six-digit "expanded" years used outside the range 0000-9999.
    let mut end = cursor.position;
    while cursor
        .bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_digit())
    {
        end += 1;
    }
    let count = if sign != 1 || end - cursor.position >= 6 {
        6
    } else {
        4
    };
    let digits = cursor.digits(count)?;
    Some(sign * digits)
}

/// Parse an ISO 8601 date/time string. Returns milliseconds or `None` when the
/// text does not match a supported form. Local times (no zone suffix) are UTC.
fn parse_iso(text: &str) -> Option<f64> {
    let cursor = &mut Cursor::new(text);
    let year = parse_year(cursor)?;
    if !cursor.eat(b'-') {
        return None;
    }
    let month = cursor.digits(2)?;
    if !cursor.eat(b'-') {
        return None;
    }
    let day = cursor.digits(2)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || day > i64::from(days_in_month(year, month as u32))
    {
        return None;
    }

    let mut hour = 0;
    let mut minute = 0;
    let mut second = 0;
    let mut millisecond = 0;
    let mut offset_minutes = 0_i64;

    if !cursor.at_end() {
        if !cursor.eat(b'T') && !cursor.eat(b't') && !cursor.eat(b' ') {
            return None;
        }
        hour = cursor.digits(2)?;
        if !cursor.eat(b':') {
            return None;
        }
        minute = cursor.digits(2)?;
        if cursor.eat(b':') {
            second = cursor.digits(2)?;
        }
        if cursor.eat(b'.') {
            let start = cursor.position;
            while cursor.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                cursor.position += 1;
            }
            let count = cursor.position - start;
            if count == 0 {
                return None;
            }
            let mut fraction = 0_i64;
            for digit in &cursor.bytes[start..cursor.position] {
                fraction = fraction * 10 + i64::from(digit - b'0');
            }
            // JS truncates extra fractional digits and pads short ones.
            millisecond = if count <= 3 {
                fraction * 10_i64.pow((3 - count) as u32)
            } else {
                fraction / 10_i64.pow((count - 3) as u32)
            };
        }
        // `24:00:00` is the one valid out-of-range hour in ISO 8601.
        if hour > 24
            || minute > 59
            || second > 59
            || (hour == 24 && (minute != 0 || second != 0 || millisecond != 0))
        {
            return None;
        }

        match cursor.peek() {
            Some(b'Z' | b'z') => {
                cursor.position += 1;
            }
            Some(b'+' | b'-') => {
                let negative = cursor.peek() == Some(b'-');
                cursor.position += 1;
                let offset_hours = cursor.digits(2)?;
                let offset_mins = if cursor.eat(b':') {
                    cursor.digits(2)?
                } else {
                    // Offsets without a colon are written as `+HHMM`.
                    cursor.digits(2)?
                };
                if offset_hours > 23 || offset_mins > 59 {
                    return None;
                }
                offset_minutes = (offset_hours * 60 + offset_mins) * if negative { -1 } else { 1 };
            }
            _ => {}
        }
    }
    if !cursor.at_end() {
        return None;
    }
    // `24:00:00` denotes midnight at the start of the following day.
    let next_day = hour == 24;
    let value = ms_from_parts(
        year,
        month - 1,
        day + i64::from(next_day),
        if next_day { 0 } else { hour },
        minute,
        second,
        millisecond,
    );
    if value.is_nan() {
        return None;
    }
    Some(value - offset_minutes as f64 * MS_PER_MINUTE as f64)
}

fn month_number(token: &str) -> Option<u32> {
    let prefix = token.as_bytes().get(..3)?;
    MONTHS
        .iter()
        .position(|name| name.as_bytes()[..3].eq_ignore_ascii_case(prefix))
        .map(|index| index as u32 + 1)
}

fn parse_hms(token: &str) -> Option<(i64, i64, i64)> {
    let mut parts = token.split(':');
    let hour = parts.next()?.parse().ok()?;
    let minute = parts.next()?.parse().ok()?;
    let second = match parts.next() {
        Some(value) => value.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((hour, minute, second))
}

/// Parse RFC 1123 / asctime strings so `toUTCString()` and `toString()`
/// round-trip through `Date.parse`.
fn parse_named(text: &str) -> Option<f64> {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch == ',' { ' ' } else { ch })
        .collect();
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    let month_index = tokens
        .iter()
        .position(|token| month_number(token).is_some())?;
    let month = month_number(tokens[month_index])?;
    let mut day = None;
    let mut year = None;
    let mut time = None;
    let mut offset_minutes = 0_i64;
    for (index, token) in tokens.iter().enumerate() {
        if index == month_index {
            continue;
        }
        if let Ok(number) = token.parse::<u32>() {
            if number <= 31 && day.is_none() {
                day = Some(number);
            } else if year.is_none() {
                year = Some(i64::from(number));
            }
        } else if token.contains(':') {
            time = Some(*token);
        } else if token.eq_ignore_ascii_case("GMT") || token.eq_ignore_ascii_case("UTC") {
            offset_minutes = 0;
        } else if token.len() >= 5 && matches!(token.as_bytes()[0], b'+' | b'-') {
            let negative = token.starts_with('-');
            let digits = &token[1..];
            let (hours, minutes) = if digits.len() == 4 {
                (&digits[0..2], &digits[2..4])
            } else {
                continue;
            };
            offset_minutes = (hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?)
                * if negative { -1 } else { 1 };
        }
    }
    let (hour, minute, second) = parse_hms(time?)?;
    let day = day?;
    let mut year = year?;
    if (0..100).contains(&year) {
        year += 1900;
    }
    let value = ms_from_parts(year, month as i64 - 1, day as i64, hour, minute, second, 0);
    if value.is_nan() {
        return None;
    }
    Some(value - offset_minutes as f64 * MS_PER_MINUTE as f64)
}

/// The entry point used by both `Date.parse` and the string constructor.
fn parse_date_string(text: &str) -> f64 {
    let trimmed = text.trim();
    parse_iso(trimmed)
        .or_else(|| parse_named(trimmed))
        .map_or(f64::NAN, time_clip)
}

// ---------------------------------------------------------------------------
// String formatting
// ---------------------------------------------------------------------------

fn padded(value: u32, width: usize) -> String {
    format!("{value:0width$}")
}

fn iso_year(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{year:06}")
    } else {
        format!("+{year:06}")
    }
}

fn display_year(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        iso_year(year)
    }
}

fn weekday(parts: &DateParts) -> &'static str {
    WEEKDAYS[parts.weekday as usize]
}

fn month_name(parts: &DateParts) -> &'static str {
    MONTHS[(parts.month - 1) as usize]
}

fn format_iso(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{}-{}-{}T{}:{}:{}.{}Z",
        iso_year(parts.year),
        padded(parts.month, 2),
        padded(parts.day, 2),
        padded(parts.hour, 2),
        padded(parts.minute, 2),
        padded(parts.second, 2),
        padded(parts.millisecond, 3),
    ))
}

fn format_utc_string(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{}, {} {} {} {}:{}:{} GMT",
        weekday(&parts),
        padded(parts.day, 2),
        month_name(&parts),
        display_year(parts.year),
        padded(parts.hour, 2),
        padded(parts.minute, 2),
        padded(parts.second, 2),
    ))
}

fn format_date_string(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{} {} {} {}",
        weekday(&parts),
        month_name(&parts),
        padded(parts.day, 2),
        display_year(parts.year),
    ))
}

fn format_time_string(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{}:{}:{} GMT+0000 (Coordinated Universal Time)",
        padded(parts.hour, 2),
        padded(parts.minute, 2),
        padded(parts.second, 2),
    ))
}

fn format_to_string(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{} {} {} {} {}:{}:{} GMT+0000 (Coordinated Universal Time)",
        weekday(&parts),
        month_name(&parts),
        padded(parts.day, 2),
        display_year(parts.year),
        padded(parts.hour, 2),
        padded(parts.minute, 2),
        padded(parts.second, 2),
    ))
}

fn format_locale_date(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{}/{}/{}",
        parts.month,
        padded(parts.day, 2),
        display_year(parts.year),
    ))
}

fn format_locale_time(ms: f64) -> Option<String> {
    let parts = parts_from_ms(ms)?;
    Some(format!(
        "{}:{}:{}",
        padded(parts.hour, 2),
        padded(parts.minute, 2),
        padded(parts.second, 2),
    ))
}

// ---------------------------------------------------------------------------
// Host glue
// ---------------------------------------------------------------------------

/// Read the internal timestamp from a Date receiver.
fn this_ms(runtime: &Runtime, this: &Value) -> Result<f64, JsError> {
    match this {
        Value::Object(id) => match runtime.get_own_property(*id, DATE_VALUE_SLOT) {
            Some(Value::Number(ms)) => Ok(ms),
            _ => Err(JsError::type_error("this is not a Date object")),
        },
        _ => Err(JsError::type_error("this is not a Date object")),
    }
}

/// Write the internal timestamp of an existing Date receiver.
fn set_this_ms(runtime: &mut Runtime, this: &Value, ms: f64) -> Result<f64, JsError> {
    let Value::Object(id) = this else {
        return Err(JsError::type_error("this is not a Date object"));
    };
    if runtime.get_own_property(*id, DATE_VALUE_SLOT).is_none() {
        return Err(JsError::type_error("this is not a Date object"));
    }
    let ms = time_clip(ms);
    runtime.define_data_property(*id, DATE_VALUE_SLOT, Value::Number(ms));
    Ok(ms)
}

/// Compute the timestamp for the `new Date(...)` argument list.
fn constructor_ms(runtime: &mut Runtime, args: &[Value]) -> Result<f64, JsError> {
    match args {
        [] => Ok(now_ms()),
        [only] => match only {
            Value::Undefined => Ok(f64::NAN),
            Value::String(text) => Ok(parse_date_string(text)),
            Value::Object(_) => match runtime.to_primitive(only)? {
                Value::String(text) => Ok(parse_date_string(&text)),
                primitive => Ok(time_clip(primitive.to_number())),
            },
            primitive => Ok(time_clip(primitive.to_number())),
        },
        _ => components_to_ms(
            runtime,
            args,
            [f64::NAN, f64::NAN, 1.0, 0.0, 0.0, 0.0, 0.0],
            true,
        ),
    }
}

fn date_constructor(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    // A plain `Date(...)` call receives `undefined` as `this`; `new Date(...)`
    // receives the freshly allocated instance created by `[[Construct]]`.
    let Value::Object(id) = this else {
        return Ok(Value::String(
            format_to_string(now_ms()).unwrap_or_else(|| "Invalid Date".to_owned()),
        ));
    };
    let ms = constructor_ms(runtime, &args)?;
    runtime.define_data_property(id, DATE_VALUE_SLOT, Value::Number(time_clip(ms)));
    Ok(Value::Undefined)
}

fn date_now(_runtime: &mut Runtime, _this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    Ok(Value::Number(now_ms()))
}

fn date_parse(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let text = runtime.to_string_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(parse_date_string(&text)))
}

fn date_utc(runtime: &mut Runtime, _this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let ms = components_to_ms(
        runtime,
        &args,
        [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        true,
    )?;
    Ok(Value::Number(ms))
}

/// Generate a getter that reads one field from the receiver's `[[DateValue]]`.
macro_rules! part_getter {
    ($name:ident, $field:ident) => {
        fn $name(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
            let ms = this_ms(runtime, &this)?;
            Ok(Value::Number(
                parts_from_ms(ms).map_or(f64::NAN, |parts| parts.$field as f64),
            ))
        }
    };
    ($name:ident, $field:ident, $delta:expr) => {
        fn $name(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
            let ms = this_ms(runtime, &this)?;
            Ok(Value::Number(
                parts_from_ms(ms).map_or(f64::NAN, |parts| parts.$field as f64 + $delta),
            ))
        }
    };
}

fn date_get_time(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    Ok(Value::Number(this_ms(runtime, &this)?))
}

fn date_value_of(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    date_get_time(runtime, this, args)
}

part_getter!(date_get_full_year, year);
part_getter!(date_get_month, month, -1.0);
part_getter!(date_get_date, day);
part_getter!(date_get_day, weekday);
part_getter!(date_get_hours, hour);
part_getter!(date_get_minutes, minute);
part_getter!(date_get_seconds, second);
part_getter!(date_get_milliseconds, millisecond);

fn date_get_year(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let full_year = date_get_full_year(runtime, this, args)?;
    match full_year {
        Value::Number(year) => Ok(Value::Number(year - 1900.0)),
        other => Ok(other),
    }
}

fn date_get_timezone_offset(
    _runtime: &mut Runtime,
    _this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    // Local time is UTC in this runtime.
    Ok(Value::Number(0.0))
}

fn iso_formatter(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
    format: fn(f64) -> Option<String>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    match format(ms) {
        Some(text) => Ok(Value::String(text)),
        None => Err(JsError::range_error("Invalid time value")),
    }
}

fn date_to_iso_string(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    iso_formatter(runtime, this, args, format_iso)
}

fn date_to_utc_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    Ok(Value::String(
        format_utc_string(ms).unwrap_or_else(|| "Invalid Date".to_owned()),
    ))
}

fn date_to_date_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    Ok(Value::String(
        format_date_string(ms).unwrap_or_else(|| "Invalid Date".to_owned()),
    ))
}

fn date_to_time_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    Ok(Value::String(
        format_time_string(ms).unwrap_or_else(|| "Invalid Date".to_owned()),
    ))
}

fn date_to_string(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    Ok(Value::String(
        format_to_string(ms).unwrap_or_else(|| "Invalid Date".to_owned()),
    ))
}

fn date_to_locale_date_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    Ok(Value::String(
        format_locale_date(ms).unwrap_or_else(|| "Invalid Date".to_owned()),
    ))
}

fn date_to_locale_time_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    Ok(Value::String(
        format_locale_time(ms).unwrap_or_else(|| "Invalid Date".to_owned()),
    ))
}

fn date_to_locale_string(
    runtime: &mut Runtime,
    this: Value,
    _args: Vec<Value>,
) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    let text = match (format_locale_date(ms), format_locale_time(ms)) {
        (Some(date), Some(time)) => format!("{date}, {time}"),
        _ => "Invalid Date".to_owned(),
    };
    Ok(Value::String(text))
}

fn date_to_json(runtime: &mut Runtime, this: Value, _args: Vec<Value>) -> Result<Value, JsError> {
    let ms = this_ms(runtime, &this)?;
    match format_iso(ms) {
        Some(text) => Ok(Value::String(text)),
        None => Ok(Value::Null),
    }
}

fn date_set_time(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    let time = runtime.to_number_rt(&args.first().cloned().unwrap_or(Value::Undefined))?;
    Ok(Value::Number(set_this_ms(runtime, &this, time)?))
}

/// Apply argument overrides to a receiver's broken-down date and store the
/// result. `overrides` maps a component index (0=year ... 6=millisecond) to an
/// argument index; components not mentioned keep their current value.
fn set_components(
    runtime: &mut Runtime,
    this: Value,
    args: &[Value],
    overrides: &[(usize, usize)],
    map_year: bool,
) -> Result<Value, JsError> {
    let old = this_ms(runtime, &this)?;
    let parts = parts_from_ms(old);
    let mut values = [f64::NAN; 7];
    if let Some(parts) = parts {
        values[0] = parts.year as f64;
        values[1] = f64::from(parts.month) - 1.0;
        values[2] = f64::from(parts.day);
        values[3] = f64::from(parts.hour);
        values[4] = f64::from(parts.minute);
        values[5] = f64::from(parts.second);
        values[6] = f64::from(parts.millisecond);
    }
    for &(component, argument) in overrides {
        if let Some(value) = args.get(argument) {
            values[component] = runtime.to_number_rt(value)?;
        }
    }
    if !values.iter().all(|value| value.is_finite()) {
        return Ok(Value::Number(set_this_ms(runtime, &this, f64::NAN)?));
    }
    let integers: Vec<i64> = values.iter().map(|value| value.trunc() as i64).collect();
    let year = if map_year && (0..=99).contains(&integers[0]) {
        integers[0] + 1900
    } else {
        integers[0]
    };
    let ms = ms_from_parts(
        year,
        integers[1],
        integers[2],
        integers[3],
        integers[4],
        integers[5],
        integers[6],
    );
    Ok(Value::Number(set_this_ms(runtime, &this, ms)?))
}

fn date_set_milliseconds(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(6, 0)], false)
}

fn date_set_seconds(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(5, 0), (6, 1)], false)
}

fn date_set_minutes(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(4, 0), (5, 1), (6, 2)], false)
}

fn date_set_hours(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    set_components(
        runtime,
        this,
        &args,
        &[(3, 0), (4, 1), (5, 2), (6, 3)],
        false,
    )
}

fn date_set_date(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(2, 0)], false)
}

fn date_set_month(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(1, 0), (2, 1)], false)
}

fn date_set_full_year(
    runtime: &mut Runtime,
    this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(0, 0), (1, 1), (2, 2)], true)
}

fn date_set_year(runtime: &mut Runtime, this: Value, args: Vec<Value>) -> Result<Value, JsError> {
    set_components(runtime, this, &args, &[(0, 0)], true)
}

impl Runtime {
    /// Install the `Date` constructor, its prototype and static methods.
    pub(crate) fn install_date(&mut self) {
        let prototype = self.bootstrap_object(Some(self.prototypes.object));
        self.define_builtin_method(prototype, "getTime", 0, date_get_time);
        self.define_builtin_method(prototype, "valueOf", 0, date_value_of);
        self.define_builtin_method(prototype, "getFullYear", 0, date_get_full_year);
        self.define_builtin_method(prototype, "getUTCFullYear", 0, date_get_full_year);
        self.define_builtin_method(prototype, "getMonth", 0, date_get_month);
        self.define_builtin_method(prototype, "getUTCMonth", 0, date_get_month);
        self.define_builtin_method(prototype, "getDate", 0, date_get_date);
        self.define_builtin_method(prototype, "getUTCDate", 0, date_get_date);
        self.define_builtin_method(prototype, "getDay", 0, date_get_day);
        self.define_builtin_method(prototype, "getUTCDay", 0, date_get_day);
        self.define_builtin_method(prototype, "getHours", 0, date_get_hours);
        self.define_builtin_method(prototype, "getUTCHours", 0, date_get_hours);
        self.define_builtin_method(prototype, "getMinutes", 0, date_get_minutes);
        self.define_builtin_method(prototype, "getUTCMinutes", 0, date_get_minutes);
        self.define_builtin_method(prototype, "getSeconds", 0, date_get_seconds);
        self.define_builtin_method(prototype, "getUTCSeconds", 0, date_get_seconds);
        self.define_builtin_method(prototype, "getMilliseconds", 0, date_get_milliseconds);
        self.define_builtin_method(prototype, "getUTCMilliseconds", 0, date_get_milliseconds);
        self.define_builtin_method(prototype, "getYear", 0, date_get_year);
        self.define_builtin_method(prototype, "getTimezoneOffset", 0, date_get_timezone_offset);
        self.define_builtin_method(prototype, "toISOString", 0, date_to_iso_string);
        self.define_builtin_method(prototype, "toUTCString", 0, date_to_utc_string);
        self.define_builtin_method(prototype, "toGMTString", 0, date_to_utc_string);
        self.define_builtin_method(prototype, "toDateString", 0, date_to_date_string);
        self.define_builtin_method(prototype, "toTimeString", 0, date_to_time_string);
        self.define_builtin_method(prototype, "toString", 0, date_to_string);
        self.define_builtin_method(
            prototype,
            "toLocaleDateString",
            0,
            date_to_locale_date_string,
        );
        self.define_builtin_method(
            prototype,
            "toLocaleTimeString",
            0,
            date_to_locale_time_string,
        );
        self.define_builtin_method(prototype, "toLocaleString", 0, date_to_locale_string);
        self.define_builtin_method(prototype, "toJSON", 1, date_to_json);
        self.define_builtin_method(prototype, "setTime", 1, date_set_time);
        self.define_builtin_method(prototype, "setMilliseconds", 1, date_set_milliseconds);
        self.define_builtin_method(prototype, "setUTCMilliseconds", 1, date_set_milliseconds);
        self.define_builtin_method(prototype, "setSeconds", 1, date_set_seconds);
        self.define_builtin_method(prototype, "setUTCSeconds", 1, date_set_seconds);
        self.define_builtin_method(prototype, "setMinutes", 1, date_set_minutes);
        self.define_builtin_method(prototype, "setUTCMinutes", 1, date_set_minutes);
        self.define_builtin_method(prototype, "setHours", 1, date_set_hours);
        self.define_builtin_method(prototype, "setUTCHours", 1, date_set_hours);
        self.define_builtin_method(prototype, "setDate", 1, date_set_date);
        self.define_builtin_method(prototype, "setUTCDate", 1, date_set_date);
        self.define_builtin_method(prototype, "setMonth", 1, date_set_month);
        self.define_builtin_method(prototype, "setUTCMonth", 1, date_set_month);
        self.define_builtin_method(prototype, "setFullYear", 1, date_set_full_year);
        self.define_builtin_method(prototype, "setUTCFullYear", 1, date_set_full_year);
        self.define_builtin_method(prototype, "setYear", 1, date_set_year);

        let constructor = self.new_builtin_value("Date", 7, date_constructor);
        if let Value::Object(id) = &constructor {
            self.define_data_property(*id, "prototype", Value::Object(prototype));
            self.define_data_property(prototype, "constructor", constructor.clone());
            self.define_builtin_method(*id, "now", 0, date_now);
            self.define_builtin_method(*id, "parse", 1, date_parse);
            self.define_builtin_method(*id, "UTC", 7, date_utc);
        }
        self.declare_global("Date", constructor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(runtime: &mut Runtime, source: &str) -> Value {
        runtime
            .eval(source)
            .unwrap_or_else(|error| panic!("eval failed for {source:?}: {error}"))
    }

    fn number(runtime: &mut Runtime, source: &str) -> f64 {
        eval(runtime, source).to_number()
    }

    fn text(runtime: &mut Runtime, source: &str) -> String {
        eval(runtime, source).to_string_value()
    }

    #[test]
    fn civil_conversions_roundtrip() {
        for days in [-10_000, -1, 0, 1, 1_000, 18_000, 20_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn known_epoch_and_leap_values() {
        assert_eq!(ms_from_parts(1970, 0, 1, 0, 0, 0, 0), 0.0);
        assert_eq!(ms_from_parts(2000, 0, 1, 0, 0, 0, 0), 946_684_800_000.0);
        assert_eq!(
            ms_from_parts(2020, 1, 29, 12, 34, 56, 789),
            1_582_979_696_789.0
        );
        let parts = parts_from_ms(1_582_979_696_789.0).unwrap();
        assert_eq!((parts.year, parts.month, parts.day), (2020, 2, 29));
        assert_eq!((parts.hour, parts.minute, parts.second), (12, 34, 56));
    }

    #[test]
    fn time_clip_rejects_invalid_values() {
        assert!(time_clip(f64::NAN).is_nan());
        assert!(time_clip(f64::INFINITY).is_nan());
        assert!(time_clip(8.64e15 + 1.0).is_nan());
        assert_eq!(time_clip(1.9), 1.0);
    }

    #[test]
    fn construction_and_numeric_getters() {
        let mut runtime = Runtime::new();
        assert_eq!(number(&mut runtime, "new Date(0).getTime()"), 0.0);
        assert_eq!(number(&mut runtime, "new Date(1500).valueOf()"), 1500.0);
        assert_eq!(number(&mut runtime, "+new Date(2500)"), 2500.0);
        assert_eq!(
            number(&mut runtime, "new Date(2020, 0, 2).getFullYear()"),
            2020.0
        );
        assert_eq!(number(&mut runtime, "new Date(2020, 0, 2).getMonth()"), 0.0);
        assert_eq!(number(&mut runtime, "new Date(2020, 0, 2).getDate()"), 2.0);
        assert_eq!(number(&mut runtime, "new Date(2020, 0, 2).getDay()"), 4.0);
        assert_eq!(
            number(&mut runtime, "new Date(2020, 0, 2, 3, 4, 5, 6).getHours()"),
            3.0
        );
        assert_eq!(
            number(
                &mut runtime,
                "new Date(2020, 0, 2, 3, 4, 5, 6).getMinutes()"
            ),
            4.0
        );
        assert_eq!(
            number(
                &mut runtime,
                "new Date(2020, 0, 2, 3, 4, 5, 6).getSeconds()"
            ),
            5.0
        );
        assert_eq!(
            number(
                &mut runtime,
                "new Date(2020, 0, 2, 3, 4, 5, 6).getMilliseconds()"
            ),
            6.0
        );
    }

    #[test]
    fn two_digit_years_and_missing_components() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(&mut runtime, "new Date(98, 0, 1).getFullYear()"),
            1998.0
        );
        assert_eq!(
            text(&mut runtime, "new Date(2020, 0).toISOString()"),
            "2020-01-01T00:00:00.000Z"
        );
        assert_eq!(
            number(
                &mut runtime,
                "new Date(2020, 1, 30).toISOString() === '2020-03-01T00:00:00.000Z'"
            ),
            1.0
        );
    }

    #[test]
    fn utc_getters_match_local_getters() {
        let mut runtime = Runtime::new();
        let getters = [
            "FullYear",
            "Month",
            "Date",
            "Day",
            "Hours",
            "Minutes",
            "Seconds",
            "Milliseconds",
        ];
        for getter in getters {
            let source =
                format!("var d = new Date(1583037296789); d.get{getter}() === d.getUTC{getter}()");
            assert_eq!(eval(&mut runtime, &source), Value::Bool(true), "{getter}");
        }
        assert_eq!(number(&mut runtime, "new Date(0).getTimezoneOffset()"), 0.0);
    }

    #[test]
    fn iso_roundtrip() {
        let mut runtime = Runtime::new();
        let iso = "2021-06-15T12:34:56.789Z";
        eval(&mut runtime, &format!("var d = new Date('{iso}');"));
        assert_eq!(text(&mut runtime, "d.toISOString()"), iso);
        assert_eq!(
            number(&mut runtime, "Date.parse('2021-06-15T12:34:56.789Z')"),
            number(&mut runtime, "d.getTime()")
        );
        // Offsets are applied.
        assert_eq!(
            text(
                &mut runtime,
                "new Date('2021-06-15T14:34:56.789+02:00').toISOString()"
            ),
            iso
        );
        assert_eq!(
            text(
                &mut runtime,
                "new Date('2021-06-15T08:34:56.789-04:00').toISOString()"
            ),
            iso
        );
        assert_eq!(
            text(&mut runtime, "new Date('2021-06-15').toISOString()"),
            "2021-06-15T00:00:00.000Z"
        );
        assert_eq!(
            text(
                &mut runtime,
                "new Date('2021-06-15T00:00:00.123456Z').toISOString()"
            ),
            "2021-06-15T00:00:00.123Z"
        );
        assert_eq!(
            text(
                &mut runtime,
                "new Date('2021-06-15T24:00:00Z').toISOString()"
            ),
            "2021-06-16T00:00:00.000Z"
        );
    }

    #[test]
    fn json_stringify_uses_to_json() {
        let mut runtime = Runtime::new();
        assert_eq!(
            text(&mut runtime, "JSON.stringify(new Date(0))"),
            "\"1970-01-01T00:00:00.000Z\""
        );
        assert_eq!(
            text(&mut runtime, "JSON.stringify({ when: new Date(0) })"),
            "{\"when\":\"1970-01-01T00:00:00.000Z\"}"
        );
        assert_eq!(text(&mut runtime, "JSON.stringify(new Date(NaN))"), "null");
    }

    #[test]
    fn invalid_dates_and_parsing() {
        let mut runtime = Runtime::new();
        assert!(number(&mut runtime, "new Date('not a date').getTime()").is_nan());
        assert!(number(&mut runtime, "new Date('2020-13-01').getTime()").is_nan());
        assert!(number(&mut runtime, "Date.parse('nope')").is_nan());
        assert!(number(&mut runtime, "new Date(NaN).getTime()").is_nan());
        assert!(number(&mut runtime, "new Date(undefined).getTime()").is_nan());
        assert!(runtime.eval("new Date(NaN).toISOString()").is_err());
        assert_eq!(
            text(&mut runtime, "new Date(NaN).toString()"),
            "Invalid Date"
        );
        assert_eq!(number(&mut runtime, "new Date(null).getTime()"), 0.0);
    }

    #[test]
    fn statics_now_parse_utc() {
        let mut runtime = Runtime::new();
        assert!(number(&mut runtime, "Date.now()") > 1_500_000_000_000.0);
        assert_eq!(number(&mut runtime, "Date.UTC(1970, 0, 1)"), 0.0);
        assert_eq!(
            number(&mut runtime, "Date.UTC(2020, 1, 29)"),
            1_582_934_400_000.0
        );
        assert_eq!(
            number(
                &mut runtime,
                "new Date(Date.UTC(2020, 0, 2, 3, 4, 5, 6)).getUTCHours()"
            ),
            3.0
        );
    }

    #[test]
    fn setters_mutate_the_receiver() {
        let mut runtime = Runtime::new();
        assert_eq!(
            text(
                &mut runtime,
                "var d = new Date(0); d.setFullYear(2020); d.toISOString()"
            ),
            "2020-01-01T00:00:00.000Z"
        );
        assert_eq!(
            text(
                &mut runtime,
                "d.setMonth(5); d.setDate(15); d.setHours(12, 34, 56, 789); d.toISOString()"
            ),
            "2020-06-15T12:34:56.789Z"
        );
        assert_eq!(number(&mut runtime, "d.setTime(0)"), 0.0);
        assert_eq!(
            text(&mut runtime, "d.toISOString()"),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(
            number(
                &mut runtime,
                "d.setUTCFullYear(98) === Date.UTC(1998, 0, 1)"
            ),
            1.0
        );
    }

    #[test]
    fn date_arithmetic_and_comparison() {
        let mut runtime = Runtime::new();
        assert_eq!(
            number(
                &mut runtime,
                "var start = new Date('2020-01-01T00:00:00.000Z'); \
                 var end = new Date(start.getTime() + 86400000); \
                 end.getTime() - start.getTime()"
            ),
            86_400_000.0
        );
        assert_eq!(
            eval(
                &mut runtime,
                "new Date(1000) < new Date(2000) && new Date(2000) - new Date(1000) === 1000"
            ),
            Value::Bool(true)
        );
    }

    #[test]
    fn string_formats_and_named_roundtrip() {
        let mut runtime = Runtime::new();
        eval(
            &mut runtime,
            "var d = new Date('2020-06-15T12:34:56.789Z');",
        );
        assert_eq!(
            text(&mut runtime, "d.toUTCString()"),
            "Mon, 15 Jun 2020 12:34:56 GMT"
        );
        assert_eq!(text(&mut runtime, "d.toDateString()"), "Mon Jun 15 2020");
        assert_eq!(
            text(&mut runtime, "d.toString()"),
            "Mon Jun 15 2020 12:34:56 GMT+0000 (Coordinated Universal Time)"
        );
        assert_eq!(text(&mut runtime, "d.toLocaleDateString()"), "6/15/2020");
        assert_eq!(text(&mut runtime, "d.toLocaleTimeString()"), "12:34:56");
        // The named formats are second-precision, so round-trip a date whose
        // millisecond field is zero.
        assert_eq!(
            number(
                &mut runtime,
                "var whole = new Date(Date.UTC(2020, 5, 15, 12, 34, 56)); \
                 Date.parse(whole.toUTCString()) === whole.getTime()"
            ),
            1.0
        );
        assert_eq!(
            number(
                &mut runtime,
                "Date.parse(whole.toString()) === whole.getTime()"
            ),
            1.0
        );
    }

    #[test]
    fn date_as_function_and_instances() {
        let mut runtime = Runtime::new();
        let string = text(&mut runtime, "Date()");
        assert!(string.contains("GMT+0000") && string.len() > 10, "{string}");
        assert_eq!(
            eval(&mut runtime, "typeof Date"),
            Value::String("function".into())
        );
        assert_eq!(
            eval(&mut runtime, "new Date(0) instanceof Date"),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&mut runtime, "Date.prototype.getTime !== undefined"),
            Value::Bool(true)
        );
        assert_eq!(
            eval(&mut runtime, "new Date(0).constructor === Date"),
            Value::Bool(true)
        );
    }
}
