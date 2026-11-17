//! URL parser and relative-reference resolution.

use crate::{Url, UrlError, default_port, encode_query, percent_encode};

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

    if let Some(rest) = cleaned.strip_prefix("//") {
        return parse_absolute(base.scheme(), rest, base.is_special());
    }

    let (before_fragment, fragment) = match cleaned.find('#') {
        Some(i) => (&cleaned[..i], Some(cleaned[i + 1..].to_string())),
        None => (cleaned.as_str(), None),
    };

    if before_fragment.is_empty() {
        let mut url = base.clone();
        url.set_fragment(fragment.map(|f| encode_query(&f)));
        return Ok(url);
    }

    // Path (and possibly query) relative reference.
    let (path_part, query) = split_query(before_fragment);
    let mut segments: Vec<String> = Vec::new();
    let absolute_path = path_part.starts_with('/');

    if !absolute_path {
        segments.extend(
            base.path_segments()
                .iter()
                .take(if base.path_segments().is_empty() {
                    0
                } else {
                    base.path_segments().len() - 1
                })
                .cloned(),
        );
    }

    let mut raw: Vec<String> = path_part
        .split('/')
        .map(|s| percent_encode(s, keep_path_byte))
        .collect();
    if raw.first().is_some_and(String::is_empty) {
        raw.remove(0);
    }
    segments.extend(raw);
    let path = normalise_segments(&segments);

    let mut url = base.clone();
    url.set_path(path);
    url.set_query(query.map(encode_query));
    url.set_fragment(fragment.map(|f| encode_query(&f)));
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

fn parse_absolute(scheme: &str, rest: &str, special: bool) -> Result<Url, UrlError> {
    let (without_fragment, fragment) = match rest.find('#') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };

    let mut url = Url::empty(scheme, special);
    url.set_fragment(fragment.map(encode_query));

    let mut after_authority = without_fragment;
    let mut authority: Option<&str> = None;

    if let Some(stripped) = after_authority.strip_prefix("//") {
        let end = stripped.find(['/', '?']).unwrap_or(stripped.len());
        authority = Some(&stripped[..end]);
        after_authority = &stripped[end..];
    } else if special {
        // WHATWG treats `http:example.com` as `http://example.com`.
        let end = after_authority
            .find(['/', '?'])
            .unwrap_or(after_authority.len());
        authority = Some(&after_authority[..end]);
        after_authority = &after_authority[end..];
    }

    if let Some(auth) = authority {
        if auth.is_empty() && scheme == "file" {
            url.set_host(None);
        } else {
            parse_authority(auth, &mut url)?;
        }
    } else if special && scheme != "file" {
        return Err(UrlError::MissingHost);
    }

    let (path_part, query) = split_query(after_authority);
    url.set_query(query.map(encode_query));

    let mut segments: Vec<String> = path_part
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| percent_encode(s, keep_path_byte))
        .collect();
    segments = normalise_segments(&segments);
    if !path_part.is_empty()
        && path_part.ends_with('/')
        && !segments.last().is_some_and(String::is_empty)
    {
        segments.push(String::new());
    }
    if segments.is_empty() && special {
        segments.push(String::new());
    }
    url.set_path(segments);

    if url.host().is_none() && special && scheme != "file" {
        return Err(UrlError::MissingHost);
    }
    Ok(url)
}

fn parse_authority(auth: &str, url: &mut Url) -> Result<(), UrlError> {
    let mut rest = auth;
    if let Some(at) = auth.rfind('@') {
        let userinfo = &auth[..at];
        rest = &auth[at + 1..];
        let (user, pass) = match userinfo.find(':') {
            Some(i) => (&userinfo[..i], Some(&userinfo[i + 1..])),
            None => (userinfo, None),
        };
        url.set_username(&percent_encode(user, keep_userinfo_byte));
        url.set_password(pass.map(|p| percent_encode(p, keep_userinfo_byte)));
    }

    if let Some(stripped) = rest.strip_prefix('[') {
        // IPv6 literal.
        let end = stripped.find(']').ok_or(UrlError::InvalidHost)?;
        let host = &stripped[..end];
        url.set_host(Some(&format!("[{host}]")));
        let tail = &stripped[end + 1..];
        if let Some(port) = tail.strip_prefix(':') {
            url.set_port(Some(parse_port(port)?));
        } else if !tail.is_empty() {
            return Err(UrlError::InvalidHost);
        }
        return Ok(());
    }

    let (host, port) = match rest.rfind(':') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if host.is_empty() {
        return Err(UrlError::MissingHost);
    }
    if !host.chars().all(keep_host_char) {
        return Err(UrlError::InvalidHost);
    }
    url.set_host(Some(&host.to_ascii_lowercase()));
    if let Some(p) = port {
        if !p.is_empty() {
            url.set_port(Some(parse_port(p)?));
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
    keep_path_byte(b) && b != b'@' && b != b'/' && b != b'?'
}

fn keep_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_') || !c.is_ascii()
}

/// Resolves `.` and `..` segments and drops empty segments.
fn normalise_segments(segments: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(segments.len());
    for seg in segments {
        match seg.as_str() {
            "." | "" => {}
            ".." => {
                out.pop();
            }
            other => out.push(other.to_string()),
        }
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
