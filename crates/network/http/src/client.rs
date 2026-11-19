//! HTTP/1.1 client: connection management, TLS, body framing and redirects.

use std::collections::HashSet;
use std::io::{self, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use browser_url::Url;

use crate::cache::HttpCache;
use crate::cookies::CookieJar;
use crate::headers::Headers;
use crate::redirect::{RedirectOutcome, RedirectPolicy};
use crate::request::{Method, Request};
use crate::response::Response;

/// Upper bound on how long a single socket read or write may block before the
/// surrounding deadline loop regains control and re-checks the overall request
/// deadline. Without this, one `read` could stall for the whole timeout and
/// push a request well past its deadline.
const IO_SLICE: Duration = Duration::from_secs(2);

/// Errors raised while performing a request.
#[derive(Debug)]
pub enum HttpError {
    /// The URL could not be parsed or resolved.
    Url(browser_url::UrlError),
    /// The connection failed.
    Io(io::Error),
    /// TLS negotiation failed.
    Tls(String),
    /// The peer sent a malformed response.
    Protocol(String),
    /// Too many redirects were followed.
    TooManyRedirects(usize),
    /// A redirect pointed back at a URL already visited in this chain.
    RedirectLoop(String),
    /// An operation (DNS, connect or the whole request) exceeded its deadline.
    Timeout(String),
    /// The response body exceeded the configured limit.
    PayloadTooLarge,
    /// The `Content-Encoding` cannot be decoded by this build (for example br).
    UnsupportedContentEncoding(String),
    /// A compressed body was present but malformed or truncated.
    Decompression(String),
    /// The URL cannot be fetched over the network.
    UnsupportedScheme(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Url(e) => write!(f, "invalid URL: {e}"),
            Self::Io(e) => write!(f, "connection error: {e}"),
            Self::Tls(e) => write!(f, "TLS error: {e}"),
            Self::Protocol(e) => write!(f, "protocol error: {e}"),
            Self::TooManyRedirects(n) => write!(f, "too many redirects ({n})"),
            Self::RedirectLoop(url) => write!(f, "redirect loop detected at {url}"),
            Self::Timeout(e) => write!(f, "request timed out: {e}"),
            Self::PayloadTooLarge => f.write_str("response body too large"),
            Self::UnsupportedContentEncoding(name) => {
                write!(f, "unsupported content encoding: {name}")
            }
            Self::Decompression(e) => write!(f, "failed to decompress response body: {e}"),
            Self::UnsupportedScheme(s) => write!(f, "unsupported scheme: {s}"),
        }
    }
}

impl std::error::Error for HttpError {}

impl From<io::Error> for HttpError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<browser_url::UrlError> for HttpError {
    fn from(value: browser_url::UrlError) -> Self {
        Self::Url(value)
    }
}

/// Client configuration.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `User-Agent` header value.
    pub user_agent: String,
    /// Deadline for a single request (DNS + connect + headers + body).
    pub timeout: Duration,
    /// Overall deadline for an entire redirect chain.
    pub redirect_chain_timeout: Duration,
    /// Maximum number of redirects.
    pub max_redirects: usize,
    /// Maximum accepted body size in bytes.
    pub max_body_bytes: usize,
    /// Whether redirects are followed automatically.
    pub follow_redirects: bool,
    /// Send HTTP cookies from the jar.
    pub send_cookies: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            user_agent: format!("Mag/{} (+engine)", env!("CARGO_PKG_VERSION")),
            timeout: Duration::from_secs(8),
            redirect_chain_timeout: Duration::from_secs(24),
            max_redirects: 10,
            max_body_bytes: 32 * 1024 * 1024,
            follow_redirects: true,
            send_cookies: true,
        }
    }
}

/// An HTTP client with cookie and cache support.
pub struct Client {
    config: ClientConfig,
    cookies: CookieJar,
    cache: HttpCache,
    tls: Option<std::sync::Arc<rustls::ClientConfig>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("config", &self.config)
            .field("cookies", &self.cookies.len())
            .finish()
    }
}

