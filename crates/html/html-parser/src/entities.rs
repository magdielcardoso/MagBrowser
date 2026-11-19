//! Character reference decoding for HTML text and attribute values.
//!
//! The named table covers the common HTML entities and their uppercase
//! variants. Numeric references support decimal and hexadecimal syntax,
//! Windows-1252 remapping for `0x80..=0x9F`, and replacement of invalid
//! codepoints with U+FFFD. A curated legacy set is accepted without a
//! trailing semicolon, matching the HTML parsing algorithm's behaviour for
//! the most frequently used references.

/// Named character references: `(name, replacement)`.
const NAMED: &[(&str, &str)] = &[
    ("amp", "&"),
    ("AMP", "&"),
    ("lt", "<"),
    ("LT", "<"),
    ("gt", ">"),
    ("GT", ">"),
    ("quot", "\""),
    ("QUOT", "\""),
    ("apos", "'"),
    ("nbsp", "\u{a0}"),
    ("copy", "\u{a9}"),
    ("COPY", "\u{a9}"),
    ("reg", "\u{ae}"),
    ("REG", "\u{ae}"),
    ("trade", "\u{2122}"),
    ("hellip", "\u{2026}"),
    ("mdash", "\u{2014}"),
    ("ndash", "\u{2013}"),
    ("laquo", "\u{ab}"),
    ("raquo", "\u{bb}"),
    ("lsaquo", "\u{2039}"),
    ("rsaquo", "\u{203a}"),
    ("times", "\u{d7}"),
    ("divide", "\u{f7}"),
    ("euro", "\u{20ac}"),
    ("pound", "\u{a3}"),
    ("yen", "\u{a5}"),
    ("cent", "\u{a2}"),
    ("curren", "\u{a4}"),
    ("sect", "\u{a7}"),
    ("deg", "\u{b0}"),
    ("plusmn", "\u{b1}"),
    ("sup1", "\u{b9}"),
    ("sup2", "\u{b2}"),
    ("sup3", "\u{b3}"),
    ("frac12", "\u{bd}"),
    ("frac14", "\u{bc}"),
    ("frac34", "\u{be}"),
    ("micro", "\u{b5}"),
    ("para", "\u{b6}"),
    ("middot", "\u{b7}"),
    ("bull", "\u{2022}"),
    ("dagger", "\u{2020}"),
    ("Dagger", "\u{2021}"),
    ("permil", "\u{2030}"),
    ("prime", "\u{2032}"),
    ("Prime", "\u{2033}"),
    ("lsquo", "\u{2018}"),
    ("rsquo", "\u{2019}"),
    ("ldquo", "\u{201c}"),
    ("rdquo", "\u{201d}"),
    ("sbquo", "\u{201a}"),
    ("bdquo", "\u{201e}"),
    ("oline", "\u{203e}"),
    ("frasl", "\u{2044}"),
    ("acute", "\u{b4}"),
    ("cedil", "\u{b8}"),
    ("uml", "\u{a8}"),
    ("macr", "\u{af}"),
    ("ordf", "\u{aa}"),
    ("ordm", "\u{ba}"),
    ("not", "\u{ac}"),
    ("shy", "\u{ad}"),
    ("iexcl", "\u{a1}"),
    ("iquest", "\u{bf}"),
    ("brvbar", "\u{a6}"),
    ("brokenbar", "\u{a6}"),
    ("die", "\u{a8}"),
    ("szlig", "\u{df}"),
    ("ensp", "\u{2002}"),
    ("emsp", "\u{2003}"),
    ("thinsp", "\u{2009}"),
    ("zwnj", "\u{200c}"),
    ("zwj", "\u{200d}"),
    ("lrm", "\u{200e}"),
    ("rlm", "\u{200f}"),
    ("agrave", "\u{e0}"),
    ("aacute", "\u{e1}"),
    ("acirc", "\u{e2}"),
    ("atilde", "\u{e3}"),
    ("auml", "\u{e4}"),
    ("aring", "\u{e5}"),
    ("aelig", "\u{e6}"),
    ("ccedil", "\u{e7}"),
    ("egrave", "\u{e8}"),
    ("eacute", "\u{e9}"),
    ("ecirc", "\u{ea}"),
    ("euml", "\u{eb}"),
    ("igrave", "\u{ec}"),
    ("iacute", "\u{ed}"),
    ("icirc", "\u{ee}"),
    ("iuml", "\u{ef}"),
    ("eth", "\u{f0}"),
    ("ntilde", "\u{f1}"),
    ("ograve", "\u{f2}"),
    ("oacute", "\u{f3}"),
    ("ocirc", "\u{f4}"),
    ("otilde", "\u{f5}"),
    ("ouml", "\u{f6}"),
    ("oslash", "\u{f8}"),
    ("ugrave", "\u{f9}"),
    ("uacute", "\u{fa}"),
    ("ucirc", "\u{fb}"),
    ("uuml", "\u{fc}"),
    ("yacute", "\u{fd}"),
    ("thorn", "\u{fe}"),
    ("yuml", "\u{ff}"),
    ("OElig", "\u{152}"),
    ("oelig", "\u{153}"),
    ("Scaron", "\u{160}"),
    ("scaron", "\u{161}"),
    ("Yuml", "\u{178}"),
    ("Agrave", "\u{c0}"),
    ("Aacute", "\u{c1}"),
    ("Acirc", "\u{c2}"),
    ("Atilde", "\u{c3}"),
    ("Auml", "\u{c4}"),
    ("Aring", "\u{c5}"),
    ("AElig", "\u{c6}"),
    ("Ccedil", "\u{c7}"),
    ("Egrave", "\u{c8}"),
    ("Eacute", "\u{c9}"),
    ("Ecirc", "\u{ca}"),
    ("Euml", "\u{cb}"),
    ("Igrave", "\u{cc}"),
    ("Iacute", "\u{cd}"),
    ("Icirc", "\u{ce}"),
    ("Iuml", "\u{cf}"),
    ("ETH", "\u{d0}"),
    ("Ntilde", "\u{d1}"),
    ("Ograve", "\u{d2}"),
    ("Oacute", "\u{d3}"),
    ("Ocirc", "\u{d4}"),
    ("Otilde", "\u{d5}"),
    ("Ouml", "\u{d6}"),
    ("Oslash", "\u{d8}"),
    ("Ugrave", "\u{d9}"),
    ("Uacute", "\u{da}"),
    ("Ucirc", "\u{db}"),
    ("Uuml", "\u{dc}"),
    ("Yacute", "\u{dd}"),
    ("THORN", "\u{de}"),
];

