//! Same-origin policy, CORS, CSP and permission checks.
//!
//! This crate holds the engine's security decisions. It is deliberately policy
//! rich and I/O free: parsers and evaluators take request/response data as plain
//! values, return explicit allow/deny decisions, and never touch the network or
//! the DOM.
//!
//! # Threat model
//!
//! * A hostile page can issue cross-origin requests, embed cross-origin resources,
//!   navigate frames and ask for powerful capabilities. Every such action is
//!   checked against an explicit origin, a CORS decision, a CSP policy or a stored
//!   permission rather than trusted by default.
//! * Opaque origins (`data:`, `javascript:`, `file:` and sandboxed documents
//!   without `allow-same-origin`) never compare equal to any origin. They cannot
//!   read resources, pass `OriginPolicy::validate`, or satisfy `'self'`.
//! * A sandboxed document starts with every capability denied; only explicit
//!   `allow-*` tokens grant one.
//! * Multiple CSP policies combine most-restrictively: a resource must be allowed
//!   by every policy.
//! * Permissions are stored per origin, so a grant to one site cannot leak to
//!   another. The default is [`permissions::PermissionState::Prompt`].
//!
//! # Limitations
//!
//! * Origins are exact scheme/host/port tuples. `document.domain` relaxation and
//!   `postMessage` target-origin checks are not implemented here.
//! * CORS credentials are inferred from `Cookie`/`Authorization`; an explicit
//!   credentials mode is available through
//!   [`cors::evaluate_response_with_credentials`]. `Vary: Origin` is parsed by
//!   [`cors::CorsResponse::varies_on_origin`] but caching is the caller's job.
//! * CSP supports the fetch directives listed in [`csp`], with `'self'`, `'none'`,
//!   `'unsafe-inline'`, `*`, scheme sources, host sources, nonces, hashes and
//!   `'strict-dynamic'`. Hash digests are computed by the caller; this crate only
//!   matches them. Report-only policies, reports and `frame-ancestors` are not
//!   enforced.
//! * Mixed content is classified from plain URLs by [`mixed_content`]; the
//!   caller must apply the returned upgrade or block decision to the load.
//! * The permission store is in-memory and per process; persistence and a user
//!   prompt UI belong to higher layers.
//! * Sandbox supports the six capability tokens modelled in [`sandbox`]; tokens
//!   such as `allow-top-navigation` are ignored rather than half-enforced.
//!
//! ```
//! use browser_security::{OriginPolicy, same_origin};
//! use browser_url::Url;
//!
//! let a = OriginPolicy::from_url(&Url::parse("https://example.com/").unwrap()).unwrap();
//! let b = OriginPolicy::from_url(&Url::parse("https://example.com:443/x").unwrap()).unwrap();
//! assert!(same_origin(&a, &b));
//! ```

#![forbid(unsafe_code)]

pub mod cors;
pub mod csp;
pub mod mixed_content;
pub mod origin;
pub mod permissions;
pub mod sandbox;

pub use browser_url::Origin;
pub use cors::{
    CorsDecision, CorsError, CorsRequest, CorsResponse, RequestKind, evaluate_request,
    evaluate_response, evaluate_response_with_credentials, is_simple_header, is_simple_method,
    parse_header_list,
};
pub use csp::{
    ContentCheck, ContentHash, CspList, HashAlgorithm, HashSource, HostSource, Policy,
    SourceExpression, decode_base64, encode_base64,
};
pub use mixed_content::{
    MixedContentDecision, MixedContentMode, SubresourceKind, allows as mixed_content_allows,
    check as check_mixed_content, check_with_context, check_with_mode, upgraded_url,
};
pub use origin::{
    OriginError, OriginHeader, OriginPolicy, SubresourceOrigin, allows_read,
    redirect_stays_same_origin, resource_origin, same_origin, same_origin_redirect, validate,
};
pub use permissions::{Permission, PermissionState, PermissionStore};
pub use sandbox::{Sandbox, SandboxCapability, SandboxFlags, parse_sandbox_attribute};