impl Client {
    /// Creates a client with the given configuration.
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            cookies: CookieJar::new(),
            cache: HttpCache::default(),
            tls: None,
        }
    }

    /// The active configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Mutable access to the cookie jar.
    pub fn cookies_mut(&mut self) -> &mut CookieJar {
        &mut self.cookies
    }

    /// Access to the response cache.
    pub fn cache(&self) -> &HttpCache {
        &self.cache
    }

    /// Performs `GET url`.
    pub fn get(&mut self, url: Url) -> Result<Response, HttpError> {
        self.fetch(&Request::get(url))
    }

    /// Performs `GET` for a URL string.
    pub fn get_str(&mut self, url: &str) -> Result<Response, HttpError> {
        let url = Url::parse(url)?;
        self.get(url)
    }

    /// Fetches a request, following redirects when configured.
    pub fn fetch(&mut self, request: &Request) -> Result<Response, HttpError> {
        let mut url = request.url.clone();
        if !url.is_network() {
            return Err(HttpError::UnsupportedScheme(url.scheme().to_string()));
        }
        let mut method = request.method;

        let mut conditional_etag = None;
        if method == Method::Get {
            if let Some(cached) = self.cache.get(&url.as_str()) {
                return Ok(cached);
            }
            // A stale-but-validated entry can be revalidated instead of
            // downloaded again. The validator belongs to the original URL only.
            conditional_etag = self.cache.revalidation_etag(&url.as_str());
        }

        let mut chain = RedirectChain::new(
            &url,
            self.config.max_redirects,
            self.config.redirect_chain_timeout,
        );
        let mut hops = 0_usize;
        loop {
            // Every hop gets its own deadline covering DNS, connect, headers
            // and body. The redirect chain as a whole has a separate cap.
            let request_deadline = Instant::now() + self.config.timeout;

            let mut req = Request {
                method,
                url: url.clone(),
                headers: request.headers.clone(),
                body: if method == request.method {
                    request.body.clone()
                } else {
                    Vec::new()
                },
            };
            self.prepare(&mut req);
            if hops == 0
                && let Some(tag) = &conditional_etag
                && !req.headers.contains("If-None-Match")
            {
                req.headers.insert("If-None-Match", tag);
            }
            let response = self.send(&req, request_deadline)?;

            self.cookies.store_response(&url, &response);

            // A 304 has no body: replay the cached entity rather than returning
            // an empty response to the caller.
            if hops == 0
                && response.status == 304
                && conditional_etag.is_some()
                && let Some(cached) = self.cache.get_stored(&url.as_str())
            {
                return Ok(cached);
            }

            if self.config.follow_redirects && response.is_redirect() {
                let policy = RedirectPolicy {
                    max_redirects: self.config.max_redirects,
                    allow_cross_scheme: true,
                };
                match policy.resolve(&url, &response, method, hops)? {
                    RedirectOutcome::Follow(next, next_method) => {
                        chain.check(&next, hops)?;
                        chain.record(&next);
                        hops += 1;
                        url = next;
                        method = next_method;
                        continue;
                    }
                    RedirectOutcome::Reject(reason) => {
                        return Err(HttpError::Protocol(format!("redirect rejected: {reason}")));
                    }
                }
            }

            if method == Method::Get {
                self.cache.store(&url.as_str(), &response);
            }
            return Ok(response);
        }
    }

    fn prepare(&self, req: &mut Request) {
        if let Some(host) = req.url.host_header() {
            req.headers.insert("Host", &host);
        }
        req.headers.insert("User-Agent", &self.config.user_agent);
        if !req.headers.contains("Accept") {
            req.headers
                .insert("Accept", "text/html,application/xhtml+xml,*/*;q=0.8");
        }
        // Only codings we can actually decode in-process (no brotli crate).
        req.headers.insert("Accept-Encoding", "gzip, deflate");
        req.headers.insert("Connection", "close");
        if !req.body.is_empty() {
            req.headers
                .insert("Content-Length", &req.body.len().to_string());
        }
        if self.config.send_cookies {
            req.headers.remove("Cookie");
            if let Some(cookie) = self.cookies.cookie_header(&req.url) {
                req.headers.insert("Cookie", &cookie);
            }
        }
    }

    fn send(&mut self, req: &Request, deadline: Instant) -> Result<Response, HttpError> {
        check_deadline(deadline)?;
        let url = &req.url;
        let host = url
            .host()
            .ok_or(HttpError::UnsupportedScheme("no host".into()))?;
        let Some(port) = url.port_or_default() else {
            return Err(HttpError::UnsupportedScheme(url.scheme().to_string()));
        };
        let stream = connect(host, port, self.config.timeout, deadline)?;

        let head = req.to_wire().into_bytes();
        let mut body = Vec::with_capacity(head.len() + req.body.len());
        body.extend_from_slice(&head);
        body.extend_from_slice(&req.body);

        let response = if url.scheme() == "https" {
            let tls = self.tls_config()?;
            let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
                .map_err(|e| HttpError::Tls(e.to_string()))?;
            let conn = rustls::ClientConnection::new(tls, server_name)
                .map_err(|e| HttpError::Tls(e.to_string()))?;
            let mut stream = rustls::StreamOwned::new(conn, stream);
            write_all_deadline(&mut stream, &body, deadline)?;
            flush_deadline(&mut stream, deadline)?;
            read_response(
                BufReader::new(stream),
                url,
                req.method,
                self.config.max_body_bytes,
                deadline,
            )?
        } else {
            let mut stream = stream;
            write_all_deadline(&mut stream, &body, deadline)?;
            flush_deadline(&mut stream, deadline)?;
            read_response(
                BufReader::new(stream),
                url,
                req.method,
                self.config.max_body_bytes,
                deadline,
            )?
        };
        Ok(response)
    }

    fn tls_config(&mut self) -> Result<std::sync::Arc<rustls::ClientConfig>, HttpError> {
        if let Some(cfg) = &self.tls {
            return Ok(cfg.clone());
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let cfg = std::sync::Arc::new(cfg);
        self.tls = Some(cfg.clone());
        Ok(cfg)
    }
}