/// Names accepted without a trailing semicolon (HTML legacy references).
const LEGACY: &[&str] = &[
    "amp", "AMP", "lt", "LT", "gt", "GT", "quot", "QUOT", "nbsp", "copy", "COPY", "reg", "REG",
    "times", "divide", "plusmn", "sup1", "sup2", "sup3", "frac12", "frac14", "frac34", "deg",
    "micro", "para", "middot", "sect", "cent", "pound", "yen", "curren", "laquo", "raquo", "not",
    "shy", "macr", "acute", "cedil", "uml", "ordf", "ordm", "iexcl", "iquest", "brvbar", "szlig",
    "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil", "egrave", "eacute",
    "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde", "ograve", "oacute",
    "ocirc", "otilde", "ouml", "oslash", "ugrave", "uacute", "ucirc", "uuml", "yacute", "thorn",
    "yuml",
];

/// Windows-1252 replacements for control characters `0x80..=0x9F`.
const WINDOWS_1252: [u32; 32] = [
    0x20ac, 0x0081, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008d, 0x017d, 0x008f, 0x0090, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x009d, 0x017e, 0x0178,
];

fn lookup(name: &str) -> Option<&'static str> {
    NAMED
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

fn is_legacy(name: &str) -> bool {
    LEGACY.contains(&name)
}

/// Maximum number of numeric-reference digits that are decoded. A reference
/// with more digits is still consumed in full (so parsing makes progress), but
/// only the first `MAX_REFERENCE_DIGITS` are parsed into a code point. This
/// bounds work on pathological input such as `&#` followed by megabytes of
/// digits.
const MAX_REFERENCE_DIGITS: usize = 32;

