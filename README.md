# Mag — a browser engine written from scratch in Rust

Mag is an independent browser engine. It does **not** embed Chromium, WebKit, Gecko,
Servo, V8, JavaScriptCore, SpiderMonkey or Boa. Every subsystem in this workspace is
implemented here: URL parsing, HTTP/1.1 + TLS, HTML tokenizer and tree builder, DOM,
CSS parser, selectors, cascade, layout, painting, PNG encoding, a JavaScript parser
and interpreter with its own garbage collector, DOM bindings, an event loop,
navigation and security policy primitives.

## What works today

- **Networking** — HTTP/1.1 over TCP with TLS, chunked and length-delimited bodies,
  redirects, cookies, `Cache-Control` caching, timeouts and size limits.
- **HTML** — WHATWG-style tokenizer and tree builder: insertion modes, void
  elements, optional end tags, rawtext/RCDATA, entities, misnested-markup recovery,
  SVG/MathML namespaces.
- **CSS** — full tokenizer/parser into component values, at-rules, colours,
  selectors (all combinators, `:not/:is/:where`, `nth-child`), specificity,
  cascade with origins/`!important`/inline styles, inheritance, `var()`, media
  queries and a user-agent stylesheet.
- **Layout** — block flow with margin collapsing, inline flow with line breaking,
  `text-align`, `white-space`, inline-blocks, flexbox (single line), absolute and
  fixed positioning, relative offsets, overflow scrolling, hit testing.
- **Rendering** — display list, software rasterizer with an embedded bitmap font,
  borders, backgrounds, images, clipping, scrolling, a dependency-free PNG encoder
  and a deterministic headless renderer.
- **JavaScript** — lexer, parser, tree-walking interpreter, closures, prototypes,
  exceptions, promises/microtasks and a mark-and-sweep GC.
- **Web APIs** — `document`, element wrappers, `textContent`, `innerHTML`,
  `appendChild`, `querySelector(All)`, `addEventListener` with bubbling,
  `setTimeout`/`setInterval`, `localStorage`, `console`, `location`.
- **Browser core** — tabs, navigation with history, address-bar normalisation, CSP
  enforcement for scripts and stylesheets, rendering and PNG screenshots.

## Quick start

```sh
# Render a local page to PNG (deterministic, no network)
cargo run -p browser-headless -- --html tests/fixtures/page.html --out out/page.png

# Inspect each stage
cargo run -p browser-cli -- tree    tests/fixtures/page.html
cargo run -p browser-cli -- styles  tests/fixtures/page.html "h1"
cargo run -p browser-cli -- layout  tests/fixtures/page.html
cargo run -p browser-cli -- render  tests/fixtures/page.html -o out/page.png

# Interactive browser shell (frames written as PNG)
cargo run -p mag-browser -- tests/fixtures/script.html --script

# Validation gate
./tools/conformance/run.sh     # fmt + check + test + clippy -D warnings
```

## Limitations (documented, not hidden)

No native window/GPU path yet (frames are PNGs), no image decoders in the default
build until the media task lands, no floats/grid in layout until the layout task
lands, no regex literals or classes in JavaScript, no process isolation. See
`docs/roadmap.md` for the full list and `docs/security.md` for the threat model.

## Layout

| Path | Purpose |
| --- | --- |
| `apps/browser` | Browser shell (tabs, address bar, commands, PNG frames) |
| `apps/browser-cli` | Stage-by-stage inspection CLI |
| `apps/browser-headless` | Deterministic PNG renderer for CI |
| `crates/network` | URL, HTTP+TLS, resource loader, preload scanning |
| `crates/html` | DOM, HTML parser, selectors |
| `crates/css` | CSS parser, cascade and computed style |
| `crates/layout` | Text metrics, block/inline/flex layout |
| `crates/graphics` | Display list, rasterizer, compositor, GPU abstraction |
| `crates/javascript` | JS lexer, parser, interpreter, GC |
| `crates/web` | Event loop, web APIs, navigation |
| `crates/platform` | Browser core, security, IPC, OS abstractions |
| `tests/integration` | End-to-end and local-HTTP-server tests |

See `docs/architecture.md` for the dependency graph and the contracts between crates.
