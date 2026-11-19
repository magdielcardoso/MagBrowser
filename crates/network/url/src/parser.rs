//! URL parser and relative-reference resolution.

use crate::{
    Url, UrlError, default_port, encode_query, idna, normalize_percent_encoding, percent_encode,
};

/// Parses an absolute URL.
pub fn parse(input: &str) -> Result<Url, UrlError> {
    let cleaned = clean(input);
    let (scheme, rest) = match split_scheme(&cleaned) {
        Some(parts) => parts,
        None if cleaned
            .find(':')
            .is_some_and(|i| !cleaned[..i].contains(['/', '?', '#'])) =>
        {
            return Err(UrlError::InvalidScheme);
        }
        None => return Err(UrlError::MissingScheme),
    };
    let special = default_port(&scheme).is_some() || scheme == "file";
    parse_absolute(&scheme, rest, special)
}

/// Parses `input` relative to `base`.
pub fn parse_with_base(input: &str, base: &Url) -> Result<Url, UrlError> {
    let cleaned = clean(input);

    if let Some((scheme, rest)) = split_scheme(&cleaned) {
        let special = default_port(&scheme).is_some() || scheme == "file";
        return parse_absolute(&scheme, rest, special);
    }

    // Scheme-relative reference (`//host/path`): inherits the base scheme.
    // For special schemes a backslash is a slash, so `\\host\path` counts too.
    if base.is_special() {
        let bytes = cleaned.as_bytes();
        if bytes.len() >= 2 && matches!(bytes[0], b'/' | b'\\') && matches!(bytes[1], b'/' | b'\\')
        {
            return parse_absolute(base.scheme(), &cleaned[2..], true);
        }
    }
    if let Some(rest) = cleaned.strip_prefix("//") {
        return parse_absolute(base.scheme(), rest, base.is_special());
    }

    let (before_fragment, fragment) = match cleaned.find('#') {
        Some(i) => (&cleaned[..i], Some(cleaned[i + 1..].to_string())),
        None => (cleaned.as_str(), None),
    };

    // An empty (or fragment-only) reference keeps the base path and query
    // but drops an existing fragment unless a new one is supplied.
    if before_fragment.is_empty() {
        let mut url = base.clone();
        url.set_fragment(fragment.map(|fragment| encode_fragment(&fragment)));
        return Ok(url);
    }

    let (path_part, query) = split_query(before_fragment);
    let mut url = base.clone();

    if !path_part.is_empty() {
        let absolute =
            path_part.starts_with('/') || (base.is_special() && path_part.starts_with('\\'));
        let mut segments: Vec<String> = if absolute {
            Vec::new()
        } else {
            let base_segments = base.path_segments();
            if base_segments.is_empty() {
                Vec::new()
            } else {
                base_segments[..base_segments.len() - 1].to_vec()
            }
        };
        segments.extend(split_path(path_part, base.is_special()));
        url.set_path(normalize_segments(&segments));
    }
    // A query-only reference (`?x=1`) leaves the whole base path untouched.

    // Any reference with a path replaces the base query and fragment; only a
    // fragment-only reference keeps them (handled above).
    url.set_query(query.map(encode_query_component));
    url.set_fragment(fragment.map(|fragment| encode_fragment(&fragment)));
    Ok(url)
}

fn clean(input: &str) -> String {
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect()
}

fn split_scheme(input: &str) -> Option<(String, &str)> {
    let idx = input.find(':')?;
    let scheme = &input[..idx];
    if scheme.is_empty() || !scheme.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    if !scheme
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        // A colon that appears inside a path is not a scheme separator.
        return None;
    }
    Some((scheme.to_ascii_lowercase(), &input[idx + 1..]))
}

fn split_query(input: &str) -> (&str, Option<&str>) {
    match input.find('?') {
        Some(i) => (&input[..i], Some(&input[i + 1..])),
        None => (input, None),
    }
}

/// Percent-encodes a component and normalises any escapes that were already
/// present (uppercase hex, decode unreserved bytes, fix stray `%`).
fn encode_component(input: &str, keep: impl Fn(u8) -> bool) -> String {
    normalize_percent_encoding(&percent_encode(input, keep))
}

fn encode_query_component(input: &str) -> String {
    normalize_percent_encoding(&encode_query(input))
}

fn encode_fragment(input: &str) -> String {
    normalize_percent_encoding(&encode_query(input))
}

fn parse_absolute(scheme: &str, rest: &str, special: bool) -> Result<Url, UrlError> {
    // Special schemes treat backslashes as path separators.
    let normalised;
    let rest = if special && rest.contains('\\') {
        normalised = rest.replace('\\', "/");
        normalised.as_str()
    } else {
        rest
    };

    let (without_fragment, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };

    let mut url = Url::empty(scheme, special);
    if let Some(fragment) = fragment {
        url.set_fragment(Some(encode_fragment(fragment)));
    }

    let mut after_authority = without_fragment;
    let mut authority: Option<&str> = None;

    if special {
        // WHATWG skips any number of leading slashes for special schemes, so
        // `http:example.com`, `http:/example.com` and `http://example.com`
        // all parse the same way. `file:` keeps its empty-authority form.
        let stripped = after_authority
            .strip_prefix("//")
            .unwrap_or(after_authority);
        let stripped = if scheme == "file" {
            stripped
        } else {
            stripped.trim_start_matches('/')
        };
        let end = stripped.find(['/', '?']).unwrap_or(stripped.len());
        authority = Some(&stripped[..end]);
        after_authority = &stripped[end..];
    } else if let Some(stripped) = after_authority.strip_prefix("//") {
        let end = stripped.find(['/', '?']).unwrap_or(stripped.len());
        authority = Some(&stripped[..end]);
        after_authority = &stripped[end..];
    }

    if let Some(auth) = authority {
        if auth.is_empty() && scheme == "file" {
            url.set_host(None);
        } else {
            parse_authority(auth, &mut url, scheme)?;
        }
    } else if special && scheme != "file" {
        return Err(UrlError::MissingHost);
    }

    let (path_part, query) = split_query(after_authority);
    if let Some(query) = query {
        url.set_query(Some(encode_query_component(query)));
    }

    let segments = normalize_segments(&split_path(path_part, special));
    url.set_path(segments);

    if url.host().is_none() && special && scheme != "file" {
        return Err(UrlError::MissingHost);
    }
    Ok(url)
}

