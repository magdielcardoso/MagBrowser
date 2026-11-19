//! Minimal internationalized-domain and IP-literal support.
//!
//! This module implements the small part of IDNA that a browser needs on the
//! wire: RFC 3492 punycode encoding plus the `ToASCII` mapping of domain labels.
//! Everything is implemented locally so the crate stays dependency-free.
//!
//! IPv6 literals are validated and canonicalised here too, so the value stored
//! in [`crate::Url`] is the one that belongs in a `Host` header and an origin.

use crate::UrlError;

const BASE: u64 = 36;
const TMIN: u64 = 1;
const TMAX: u64 = 26;
const SKEW: u64 = 38;
const DAMP: u64 = 700;
const INITIAL_BIAS: u64 = 72;
const INITIAL_N: u64 = 0x80;

fn encode_digit(digit: u64) -> char {
    match digit {
        0..=25 => (b'a' + digit as u8) as char,
        _ => (b'0' + (digit - 26) as u8) as char,
    }
}

fn adapt(mut delta: u64, numpoints: u64, first_time: bool) -> u64 {
    delta = if first_time { delta / DAMP } else { delta / 2 };
    delta += delta / numpoints;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (BASE - TMIN + 1) * delta / (delta + SKEW)
}

/// Punycode-encodes a string into an ASCII label (RFC 3492, section 6.3).
///
/// The result is the part that follows the `xn--` prefix. It is lowercase and
/// contains only letters and digits.
pub fn punycode_encode(input: &str) -> String {
    let points: Vec<u32> = input.chars().map(u32::from).collect();
    if points.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut basic = 0u64;
    for &cp in &points {
        if u64::from(cp) < INITIAL_N {
            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
            basic += 1;
        }
    }
    let mut handled = basic;
    if basic > 0 {
        out.push('-');
    }

    let mut n = INITIAL_N;
    let mut delta = 0u64;
    let mut bias = INITIAL_BIAS;
    while handled < points.len() as u64 {
        let m = points
            .iter()
            .map(|&cp| u64::from(cp))
            .filter(|&cp| cp >= n)
            .min()
            .unwrap_or(u64::MAX);

        delta = delta.saturating_add(m.saturating_sub(n).saturating_mul(handled + 1));
        n = m;

        for &cp in &points {
            let c = u64::from(cp);
            if c < n {
                delta = delta.saturating_add(1);
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(encode_digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(encode_digit(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta = delta.saturating_add(1);
        n = n.saturating_add(1);
    }
    out
}

/// Converts a (possibly percent-encoded, possibly Unicode) host into the
/// ASCII form used for DNS lookup, headers and origins.
///
/// Each label is lowercased; non-ASCII labels are punycode-encoded and prefixed
/// with `xn--`. A single trailing dot is preserved. Invalid or over-long
/// labels are rejected.
pub fn to_ascii(host: &str) -> Result<String, UrlError> {
    if host.is_empty() {
        return Err(UrlError::MissingHost);
    }
    if host.contains(['[', ']', '@', '\\', '/', '?', '#', ' ']) {
        return Err(UrlError::InvalidHost);
    }

    let decoded = crate::percent_decode(host);
    let lower = decoded.to_lowercase();
    if lower.is_empty() {
        return Err(UrlError::MissingHost);
    }

    let parts: Vec<&str> = lower.split('.').collect();
    let mut labels = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            // A single trailing dot denotes the DNS root and is kept.
            if index + 1 == parts.len() {
                labels.push(String::new());
                continue;
            }
            return Err(UrlError::InvalidHost);
        }

        let label = if part.is_ascii() {
            if !part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(UrlError::InvalidHost);
            }
            if part.len() > 63 {
                return Err(UrlError::InvalidHost);
            }
            (*part).to_string()
        } else {
            if part.chars().any(char::is_control) {
                return Err(UrlError::InvalidHost);
            }
            let encoded = format!("xn--{}", punycode_encode(part));
            if encoded.len() > 63 {
                return Err(UrlError::InvalidHost);
            }
            encoded
        };
        labels.push(label);
    }

    if labels.is_empty() {
        return Err(UrlError::InvalidHost);
    }
    let result = labels.join(".");
    if result.len() > 253 {
        return Err(UrlError::InvalidHost);
    }
    Ok(result)
}

/// Parses and canonicalises the inside of an IPv6 literal (without brackets).
///
/// Accepts compressed (`::`) forms, embedded IPv4 addresses and an optional
/// percent-encoded zone identifier, which is passed through unchanged.
pub fn normalize_ipv6(inner: &str) -> Result<String, UrlError> {
    if inner.is_empty() {
        return Err(UrlError::InvalidHost);
    }

    let (address, suffix) = match inner.split_once('%') {
        Some((address, zone)) => (address, Some(zone)),
        None => (inner, None),
    };
    if address.is_empty() || address.contains('%') {
        return Err(UrlError::InvalidHost);
    }

    let (left, right, compressed) = match address.find("::") {
        Some(index) => {
            if address[index + 2..].contains("::") {
                return Err(UrlError::InvalidHost);
            }
            (&address[..index], &address[index + 2..], true)
        }
        None => (address, "", false),
    };

    let mut groups = parse_ipv6_groups(left)?;
    let trailing = parse_ipv6_groups(right)?;
    if compressed {
        if groups.len() + trailing.len() >= 8 {
            return Err(UrlError::InvalidHost);
        }
        groups.resize(8 - trailing.len(), 0);
        groups.extend_from_slice(&trailing);
    }
    if groups.len() != 8 {
        return Err(UrlError::InvalidHost);
    }
    let fixed: [u16; 8] = groups.try_into().map_err(|_| UrlError::InvalidHost)?;

    let mut out = if address.contains('.') {
        // Preserve a trailing embedded IPv4 address in dotted-decimal form.
        let high = fixed[6];
        let low = fixed[7];
        let ipv4 = format!("{}.{}.{}.{}", high >> 8, high & 0xff, low >> 8, low & 0xff);
        let head = format_groups_compressed(&fixed[..6]);
        if head.ends_with("::") {
            format!("{head}{ipv4}")
        } else {
            format!("{head}:{ipv4}")
        }
    } else {
        format_groups_compressed(&fixed)
    };
    if let Some(zone) = suffix {
        out.push('%');
        out.push_str(zone);
    }
    Ok(out)
}

fn parse_ipv6_groups(input: &str) -> Result<Vec<u16>, UrlError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = input.split(':').collect();
    let mut groups = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Err(UrlError::InvalidHost);
        }
        if index + 1 == parts.len() && part.contains('.') {
            // The trailing group may be a dotted IPv4 address.
            let octets: Vec<u16> = part
                .split('.')
                .map(|octet| {
                    if octet.is_empty()
                        || !octet.bytes().all(|b| b.is_ascii_digit())
                        || octet.len() > 3
                    {
                        return Err(UrlError::InvalidHost);
                    }
                    let value = octet.parse::<u16>().map_err(|_| UrlError::InvalidHost)?;
                    if value > 255 {
                        return Err(UrlError::InvalidHost);
                    }
                    Ok(value)
                })
                .collect::<Result<_, _>>()?;
            if octets.len() != 4 {
                return Err(UrlError::InvalidHost);
            }
            groups.push((octets[0] << 8) | octets[1]);
            groups.push((octets[2] << 8) | octets[3]);
        } else {
            if part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(UrlError::InvalidHost);
            }
            let value = u16::from_str_radix(part, 16).map_err(|_| UrlError::InvalidHost)?;
            groups.push(value);
        }
    }
    Ok(groups)
}