/// Tracks the URLs already seen during one redirect chain and bounds the
/// chain's total time. Kept separate from `RedirectPolicy` so the decision
/// logic can be unit tested without any network access.
struct RedirectChain {
    visited: HashSet<String>,
    max_redirects: usize,
    deadline: Instant,
}

impl RedirectChain {
    fn new(start: &Url, max_redirects: usize, total_time: Duration) -> Self {
        let mut visited = HashSet::new();
        visited.insert(Self::key(start));
        Self {
            visited,
            max_redirects,
            deadline: Instant::now() + total_time,
        }
    }

    /// Fragment-insensitive key: a fragment is never sent to a server, so two
    /// URLs differing only by fragment are the same hop for loop detection.
    fn key(url: &Url) -> String {
        let serialised = url.as_str();
        match serialised.find('#') {
            Some(index) => serialised[..index].to_string(),
            None => serialised,
        }
    }

    /// Validates a candidate next hop before it is followed.
    fn check(&self, next: &Url, hops: usize) -> Result<(), HttpError> {
        check_deadline(self.deadline).map_err(|_| {
            HttpError::Timeout("redirect chain exceeded its total time budget".into())
        })?;
        if hops >= self.max_redirects {
            return Err(HttpError::TooManyRedirects(hops));
        }
        if self.visited.contains(&Self::key(next)) {
            return Err(HttpError::RedirectLoop(next.as_str()));
        }
        Ok(())
    }

    fn record(&mut self, next: &Url) {
        self.visited.insert(Self::key(next));
    }
}

/// Fails once `deadline` has passed.
fn check_deadline(deadline: Instant) -> Result<(), HttpError> {
    if Instant::now() >= deadline {
        Err(HttpError::Timeout("request deadline exceeded".into()))
    } else {
        Ok(())
    }
}

