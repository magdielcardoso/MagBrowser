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
  cascade with origins/`!important`/inline styles, inheritance, `var()`,
  `calc()`, HTML presentational hints (`<img width/height>`), media queries and a
  user-agent stylesheet.
- **Layout** — block flow with margin collapsing, inline flow with line breaking,
  `text-align`, `white-space`, inline-blocks, flexbox (wrap/wrap-reverse,
  multi-line), floats with shrink-to-fit, a CSS Grid subset, automatic table
  column widths, absolute and fixed positioning, relative offsets, overflow
  scrolling, hit testing.
- **Rendering** — display list, software rasterizer with an embedded Inter outline
  font and anti-aliased text, borders, backgrounds, decoded PNG/GIF/BMP images,
  clipping, scrolling, a dependency-free PNG encoder and a deterministic headless
  renderer. Rasterizer primitives for rounded rectangles, soft shadows and linear
  gradients exist but are not yet driven by CSS (see `docs/roadmap.md`).
- **JavaScript** — lexer, parser, tree-walking interpreter, closures, prototypes,
  exceptions, promises/microtasks and a mark-and-sweep GC.
- **Web APIs** — `document`, element wrappers, `textContent`, `innerHTML`,
  `appendChild`, `querySelector(All)`, `addEventListener` with bubbling,
  `setTimeout`/`setInterval`, `localStorage`, `console`, `location`.
- **Browser core** — tabs, navigation with history, address-bar normalisation, CSP
  enforcement for scripts and stylesheets, rendering and PNG screenshots.
- **Desktop window** — `--features window` opens a native `winit` + `wgpu` window
  with engine-drawn chrome (tabs, back/forward/reload, address field, status pill);
  `--window-selfcheck` renders the same composition to PNG.

## Quick start

```sh
# Render a local page to PNG (deterministic, no network)
cargo run -p browser-headless -- --html tests/fixtures/page.html --out out/page.png

# Inspect each stage
cargo run -p browser-cli -- tree    tests/fixtures/page.html
cargo run -p browser-cli -- styles  tests/fixtures/page.html "h1"
cargo run -p browser-cli -- layout  tests/fixtures/page.html
cargo run -p browser-cli -- render  tests/fixtures/page.html -o out/page.png

# Native desktop window (adds wgpu/winit)
cargo run -p mag-browser --features window -- tests/fixtures/page.html

# Window composition to PNG (works without the feature too)
cargo run -p mag-browser -- --window-selfcheck out/window.png tests/fixtures/page.html

# Interactive browser shell (frames written as PNG)
cargo run -p mag-browser -- tests/fixtures/script.html --script

# Validation gate
./tools/conformance/run.sh     # fmt + check + test + clippy -D warnings
```

## Limitations (documented, not hidden)

The native window is feature-gated (default build stays GPU-free) and there is
no process isolation yet. Modern CSS largely works — `calc()`/`min()`/`max()`/
`clamp()`, flexbox with wrapping and intrinsic sizing, grid placement,
`border-radius`, shadows, gradients, transforms and filters are implemented —
but it is not a full Chrome-equivalent engine; complex sites may still render
imperfectly. JavaScript supports classes, generators, async/await, modules and
`fetch`, but lacks the full standard library. WebP decoding is still in
progress; PNG/JPEG/GIF/BMP/SVG are supported. See `docs/roadmap.md` for the
full list and `docs/security.md` for the threat model.

## License

MagBrowser is free software licensed under the GNU Affero General Public
License, version 3 or (at your option) any later version (`AGPL-3.0-or-later`).
See [LICENSE](LICENSE) for the full text and [NOTICE](NOTICE) for third-party
component licenses (including the Inter font under the SIL Open Font License).

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
