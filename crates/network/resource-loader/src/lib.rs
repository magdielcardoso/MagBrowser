//! Resource loading: HTTP(S) and `file:` fetching, MIME sniffing and preload
//! scanning.
//!
//! ```
//! use browser_url::Url;
//! use browser_resource_loader::preload::{PreloadKind, scan};
//!
//! let base = Url::parse("https://example.com/").unwrap();
//! let requests = scan("<link rel=stylesheet href=site.css>", Some(&base));
//! assert_eq!(requests[0].kind, PreloadKind::Style);
//! ```

#![forbid(unsafe_code)]

pub mod fetch;
pub mod loader;
pub mod mime;
pub mod preload;

pub use fetch::Resource;
pub use loader::{LoadError, ResourceLoader};
pub use preload::{PreloadKind, PreloadRequest, scan};