/// Time left until `deadline`, or a timeout error if it has already passed.
fn remaining(deadline: Instant) -> Result<Duration, HttpError> {
    deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| HttpError::Timeout("request deadline exceeded".into()))
}

/// Reads once, retrying on interruption and socket timeouts so the surrounding
/// deadline loop is the single authority on how long a request may take.
fn read_some_deadline<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<usize, HttpError> {
    loop {
        check_deadline(deadline)?;
        match reader.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(HttpError::Io(e)),
        }
    }
}

/// Fills `buf` completely, checking the deadline between reads.
fn read_exact_deadline<R: Read>(
    reader: &mut R,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), HttpError> {
    let mut filled = 0;
    while filled < buf.len() {
        match read_some_deadline(reader, &mut buf[filled..], deadline)? {
            0 => return Err(HttpError::Protocol("unexpected EOF".into())),
            n => filled += n,
        }
    }
    Ok(())
}

/// Reads until EOF or `max` bytes, checking the deadline between reads.
fn read_to_end_deadline<R: Read>(
    reader: &mut R,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    let mut chunk = [0_u8; 8192];
    while out.len() < max {
        let want = (max - out.len()).min(chunk.len());
        match read_some_deadline(reader, &mut chunk[..want], deadline)? {
            0 => break,
            n => out.extend_from_slice(&chunk[..n]),
        }
    }
    Ok(out)
}

/// Writes all of `buf`, checking the deadline between writes.
fn write_all_deadline<W: Write>(
    writer: &mut W,
    mut buf: &[u8],
    deadline: Instant,
) -> Result<(), HttpError> {
    while !buf.is_empty() {
        check_deadline(deadline)?;
        match writer.write(buf) {
            Ok(0) => {
                return Err(HttpError::Io(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole request",
                )));
            }
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(HttpError::Io(e)),
        }
    }
    Ok(())
}

/// Flushes pending output, checking the deadline between attempts.
fn flush_deadline<W: Write>(writer: &mut W, deadline: Instant) -> Result<(), HttpError> {
    loop {
        check_deadline(deadline)?;
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(HttpError::Io(e)),
        }
    }
}

/// Resolves `host` on a worker thread so a slow DNS server cannot block the
/// caller. The worker is detached: if it outlives the timeout it simply drops
/// its result, but the requesting thread is freed immediately.
fn resolve_addrs(host: &str, port: u16, timeout: Duration) -> Result<Vec<SocketAddr>, HttpError> {
    let (tx, rx) = mpsc::channel();
    let resolve_host = host.to_string();
    let worker_host = resolve_host.clone();
    std::thread::Builder::new()
        .name("mag-dns-resolve".into())
        .spawn(move || {
            let resolved = (worker_host.as_str(), port)
                .to_socket_addrs()
                .map(|addrs| addrs.collect::<Vec<_>>());
            let _ = tx.send(resolved);
        })
        .map_err(HttpError::Io)?;

    match rx.recv_timeout(timeout) {
        Ok(Ok(addrs)) => Ok(addrs),
        Ok(Err(err)) => Err(HttpError::Io(err)),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(HttpError::Timeout(format!(
            "DNS resolution for '{resolve_host}' exceeded {timeout:?}"
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(HttpError::Protocol(
            "DNS resolver worker stopped unexpectedly".into(),
        )),
    }
}

fn connect(
    host: &str,
    port: u16,
    timeout: Duration,
    deadline: Instant,
) -> Result<TcpStream, HttpError> {
    let dns_budget = remaining(deadline)?;
    let addrs = resolve_addrs(host, port, timeout.min(dns_budget))?;
    if addrs.is_empty() {
        return Err(HttpError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "no addresses resolved",
        )));
    }

    let mut last_err = None;
    for addr in addrs {
        let budget = remaining(deadline)?;
        match TcpStream::connect_timeout(&addr, timeout.min(budget)) {
            Ok(stream) => {
                // Short socket timeouts make the deadline loops responsive;
                // the deadline itself decides when a request finally fails.
                let io_slice = timeout.min(IO_SLICE);
                let _ = stream.set_read_timeout(Some(io_slice));
                let _ = stream.set_write_timeout(Some(io_slice));
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(err) => last_err = Some(err),
        }
    }
    Err(HttpError::Io(last_err.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no addresses resolved")
    })))
}