fn format_groups_compressed(groups: &[u16]) -> String {
    let mut best_start = None;
    let mut best_len = 0;
    let mut index = 0;
    while index < groups.len() {
        if groups[index] == 0 {
            let start = index;
            while index < groups.len() && groups[index] == 0 {
                index += 1;
            }
            let len = index - start;
            if len > best_len {
                best_len = len;
                best_start = Some(start);
            }
        } else {
            index += 1;
        }
    }

    if best_len < 2 {
        return groups
            .iter()
            .map(|group| format!("{group:x}"))
            .collect::<Vec<_>>()
            .join(":");
    }

    let start = best_start.unwrap_or(0);
    let mut out = groups[..start]
        .iter()
        .map(|group| format!("{group:x}"))
        .collect::<Vec<_>>()
        .join(":");
    out.push_str("::");
    out.push_str(
        &groups[start + best_len..]
            .iter()
            .map(|group| format!("{group:x}"))
            .collect::<Vec<_>>()
            .join(":"),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punycode_matches_rfc_examples() {
        assert_eq!(punycode_encode("bücher"), "bcher-kva");
        assert_eq!(punycode_encode("例え"), "r8jz45g");
        assert_eq!(punycode_encode("mañana"), "maana-pta");
        assert_eq!(punycode_encode("日本語"), "wgv71a119e");
        assert_eq!(punycode_encode("münchen"), "mnchen-3ya");
    }

    #[test]
    fn to_ascii_encodes_idn_hosts() {
        assert_eq!(to_ascii("bücher.example").unwrap(), "xn--bcher-kva.example");
        assert_eq!(to_ascii("BÜCHER.Example").unwrap(), "xn--bcher-kva.example");
        assert_eq!(
            to_ascii("b%C3%BCcher.example").unwrap(),
            "xn--bcher-kva.example"
        );
        assert_eq!(to_ascii("example.com.").unwrap(), "example.com.");
        assert_eq!(to_ascii("EXAMPLE.com").unwrap(), "example.com");
    }

    #[test]
    fn to_ascii_rejects_bad_hosts() {
        assert_eq!(to_ascii(""), Err(UrlError::MissingHost));
        assert_eq!(to_ascii("a..b"), Err(UrlError::InvalidHost));
        assert_eq!(to_ascii("bad host"), Err(UrlError::InvalidHost));
        assert_eq!(to_ascii("user@host"), Err(UrlError::InvalidHost));
        assert_eq!(to_ascii("%zz.example"), Err(UrlError::InvalidHost));
    }

    #[test]
    fn ipv6_canonicalisation() {
        let cases = [
            ("::1", "::1"),
            ("0:0:0:0:0:0:0:1", "::1"),
            (
                "2001:0db8:0000:0000:0000:ff00:0042:8329",
                "2001:db8::ff00:42:8329",
            ),
            ("fe80::1%25eth0", "fe80::1%25eth0"),
            ("::ffff:192.0.2.128", "::ffff:192.0.2.128"),
        ];
        for (input, expected) in cases {
            assert_eq!(normalize_ipv6(input).unwrap(), expected, "input: {input}");
        }
    }

    #[test]
    fn ipv6_rejects_bad_literals() {
        for input in ["", "1:2:3", "1::2::3", "gggg::1", "1:2:3:4:5:6:7:8:9"] {
            assert_eq!(
                normalize_ipv6(input),
                Err(UrlError::InvalidHost),
                "input: {input}"
            );
        }
    }
}
