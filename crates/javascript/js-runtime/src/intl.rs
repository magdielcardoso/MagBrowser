//! A small `Intl` implementation covering `NumberFormat` and
//! `DateTimeFormat` for the `en-US` and `pt-BR` locales.
//!
//! Formatting is done with plain Rust code (no ICU dependency). It covers the
//! common decimal/grouping/currency/percent cases and the usual date and time
//! component styles; less common options fall back to sensible defaults.

use crate::Runtime;
use crate::errors::JsError;
use crate::value::Value;

impl Runtime {
    /// Install the `Intl` namespace.
    pub(crate) fn install_intl(&mut self) {
        let intl = self.new_object();
        let Value::Object(id) = intl else {
            return;
        };
        let number_format =
            self.new_builtin_value("NumberFormat", 0, intl_number_format_constructor);
        let date_time_format =
            self.new_builtin_value("DateTimeFormat", 0, intl_date_time_format_constructor);
        self.define_data_property(id, "NumberFormat", number_format);
        self.define_data_property(id, "DateTimeFormat", date_time_format);
        self.declare_global("Intl", intl);
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Locale {
    EnUs,
    PtBr,
}

impl Locale {
    fn tag(self) -> &'static str {
        match self {
            Self::EnUs => "en-US",
            Self::PtBr => "pt-BR",
        }
    }
}

fn parse_locale(runtime: &mut Runtime, value: Option<&Value>) -> Locale {
    let Some(value) = value else {
        return Locale::EnUs;
    };
    let text = match value {
        Value::Object(_) => runtime.object_array_items(value).and_then(|items| {
            items
                .first()
                .map(|item| item.to_string_value().to_ascii_lowercase())
        }),
        Value::String(text) => Some(text.to_ascii_lowercase()),
        _ => None,
    };
    match text.as_deref() {
        Some(text) if text.starts_with("pt") => Locale::PtBr,
        _ => Locale::EnUs,
    }
}

fn option_string(runtime: &mut Runtime, options: &Value, name: &str) -> Option<String> {
    let value = runtime.get_property(options, name);
    match value {
        Value::String(text) => Some(text),
        _ => None,
    }
}

fn option_number(runtime: &mut Runtime, options: &Value, name: &str) -> Option<f64> {
    match runtime.get_property(options, name) {
        Value::Number(number) => Some(number),
        _ => None,
    }
}

fn option_bool(runtime: &mut Runtime, options: &Value, name: &str) -> Option<bool> {
    match runtime.get_property(options, name) {
        Value::Bool(value) => Some(value),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// NumberFormat
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct NumberFormatState {
    locale: Locale,
    style: String,
    currency: String,
    use_grouping: bool,
    min_fraction: usize,
    max_fraction: usize,
}

fn intl_number_format_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let locale = parse_locale(runtime, args.first());
    let options = args.get(1).cloned().unwrap_or_else(|| runtime.new_object());
    let style = option_string(runtime, &options, "style").unwrap_or_else(|| "decimal".to_owned());
    let currency = option_string(runtime, &options, "currency").unwrap_or_else(|| "USD".to_owned());
    let use_grouping = option_bool(runtime, &options, "useGrouping").unwrap_or(true);
    let (default_min, default_max) = if style == "currency" { (2, 2) } else { (0, 3) };
    let min_fraction = option_number(runtime, &options, "minimumFractionDigits")
        .map(|value| value as usize)
        .unwrap_or(default_min);
    let max_fraction = option_number(runtime, &options, "maximumFractionDigits")
        .map(|value| value as usize)
        .unwrap_or(default_max);
    let state = NumberFormatState {
        locale,
        style,
        currency,
        use_grouping,
        min_fraction: min_fraction.min(max_fraction),
        max_fraction,
    };

    let locale_tag = state.locale.tag().to_owned();
    let object = runtime.new_object();
    let Value::Object(id) = object else {
        return Ok(object);
    };
    let captured = state.clone();
    let format = runtime.new_native_value(
        "format",
        1,
        Box::new(move |_runtime, _this, args| {
            let value = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_number();
            Ok(Value::String(format_number(&captured, value)))
        }),
    );
    let resolved_state = state.clone();
    let resolved = runtime.new_native_value(
        "resolvedOptions",
        0,
        Box::new(move |runtime, _this, _args| Ok(number_format_options(runtime, &resolved_state))),
    );
    runtime.define_data_property(id, "format", format);
    runtime.define_data_property(id, "resolvedOptions", resolved);
    runtime.define_data_property(id, "locale", Value::String(locale_tag));
    Ok(object)
}

fn format_number(state: &NumberFormatState, mut value: f64) -> String {
    let suffix = if state.style == "percent" {
        value *= 100.0;
        "%"
    } else {
        ""
    };
    let negative = value.is_sign_negative() && value != 0.0;
    let magnitude = value.abs();
    let decimals = state.max_fraction.min(20);
    let mut digits = format!("{magnitude:.*}", decimals);
    // Trim trailing fractional zeros down to the configured minimum.
    if let Some((integer, fraction)) = digits.split_once('.') {
        let trimmed = fraction.trim_end_matches('0');
        if trimmed.len() < state.min_fraction {
            digits = format!(
                "{integer}.{trimmed}{}",
                "0".repeat(state.min_fraction - trimmed.len())
            );
        } else {
            let fraction = &trimmed[..trimmed.len().min(state.max_fraction)];
            digits = if fraction.is_empty() {
                integer.to_owned()
            } else {
                format!("{integer}.{fraction}")
            };
        }
    }
    let (integer, fraction) = match digits.split_once('.') {
        Some((integer, fraction)) => (integer.to_owned(), Some(fraction.to_owned())),
        None => (digits, None),
    };
    let (group_separator, decimal_separator) = match state.locale {
        Locale::EnUs => (',', '.'),
        Locale::PtBr => ('.', ','),
    };
    let mut grouped = String::new();
    for (index, ch) in integer.chars().enumerate() {
        if index > 0 && state.use_grouping && (integer.len() - index) % 3 == 0 {
            grouped.push(group_separator);
        }
        grouped.push(ch);
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if state.style == "currency" {
        out.push_str(&currency_prefix(state.locale, &state.currency));
    }
    out.push_str(&grouped);
    if let Some(fraction) = fraction {
        out.push(decimal_separator);
        out.push_str(&fraction);
    }
    out.push_str(suffix);
    out
}

fn currency_prefix(locale: Locale, currency: &str) -> String {
    match (locale, currency) {
        (Locale::EnUs, "USD") => "$".to_owned(),
        (Locale::PtBr, "BRL") => "R$ ".to_owned(),
        (_, code) => format!("{code} "),
    }
}

fn number_format_options(runtime: &mut Runtime, state: &NumberFormatState) -> Value {
    let object = runtime.new_object();
    runtime.set_property(
        object.clone(),
        "locale",
        Value::String(state.locale.tag().to_owned()),
    );
    runtime.set_property(object.clone(), "style", Value::String(state.style.clone()));
    runtime.set_property(
        object.clone(),
        "currency",
        Value::String(state.currency.clone()),
    );
    runtime.set_property(
        object.clone(),
        "useGrouping",
        Value::Bool(state.use_grouping),
    );
    runtime.set_property(
        object.clone(),
        "minimumFractionDigits",
        Value::Number(state.min_fraction as f64),
    );
    runtime.set_property(
        object.clone(),
        "maximumFractionDigits",
        Value::Number(state.max_fraction as f64),
    );
    object
}

// ---------------------------------------------------------------------------
// DateTimeFormat
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DateTimeFormatState {
    locale: Locale,
    year: Option<String>,
    month: Option<String>,
    day: Option<String>,
    hour: Option<String>,
    minute: Option<String>,
    second: Option<String>,
    hour12: bool,
    date_style: Option<String>,
    time_style: Option<String>,
}

fn intl_date_time_format_constructor(
    runtime: &mut Runtime,
    _this: Value,
    args: Vec<Value>,
) -> Result<Value, JsError> {
    let locale = parse_locale(runtime, args.first());
    let options = args.get(1).cloned().unwrap_or_else(|| runtime.new_object());
    let date_style = option_string(runtime, &options, "dateStyle");
    let time_style = option_string(runtime, &options, "timeStyle");
    let state = DateTimeFormatState {
        locale,
        year: option_string(runtime, &options, "year"),
        month: option_string(runtime, &options, "month"),
        day: option_string(runtime, &options, "day"),
        hour: option_string(runtime, &options, "hour"),
        minute: option_string(runtime, &options, "minute"),
        second: option_string(runtime, &options, "second"),
        hour12: option_bool(runtime, &options, "hour12").unwrap_or(locale == Locale::EnUs),
        date_style,
        time_style,
    };
    let locale_tag = state.locale.tag().to_owned();
    let object = runtime.new_object();
    let Value::Object(id) = object else {
        return Ok(object);
    };
    let captured = state.clone();
    let format = runtime.new_native_value(
        "format",
        1,
        Box::new(move |runtime, _this, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let millis = date_arg_to_millis(runtime, &value)?;
            Ok(Value::String(format_date(&captured, millis)))
        }),
    );
    let resolved_state = state.clone();
    let resolved = runtime.new_native_value(
        "resolvedOptions",
        0,
        Box::new(move |runtime, _this, _args| {
            Ok(date_time_format_options(runtime, &resolved_state))
        }),
    );
    runtime.define_data_property(id, "format", format);
    runtime.define_data_property(id, "resolvedOptions", resolved);
    runtime.define_data_property(id, "locale", Value::String(locale_tag));
    Ok(object)
}

fn date_arg_to_millis(runtime: &mut Runtime, value: &Value) -> Result<i64, JsError> {
    match value {
        Value::Number(number) => Ok(*number as i64),
        Value::Object(_) => {
            let get_time = runtime.get_property(value, "getTime");
            if runtime.is_callable(&get_time) {
                let result = runtime.call_function(&get_time, value.clone(), Vec::new())?;
                return Ok(result.to_number() as i64);
            }
            let timestamp = runtime.get_property(value, "timestamp");
            if matches!(timestamp, Value::Number(_)) {
                return Ok(timestamp.to_number() as i64);
            }
            Ok(0)
        }
        Value::String(text) => Ok(parse_iso_date(text).unwrap_or(0)),
        _ => Ok(0),
    }
}

fn parse_iso_date(text: &str) -> Option<i64> {
    // Accept the common `YYYY-MM-DD` and `YYYY-MM-DDTHH:MM:SS` forms.
    let date = text.get(0..10)?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    let mut millis = days_from_civil(year, month as u32, day as u32) * 86_400_000;
    if text.len() >= 19 {
        let time = &text[11..19];
        let mut parts = time.split(':');
        let hour: i64 = parts.next()?.parse().ok()?;
        let minute: i64 = parts.next()?.parse().ok()?;
        let second: i64 = parts.next()?.parse().ok()?;
        millis += hour * 3_600_000 + minute * 60_000 + second * 1000;
    }
    Some(millis)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = i64::from(month) + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn format_date(state: &DateTimeFormatState, millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let in_day = millis.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = in_day / 3_600_000;
    let minute = (in_day % 3_600_000) / 60_000;
    let second = (in_day % 60_000) / 1000;

    let date_part = if let Some(style) = &state.date_style {
        format_styled_date(state.locale, style, year, month, day)
    } else if state.year.is_some() || state.month.is_some() || state.day.is_some() {
        format_components_date(state, year, month, day)
    } else {
        format_styled_date(state.locale, "short", year, month, day)
    };
    let time_part = if let Some(style) = &state.time_style {
        format_styled_time(state, style, hour, minute, second)
    } else if state.hour.is_some() || state.minute.is_some() || state.second.is_some() {
        format_components_time(state, hour, minute, second)
    } else {
        String::new()
    };
    if time_part.is_empty() {
        date_part
    } else if date_part.is_empty() {
        time_part
    } else {
        format!("{date_part}, {time_part}")
    }
}

fn format_components_date(state: &DateTimeFormatState, year: i64, month: u32, day: u32) -> String {
    let year_text = match state.year.as_deref() {
        Some("2-digit") => format!("{:02}", year.rem_euclid(100)),
        Some(_) => year.to_string(),
        None => String::new(),
    };
    let month_text = match state.month.as_deref() {
        Some("numeric") => month.to_string(),
        Some("2-digit") => format!("{month:02}"),
        Some(style) => month_name(state.locale, month, style),
        None => String::new(),
    };
    let day_text = match state.day.as_deref() {
        Some("2-digit") => format!("{day:02}"),
        Some(_) => day.to_string(),
        None => String::new(),
    };
    if year_text.is_empty() && month_text.is_empty() && day_text.is_empty() {
        return format_styled_date(state.locale, "short", year, month, day);
    }
    match state.locale {
        Locale::EnUs => {
            if matches!(state.month.as_deref(), Some("short" | "long" | "narrow")) {
                let mut out = month_text;
                if !day_text.is_empty() {
                    out.push(' ');
                    out.push_str(&day_text);
                }
                if !year_text.is_empty() {
                    out.push_str(", ");
                    out.push_str(&year_text);
                }
                out
            } else {
                let numeric: Vec<&str> = [&month_text, &day_text, &year_text]
                    .iter()
                    .filter(|part| !part.is_empty())
                    .map(|part| part.as_str())
                    .collect();
                numeric.join("/")
            }
        }
        Locale::PtBr => {
            if matches!(state.month.as_deref(), Some("short" | "long" | "narrow")) {
                let mut out = day_text;
                if !month_text.is_empty() {
                    out.push_str(" de ");
                    out.push_str(&month_text);
                }
                if !year_text.is_empty() {
                    out.push_str(" de ");
                    out.push_str(&year_text);
                }
                out
            } else {
                let numeric: Vec<&str> = [&day_text, &month_text, &year_text]
                    .iter()
                    .filter(|part| !part.is_empty())
                    .map(|part| part.as_str())
                    .collect();
                numeric.join("/")
            }
        }
    }
}

fn format_styled_date(locale: Locale, style: &str, year: i64, month: u32, day: u32) -> String {
    let short_year = format!("{:02}", year.rem_euclid(100));
    match (locale, style) {
        (Locale::EnUs, "short") => format!("{}/{}/{}", month, day, short_year),
        (Locale::EnUs, "medium") => {
            format!("{}, {}, {}", month_name(locale, month, "short"), day, year)
        }
        (Locale::EnUs, "long") | (Locale::EnUs, "full") => {
            format!("{} {}, {}", month_name(locale, month, "long"), day, year)
        }
        (Locale::PtBr, "short") => format!("{day:02}/{month:02}/{short_year}"),
        (Locale::PtBr, "medium") => {
            format!(
                "{} de {} de {}",
                day,
                month_name(locale, month, "short"),
                year
            )
        }
        (Locale::PtBr, "long") | (Locale::PtBr, "full") => {
            format!(
                "{} de {} de {}",
                day,
                month_name(locale, month, "long"),
                year
            )
        }
        _ => format!("{day:02}/{month:02}/{year}"),
    }
}

fn format_styled_time(
    state: &DateTimeFormatState,
    style: &str,
    hour: i64,
    minute: i64,
    second: i64,
) -> String {
    match (state.locale, style) {
        (Locale::EnUs, _) => format_12_hour(state, hour, minute, second),
        (Locale::PtBr, "medium") | (Locale::PtBr, "long") | (Locale::PtBr, "full") => {
            format!("{hour:02}:{minute:02}:{second:02}")
        }
        (Locale::PtBr, _) => format!("{hour:02}:{minute:02}"),
    }
}

fn format_12_hour(state: &DateTimeFormatState, hour: i64, minute: i64, second: i64) -> String {
    if !state.hour12 {
        let h = if state.second.is_some()
            || matches!(
                state.time_style.as_deref(),
                Some("medium" | "long" | "full")
            ) {
            format!("{hour:02}:{minute:02}:{second:02}")
        } else {
            format!("{hour:02}:{minute:02}")
        };
        return h;
    }
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let display_hour = match hour % 12 {
        0 => 12,
        value => value,
    };
    if state.second.is_some()
        || matches!(
            state.time_style.as_deref(),
            Some("medium" | "long" | "full")
        )
    {
        format!("{display_hour}:{minute:02}:{second:02} {suffix}")
    } else {
        format!("{display_hour}:{minute:02} {suffix}")
    }
}

fn format_components_time(
    state: &DateTimeFormatState,
    hour: i64,
    minute: i64,
    second: i64,
) -> String {
    let hour_text = match state.hour.as_deref() {
        Some("2-digit") => format!("{hour:02}"),
        Some(_) if state.hour12 => {
            let suffix = if hour < 12 { "AM" } else { "PM" };
            let display = match hour % 12 {
                0 => 12,
                value => value,
            };
            format!("{display} {suffix}")
        }
        Some(_) => hour.to_string(),
        None => String::new(),
    };
    let minute_text = match state.minute.as_deref() {
        Some("2-digit") => format!("{minute:02}"),
        Some(_) => minute.to_string(),
        None => String::new(),
    };
    let second_text = match state.second.as_deref() {
        Some("2-digit") => format!("{second:02}"),
        Some(_) => second.to_string(),
        None => String::new(),
    };
    let separator = ':';
    let mut parts = Vec::new();
    for part in [hour_text, minute_text, second_text] {
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join(&separator.to_string())
}

fn month_name(locale: Locale, month: u32, style: &str) -> String {
    const EN_LONG: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const EN_SHORT: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const PT_LONG: [&str; 12] = [
        "janeiro",
        "fevereiro",
        "março",
        "abril",
        "maio",
        "junho",
        "julho",
        "agosto",
        "setembro",
        "outubro",
        "novembro",
        "dezembro",
    ];
    const PT_SHORT: [&str; 12] = [
        "jan.", "fev.", "mar.", "abr.", "mai.", "jun.", "jul.", "ago.", "set.", "out.", "nov.",
        "dez.",
    ];
    let index = (month.saturating_sub(1) % 12) as usize;
    match (locale, style) {
        (Locale::EnUs, "short") => EN_SHORT[index].to_owned(),
        (Locale::EnUs, "narrow") => EN_SHORT[index][..1].to_owned(),
        (Locale::EnUs, _) => EN_LONG[index].to_owned(),
        (Locale::PtBr, "short") => PT_SHORT[index].to_owned(),
        (Locale::PtBr, "narrow") => PT_SHORT[index][..1].to_owned(),
        (Locale::PtBr, _) => PT_LONG[index].to_owned(),
    }
}

fn date_time_format_options(runtime: &mut Runtime, state: &DateTimeFormatState) -> Value {
    let object = runtime.new_object();
    runtime.set_property(
        object.clone(),
        "locale",
        Value::String(state.locale.tag().to_owned()),
    );
    if let Some(style) = &state.date_style {
        runtime.set_property(object.clone(), "dateStyle", Value::String(style.clone()));
    }
    if let Some(style) = &state.time_style {
        runtime.set_property(object.clone(), "timeStyle", Value::String(style.clone()));
    }
    object
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_grouped_decimals_per_locale() {
        let en = NumberFormatState {
            locale: Locale::EnUs,
            style: "decimal".to_owned(),
            currency: "USD".to_owned(),
            use_grouping: true,
            min_fraction: 0,
            max_fraction: 3,
        };
        assert_eq!(format_number(&en, 1234567.891), "1,234,567.891");
        let pt = NumberFormatState {
            locale: Locale::PtBr,
            ..en.clone()
        };
        assert_eq!(format_number(&pt, 1234567.891), "1.234.567,891");
    }

    #[test]
    fn converts_civil_dates_round_trip() {
        let days = days_from_civil(2024, 7, 2);
        assert_eq!(civil_from_days(days), (2024, 7, 2));
        assert_eq!(civil_from_days(days_from_civil(1970, 1, 1)), (1970, 1, 1));
    }
}