/// Reads and parses one HTTP/1.x response from `reader`.
fn read_response<R: Read>(
    mut reader: BufReader<R>,
    url: &Url,
    method: Method,
    max_body: usize,
    deadline: Instant,
) -> Result<Response, HttpError> {
    let (status, reason, headers) = read_head(&mut reader, deadline)?;
    let mut response = Response::new(url.clone(), status, reason, headers, Vec::new());

    if method == Method::Head || matches!(status, 204 | 304) {
        return Ok(response);
    }

    let chunked = response
        .headers
        .get("Transfer-Encoding")
        .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"));

    if chunked {
        response.body = read_chunked(&mut reader, max_body, deadline)?;
    } else if let Some(len) = response
        .headers
        .get("Content-Length")
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if len > max_body {
            return Err(HttpError::PayloadTooLarge);
        }
        let mut buf = vec![0_u8; len];
        read_exact_deadline(&mut reader, &mut buf, deadline)?;
        response.body = buf;
    } else {
        response.body = read_to_end_deadline(&mut reader, max_body, deadline)?;
    }
    response.body = crate::encoding::decode_body(
        response.headers.get("Content-Encoding"),
        &response.body,
        max_body,
    )?;
    Ok(response)
}

fn read_head<R: Read>(
    reader: &mut BufReader<R>,
    deadline: Instant,
) -> Result<(u16, String, Headers), HttpError> {
    let mut raw = Vec::with_capacity(1024);
    let mut byte = [0_u8; 1];
    loop {
        if read_some_deadline(reader, &mut byte, deadline)? == 0 {
            return Err(HttpError::Protocol(
                "connection closed before headers".into(),
            ));
        }
        raw.push(byte[0]);
        if raw.ends_with(b"\r\n\r\n") {
            break;
        }
        if raw.len() > 64 * 1024 {
            return Err(HttpError::Protocol("response headers too large".into()));
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| HttpError::Protocol("empty response".into()))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(HttpError::Protocol(format!(
            "bad status line: {status_line}"
        )));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| HttpError::Protocol(format!("bad status code: {status_line}")))?;
    let reason = parts.next().unwrap_or("").trim().to_string();

    let mut headers = Headers::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if (line.starts_with(' ') || line.starts_with('\t'))
            && let Some(last) = headers.iter().last().map(|(k, _)| k.to_string())
            && let Some(prev) = headers.get(&last).map(ToOwned::to_owned)
        {
            headers.insert(&last, &format!("{prev} {}", line.trim()));
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.append(name.trim(), value.trim());
        }
    }
    Ok((status, reason, headers))
}

fn read_chunked<R: Read>(
    reader: &mut BufReader<R>,
    max_body: usize,
    deadline: Instant,
) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        read_line(reader, &mut size_line, deadline)?;
        let size = parse_chunk_size(&size_line)?;
        if size == 0 {
            // Consume trailers up to the terminating empty line, but bound both
            // their count and size so a hostile peer cannot stall the reader.
            let mut trailer = String::new();
            let mut trailer_lines = 0_usize;
            let mut trailer_bytes = 0_usize;
            loop {
                trailer.clear();
                read_line(reader, &mut trailer, deadline)?;
                trailer_lines += 1;
                trailer_bytes = trailer_bytes.saturating_add(trailer.len());
                if trailer_lines > 100 || trailer_bytes > 8 * 1024 {
                    return Err(HttpError::Protocol("chunk trailers too large".into()));
                }
                if trailer.trim().is_empty() {
                    break;
                }
            }
            return Ok(body);
        }
        let total = body
            .len()
            .checked_add(size)
            .ok_or(HttpError::PayloadTooLarge)?;
        if total > max_body {
            return Err(HttpError::PayloadTooLarge);
        }
        let start = body.len();
        body.resize(total, 0);
        read_exact_deadline(reader, &mut body[start..], deadline)?;
        let mut crlf = [0_u8; 2];
        read_exact_deadline(reader, &mut crlf, deadline)?;
    }
}

