//! HTTP/1.1 client with TLS, cookies, caching and redirects.
//!
//! The transport layer is deliberately simple: one TCP connection per request
//! (`Connection: close`) with `rustls` for HTTPS. No browser engine, and no
//! high-level HTTP library, is involved.
//!
//! ```
//! use browser_http::{Client, ClientConfig};
//! let mut client = Client::new(ClientConfig::default());
//! // Network access is not required for the crate's unit tests.
//! let _ = &mut client;
//! ```

#![forbid(unsafe_code)]

pub mod cache;
pub mod client;
pub mod cookies;
pub mod encoding;
pub mod headers;
pub mod redirect;
pub mod request;
pub mod response;

pub use cache::HttpCache;
pub use client::{Client, ClientConfig, DEFAULT_USER_AGENT, HttpError};
pub use cookies::{Cookie, CookieJar};
pub use encoding::ContentEncoding;
pub use headers::Headers;
pub use redirect::{RedirectOutcome, RedirectPolicy};
pub use request::{Method, Request, RequestConfig};
pub use response::{Charset, Response};
