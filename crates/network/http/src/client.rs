//! HTTP/1.1 client: connection management, TLS, body framing and redirects.

use std::io::{self, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use browser_url::Url;

use crate::cache::HttpCache;
use crate::cookies::CookieJar;
use crate::headers::Headers;
use crate::redirect::{RedirectOutcome, RedirectPolicy};
use crate::request::{Method, Request};
use crate::response::Response;

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
    /// The response body exceeded the configured limit.
    PayloadTooLarge,
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
            Self::PayloadTooLarge => f.write_str("response body too large"),
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
    /// Connect/read/write timeout.
    pub timeout: Duration,
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
            timeout: Duration::from_secs(20),
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

        if method == Method::Get
            && let Some(cached) = self.cache.get(&url.as_str())
        {
            return Ok(cached);
        }

        let mut hops = 0_usize;
        loop {
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
            let response = self.send(&req)?;

            self.cookies.store_response(&url, &response);

            if self.config.follow_redirects && response.is_redirect() {
                let policy = RedirectPolicy {
                    max_redirects: self.config.max_redirects,
                    allow_cross_scheme: true,
                };
                match policy.resolve(&url, &response, method, hops)? {
                    RedirectOutcome::Follow(next, next_method) => {
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
        req.headers.insert("Accept-Encoding", "identity");
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

    fn send(&mut self, req: &Request) -> Result<Response, HttpError> {
        let url = &req.url;
        let host = url
            .host()
            .ok_or(HttpError::UnsupportedScheme("no host".into()))?;
        let Some(port) = url.port_or_default() else {
            return Err(HttpError::UnsupportedScheme(url.scheme().to_string()));
        };
        let stream = connect(host, port, self.config.timeout)?;

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
            stream.write_all(&body)?;
            stream.flush()?;
            read_response(
                BufReader::new(stream),
                url,
                req.method,
                self.config.max_body_bytes,
            )?
        } else {
            let mut stream = stream;
            stream.write_all(&body)?;
            stream.flush()?;
            read_response(
                BufReader::new(stream),
                url,
                req.method,
                self.config.max_body_bytes,
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

fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, HttpError> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(HttpError::Io)?
        .collect::<Vec<_>>();
    let mut last_err = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(timeout));
                let _ = stream.set_write_timeout(Some(timeout));
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
) -> Result<Response, HttpError> {
    let (status, reason, headers) = read_head(&mut reader)?;
    let mut response = Response::new(url.clone(), status, reason, headers, Vec::new());

    if method == Method::Head || matches!(status, 204 | 304) {
        return Ok(response);
    }

    let chunked = response
        .headers
        .get("Transfer-Encoding")
        .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"));

    if chunked {
        response.body = read_chunked(&mut reader, max_body)?;
    } else if let Some(len) = response
        .headers
        .get("Content-Length")
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if len > max_body {
            return Err(HttpError::PayloadTooLarge);
        }
        let mut buf = vec![0_u8; len];
        reader.read_exact(&mut buf)?;
        response.body = buf;
    } else {
        let mut buf = Vec::new();
        reader
            .take(max_body as u64)
            .read_to_end(&mut buf)
            .map_err(HttpError::Io)?;
        response.body = buf;
    }
    Ok(response)
}

fn read_head<R: Read>(reader: &mut BufReader<R>) -> Result<(u16, String, Headers), HttpError> {
    let mut raw = Vec::with_capacity(1024);
    let mut byte = [0_u8; 1];
    loop {
        if reader.read(&mut byte)? == 0 {
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

fn read_chunked<R: Read>(reader: &mut BufReader<R>, max_body: usize) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        read_line(reader, &mut size_line)?;
        let size_str = size_line.trim().split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| HttpError::Protocol(format!("bad chunk size: {size_str}")))?;
        if size == 0 {
            // Consume trailers up to the terminating empty line.
            let mut trailer = String::new();
            loop {
                trailer.clear();
                read_line(reader, &mut trailer)?;
                if trailer.trim().is_empty() {
                    break;
                }
            }
            return Ok(body);
        }
        if body.len() + size > max_body {
            return Err(HttpError::PayloadTooLarge);
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        let mut crlf = [0_u8; 2];
        reader.read_exact(&mut crlf)?;
    }
}

fn read_line<R: Read>(reader: &mut BufReader<R>, out: &mut String) -> Result<(), HttpError> {
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        if reader.read(&mut byte)? == 0 {
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

    fn parse(text: &str) -> Response {
        let url = Url::parse("http://a.test/").unwrap();
        read_response(
            BufReader::new(text.as_bytes()),
            &url,
            Method::Get,
            1024 * 1024,
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
        )
        .unwrap();
        assert!(response.body.is_empty());
    }
}