/// Decodes the character reference starting at `chars[pos]` (which must be
/// `&`). Returns the decoded text and the position immediately after the
/// reference. Unknown references decode to the literal `&`.
pub(crate) fn consume_char_ref(chars: &[char], pos: usize) -> (String, usize) {
    if pos >= chars.len() || chars[pos] != '&' {
        return (String::new(), pos);
    }
    let len = chars.len();
    let mut i = pos + 1;
    if i >= len {
        return ("&".to_string(), pos + 1);
    }

    if chars[i] == '#' {
        i += 1;
        let hexadecimal = i < len && (chars[i] == 'x' || chars[i] == 'X');
        if hexadecimal {
            i += 1;
        }
        let is_digit = |c: char| {
            if hexadecimal {
                c.is_ascii_hexdigit()
            } else {
                c.is_ascii_digit()
            }
        };
        let digits_start = i;
        while i < len && is_digit(chars[i]) {
            i += 1;
        }
        let digits_end = i;
        if digits_end == digits_start {
            // `&#` / `&#x` without digits is emitted literally.
            let literal: String = chars[pos..i].iter().collect();
            return (literal, i);
        }
        // Leading zeros are skipped before applying the digit cap so ordinary
        // zero-padded references still decode correctly. Only a bounded number
        // of significant digits are ever parsed, which keeps pathological input
        // (e.g. megabytes of digits) linear in the already-capped input size.
        let mut value_start = digits_start;
        while value_start < digits_end && chars[value_start] == '0' {
            value_start += 1;
        }
        let value_end = (value_start + MAX_REFERENCE_DIGITS).min(digits_end);
        let digits: String = chars[value_start..value_end].iter().collect();
        let value = if digits.is_empty() {
            Ok(0)
        } else if hexadecimal {
            u32::from_str_radix(&digits, 16)
        } else {
            digits.parse::<u32>()
        };
        if i < len && chars[i] == ';' {
            i += 1;
        }
        let decoded = match value {
            Ok(0) | Err(_) => '\u{fffd}',
            Ok(v) if v > 0x10ffff => '\u{fffd}',
            Ok(v) if (0xd800..=0xdfff).contains(&v) => '\u{fffd}',
            Ok(v) if (0x80..=0x9f).contains(&v) => {
                char::from_u32(WINDOWS_1252[(v - 0x80) as usize]).unwrap_or('\u{fffd}')
            }
            Ok(v) => char::from_u32(v).unwrap_or('\u{fffd}'),
        };
        return (decoded.to_string(), i);
    }

    let name_start = i;
    while i < len && chars[i].is_ascii_alphanumeric() && i - name_start < 32 {
        i += 1;
    }
    if i == name_start {
        return ("&".to_string(), pos + 1);
    }
    let candidate: String = chars[name_start..i].iter().collect();
    let has_semicolon = i < len && chars[i] == ';';

    // Prefer the longest matching name. A full-length match may consume the
    // semicolon; a shorter match is only accepted for legacy names.
    for length in (1..=candidate.len()).rev() {
        let name = &candidate[..length];
        // Names are ASCII, so byte indices and char indices agree here.
        if let Some(value) = lookup(name) {
            if length == candidate.len() {
                if has_semicolon {
                    return (value.to_string(), i + 1);
                }
                if is_legacy(name) {
                    return (value.to_string(), name_start + length);
                }
            } else if is_legacy(name) {
                return (value.to_string(), name_start + length);
            }
        }
    }

    ("&".to_string(), pos + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(input: &str) -> String {
        let chars: Vec<char> = input.chars().collect();
        let mut out = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '&' {
                let (text, next) = consume_char_ref(&chars, i);
                out.push_str(&text);
                i = next;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn named_and_numeric_references() {
        assert_eq!(decode("a &amp; b"), "a & b");
        assert_eq!(decode("&#65;&#x42;&#X43;"), "ABC");
        assert_eq!(
            decode("&copy; &hellip; &mdash;"),
            "\u{a9} \u{2026} \u{2014}"
        );
    }

    #[test]
    fn legacy_without_semicolon_and_unknown() {
        assert_eq!(decode("&amp x"), "& x");
        assert_eq!(decode("&notit;"), "\u{ac}it;");
        assert_eq!(decode("&doesnotexist;"), "&doesnotexist;");
        assert_eq!(decode("&"), "&");
    }

    #[test]
    fn invalid_codepoints_and_windows_1252() {
        assert_eq!(decode("&#0;"), "\u{fffd}");
        assert_eq!(decode("&#x110000;"), "\u{fffd}");
        assert_eq!(decode("&#x80;"), "\u{20ac}");
        assert_eq!(decode("&#xD800;"), "\u{fffd}");
        assert_eq!(decode("&#;"), "&#;");
        assert_eq!(decode("&#x;"), "&#x;");
    }
}