/// Parses a chunk-size line, accepting optional whitespace and `;` extensions.
fn parse_chunk_size(line: &str) -> Result<usize, HttpError> {
    let bad = || HttpError::Protocol(format!("bad chunk size: {line}"));
    if line.bytes().any(|byte| byte < 0x20 && byte != b'\t') {
        return Err(bad());
    }
    let size_field = line.split(';').next().unwrap_or("").trim();
    if size_field.is_empty()
        || size_field.len() > 16
        || !size_field.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(bad());
    }
    usize::from_str_radix(size_field, 16).map_err(|_| bad())
}

fn read_line<R: Read>(
    reader: &mut BufReader<R>,
    out: &mut String,
    deadline: Instant,
) -> Result<(), HttpError> {
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        if read_some_deadline(reader, &mut byte, deadline)? == 0 {
            if buf.is_empty() {
                return Err(HttpError::Protocol("unexpected EOF".into()));
            }
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            buf.push(byte[0]);
        }
        if buf.len() > 8 * 1024 {
            return Err(HttpError::Protocol("chunk line too long".into()));
        }
    }
    out.push_str(&String::from_utf8_lossy(&buf));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    fn parse(text: &str) -> Response {
        let url = Url::parse("http://a.test/").unwrap();
        read_response(
            BufReader::new(text.as_bytes()),
            &url,
            Method::Get,
            1024 * 1024,
            soon(),
        )
        .unwrap()
    }

    #[test]
    fn parses_content_length_response() {
        let response =
            parse("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\nhello");
        assert_eq!(response.status, 200);
        assert_eq!(response.reason, "OK");
        assert_eq!(response.body, b"hello");
    }

    #[test]
    fn parses_chunked_response() {
        let response = parse(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
        );
        assert_eq!(response.body, b"hello world");
    }

    #[test]
    fn reads_body_until_eof_without_length() {
        let response = parse("HTTP/1.0 200 OK\r\n\r\nraw body");
        assert_eq!(response.body, b"raw body");
    }

    #[test]
    fn rejects_garbage_status_line() {
        let url = Url::parse("http://a.test/").unwrap();
        let err = read_response(
            BufReader::new(&b"NOT HTTP\r\n\r\n"[..]),
            &url,
            Method::Get,
            1024,
            soon(),
        )
        .unwrap_err();
        assert!(matches!(err, HttpError::Protocol(_)));
    }

    #[test]
    fn head_requests_ignore_body() {
        let url = Url::parse("http://a.test/").unwrap();
        let response = read_response(
            BufReader::new(&b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n"[..]),
            &url,
            Method::Head,
            1024,
            soon(),
        )
        .unwrap();
        assert!(response.body.is_empty());
    }

    #[test]
    fn deadline_check_rejects_elapsed_instant() {
        assert!(check_deadline(Instant::now() - Duration::from_secs(1)).is_err());
        assert!(check_deadline(soon()).is_ok());
    }

    #[test]
    fn reads_fail_when_deadline_passes_mid_body() {
        let url = Url::parse("http://a.test/").unwrap();
        let err = read_response(
            BufReader::new(&b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"[..]),
            &url,
            Method::Get,
            1024,
            Instant::now() - Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(err, HttpError::Timeout(_)));
    }

    #[test]
    fn dns_resolution_is_bounded_by_timeout() {
        // A zero-length budget cannot possibly receive the worker's result, so
        // resolution must fall back to a clear timeout error rather than block.
        let err = resolve_addrs("localhost", 80, Duration::ZERO).unwrap_err();
        assert!(matches!(err, HttpError::Timeout(_)), "got {err:?}");
    }

    #[test]
    fn redirect_chain_detects_loops() {
        let start = Url::parse("https://a.test/start").unwrap();
        let back = Url::parse("https://a.test/start#different-fragment").unwrap();
        let fresh = Url::parse("https://a.test/next").unwrap();
        let chain = RedirectChain::new(&start, 10, Duration::from_secs(24));

        // A fragment-only difference is still the same hop.
        assert!(matches!(
            chain.check(&back, 1),
            Err(HttpError::RedirectLoop(_))
        ));
        assert!(chain.check(&fresh, 1).is_ok());
    }

    #[test]
    fn redirect_chain_caps_hop_count() {
        let start = Url::parse("https://a.test/").unwrap();
        let next = Url::parse("https://a.test/next").unwrap();
        let chain = RedirectChain::new(&start, 3, Duration::from_secs(24));
        assert!(matches!(
            chain.check(&next, 3),
            Err(HttpError::TooManyRedirects(3))
        ));
        assert!(chain.check(&next, 2).is_ok());
    }

    #[test]
    fn redirect_chain_caps_total_time() {
        let start = Url::parse("https://a.test/").unwrap();
        let next = Url::parse("https://a.test/next").unwrap();
        let mut chain = RedirectChain::new(&start, 10, Duration::from_secs(24));
        chain.deadline = Instant::now() - Duration::from_secs(1);
        assert!(matches!(chain.check(&next, 1), Err(HttpError::Timeout(_))));
    }

    #[test]
    fn default_timeout_is_snappy() {
        assert_eq!(ClientConfig::default().timeout, Duration::from_secs(8));
    }

    #[test]
    fn parses_chunk_size_extensions() {
        assert_eq!(parse_chunk_size("1a;name=value;flag").unwrap(), 26);
        assert_eq!(parse_chunk_size(" 5 \t; ext=1").unwrap(), 5);
        assert_eq!(parse_chunk_size("0").unwrap(), 0);
        assert!(parse_chunk_size("").is_err());
        assert!(parse_chunk_size("1g").is_err());
        assert!(parse_chunk_size("0x10").is_err());
        assert!(parse_chunk_size("12345678901234567").is_err());
        assert!(parse_chunk_size("1;\nfoo").is_err());
    }

    #[test]
    fn parses_chunked_response_with_extensions_and_trailers() {
        let response = parse(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
             5;name=value\r\nhello\r\n6;x=y\r\n world\r\n0\r\nChecksum: abc\r\n\r\n",
        );
        assert_eq!(response.body, b"hello world");
    }

    #[test]
    fn decodes_gzip_response_body() {
        // gzip of "ABC" with a stored (uncompressed) DEFLATE block.
        let gzip: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x01, 0x03, 0x00, 0xfc,
            0xff, 0x41, 0x42, 0x43, 0x48, 0x03, 0x83, 0xa3, 0x03, 0x00, 0x00, 0x00,
        ];
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n",
            gzip.len()
        );
        let mut wire = head.into_bytes();
        wire.extend_from_slice(gzip);
        let url = Url::parse("http://a.test/").unwrap();
        let response = read_response(
            BufReader::new(&wire[..]),
            &url,
            Method::Get,
            1024 * 1024,
            soon(),
        )
        .unwrap();
        assert_eq!(response.body, b"ABC");
    }

    #[test]
    fn rejects_unsupported_content_encoding() {
        let url = Url::parse("http://a.test/").unwrap();
        let err = read_response(
            BufReader::new(
                &b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nContent-Length: 1\r\n\r\nX"[..],
            ),
            &url,
            Method::Get,
            1024,
            soon(),
        )
        .unwrap_err();
        assert!(matches!(err, HttpError::UnsupportedContentEncoding(_)));
    }

    #[test]
    fn prepare_advertises_supported_encodings() {
        let client = Client::new(ClientConfig::default());
        let mut req = Request::get_str("http://a.test/").unwrap();
        client.prepare(&mut req);
        assert_eq!(req.headers.get("Accept-Encoding"), Some("gzip, deflate"));
    }
}
