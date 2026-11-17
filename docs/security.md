# Security model

## Threat model

The engine treats HTML, CSS, JavaScript, response bodies and URLs as untrusted
input. The goals for the current stage:

1. Never panic or abort on malformed input (parsers recover, errors are values).
2. Never read a file outside a sandbox root through a path supplied by content.
3. Never let a script navigate or read cross-origin data without an origin check.
4. Never execute a page's scripts with native privileges that the page did not ask
   for through a permission decision.

Out of scope for now: process isolation and site isolation. Pages and scripts run
in the engine process. This is the single largest security gap and is recorded in
the roadmap; until it is closed, the engine should only browse content you trust.

## Implemented guarantees

### Parsing safety

- The HTML tokenizer/tree builder recovers from arbitrary bytes, has a depth limit
  (`ParseOptions::max_depth`) and is covered by a random-input test.
- The CSS parser skips invalid declarations and rules without failing the sheet.
- The JavaScript parser returns `ParseError`; the interpreter returns `JsError`.
  Exceptions never unwind into Rust panics.
- No `unwrap()` on untrusted data paths in the production code of parsers, network,
  layout or paint.

### Path handling (`browser-platform::filesystem`)

`SandboxedFileSystem` canonicalises a requested path and requires the result to
stay under the sandbox root. `..` traversal, absolute paths outside the root and
symlinks that escape the root are rejected with `PermissionDenied`.

### URLs and origins (`browser-url`, `browser-security`)

- Origins are `scheme://host:port` tuples with default ports normalised; opaque
  origins (`file:`) never match anything, including other opaque origins.
- `browser-security::origin::OriginPolicy::allows_read` enforces same-origin reads.
- Redirects are resolved against the current URL and restricted to `http(s)`;
  `javascript:` and other schemes are rejected instead of followed.

### CORS (`browser-security::cors`)

Request and response evaluation implement: simple request detection, preflight
requirements for non-simple methods/headers, `Access-Control-Allow-Origin`
(including the `*` + credentials restriction), allowed methods and headers,
credentials mode, and `Access-Control-Max-Age` parsing. `Vary: Origin` handling is
not enforced (documented limitation).

### Content Security Policy (`browser-security::csp`)

`Policy` parses `default-src`, `script-src`, `style-src`, `img-src`,
`connect-src`, `font-src`, `media-src` with `'self'`, `'none'`, `'unsafe-inline'`,
`*`, scheme sources, host sources with ports and `*.example.com` wildcards.
Multiple policies combine most-restrictively (`CspList`). Nonces, hashes,
`'strict-dynamic'`, `report-only` and `frame-ancestors` are not enforced yet.

### Permissions (`browser-security::permissions`)

`PermissionStore` keeps per-origin decisions (`Granted`/`Denied`/`Prompt`) for
geolocation, notifications, clipboard, camera, microphone, fullscreen and storage.
The default is `Prompt`; nothing is auto-granted.

### Sandboxing (`browser-security::sandbox`)

`SandboxFlags` models `allow-scripts`, `allow-same-origin`, `allow-forms`,
`allow-popups`, `allow-downloads`, `allow-modals`; every capability is denied by
default, unknown tokens are ignored rather than partially honoured, and
`origin_for` returns an opaque origin when `allow-same-origin` is absent.

### Engine-side restrictions

- `Settings::enable_network = false` (headless default) makes every `http(s)`
  navigation fail with `NetworkDisabled`.
- `Settings::enable_scripting = false` is the default: scripts are collected but
  not executed unless explicitly enabled.
- `Settings::max_body_bytes` bounds response bodies; the HTTP client rejects
  oversized `Content-Length` and chunked bodies with `PayloadTooLarge`.
- The HTTP client sends `Accept-Encoding: identity`, so no decompression bomb can
  be triggered by a server choosing a compression scheme.
- Cookies are scoped by domain, path, `Secure` and `HttpOnly`; host-only cookies
  cannot leak to subdomains and `Secure` cookies are never sent over `http`.

## Current limitations

- No process/thread isolation, no site isolation, no seccomp-style sandbox.
- The JS runtime exposes the full DOM of the page; cross-frame isolation does not
  exist because there are no frames yet.
- CSP is available as a policy engine but is not yet wired into script/style/fetch
  loading by `browser-core`.
- CORS uses cookies presence to infer credentials; explicit mode is available via
  `evaluate_response_with_credentials`.
- The sandbox's filesystem check is lexical plus `canonicalize`, so a TOCTOU window
  exists between validation and opening the file.
- `fetch()` is not exposed to scripts yet; script-driven network requests are
  therefore impossible, which is safe but incomplete.