/// Splits a path into encoded segments, remembering a trailing slash as a
/// final empty segment so `a/` and `..` resolve correctly against directories.
fn split_path(path_part: &str, special: bool) -> Vec<String> {
    let normalised;
    let path_part = if special && path_part.contains('\\') {
        normalised = path_part.replace('\\', "/");
        normalised.as_str()
    } else {
        path_part
    };

    let mut raw: Vec<&str> = path_part.split('/').collect();
    if raw.first().is_some_and(|segment| segment.is_empty()) {
        raw.remove(0);
    }
    // A reference ending in `.` or `..` resolves to a directory, so it
    // gains a trailing slash just like an explicit `./` or `../`.
    let trailing = raw
        .iter()
        .rev()
        .find(|segment| !segment.is_empty())
        .is_some_and(|segment| matches!(*segment, "." | ".."))
        || raw.last().is_some_and(|segment| segment.is_empty());
    let mut segments: Vec<String> = raw
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .map(|segment| encode_component(segment, keep_path_byte))
        .collect();
    if trailing {
        segments.push(String::new());
    }
    segments
}

fn parse_authority(auth: &str, url: &mut Url, scheme: &str) -> Result<(), UrlError> {
    let mut rest = auth;
    if let Some(at) = auth.rfind('@') {
        let userinfo = &auth[..at];
        rest = &auth[at + 1..];
        let (user, pass) = match userinfo.find(':') {
            Some(i) => (&userinfo[..i], Some(&userinfo[i + 1..])),
            None => (userinfo, None),
        };
        url.set_username(&encode_component(user, keep_userinfo_byte));
        url.set_password(pass.map(|p| encode_component(p, keep_userinfo_byte)));
    }

    // Anything left after the final `@` must be a host (and optional port);
    // an embedded `@` cannot belong to a host.
    if rest.contains('@') {
        return Err(UrlError::InvalidHost);
    }

    let port = if let Some(stripped) = rest.strip_prefix('[') {
        let end = stripped.find(']').ok_or(UrlError::InvalidHost)?;
        let canonical = idna::normalize_ipv6(&stripped[..end])?;
        url.set_host(Some(&format!("[{canonical}]")));
        let tail = &stripped[end + 1..];
        match tail.strip_prefix(':') {
            Some(port) => Some(port),
            None if tail.is_empty() => None,
            None => return Err(UrlError::InvalidHost),
        }
    } else {
        let (host, port) = match rest.rfind(':') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        url.set_host(Some(&idna::to_ascii(host)?));
        port
    };

    if let Some(port) = port
        && !port.is_empty()
    {
        let parsed = parse_port(port)?;
        // The default port is not part of a URL's serialisation or origin.
        if default_port(scheme) != Some(parsed) {
            url.set_port(Some(parsed));
        }
    }
    Ok(())
}

fn parse_port(input: &str) -> Result<u16, UrlError> {
    input.parse::<u16>().map_err(|_| UrlError::InvalidPort)
}

fn keep_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'%'
        )
}

fn keep_userinfo_byte(b: u8) -> bool {
    // `@` is legal inside userinfo; only the *last* one separates it from the
    // host (see `parse_authority`), so it needs no escaping here.
    keep_path_byte(b) && b != b'/' && b != b'?'
}

/// Resolves `.` and `..` segments, preserving a single trailing empty segment.
fn normalize_segments(segments: &[String]) -> Vec<String> {
    let trailing = segments.last().is_some_and(String::is_empty);
    let mut out: Vec<String> = Vec::with_capacity(segments.len());
    for segment in segments {
        match segment.as_str() {
            "." | "" => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_string()),
        }
    }
    if trailing && !out.last().is_some_and(String::is_empty) {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_base_reference_keeps_path() {
        let base = parse("https://a.test/b?x#y").unwrap();
        let url = parse_with_base("", &base).unwrap();
        assert_eq!(url.as_str(), "https://a.test/b?x");
    }

    #[test]
    fn relative_path_after_trailing_slash() {
        let base = parse("http://a.test/dir/").unwrap();
        assert_eq!(base.join("f").unwrap().as_str(), "http://a.test/dir/f");
    }

    #[test]
    fn ipv6_host_and_port() {
        let url = parse("http://[::1]:8080/x").unwrap();
        assert_eq!(url.host(), Some("[::1]"));
        assert_eq!(url.port(), Some(8080));
        assert_eq!(url.as_str(), "http://[::1]:8080/x");
    }

    #[test]
    fn query_is_encoded() {
        let url = parse("https://a.test/p?a=hello world").unwrap();
        assert_eq!(url.query(), Some("a=hello%20world"));
    }
}
