# Architecture

Mag is a browser engine written from scratch in Rust. No existing browser engine
(Chromium/Blink, WebKit, Gecko, Servo) and no existing JavaScript engine (V8,
JavaScriptCore, SpiderMonkey, Boa) is used. The only third-party code is transport
(`rustls`, `webpki-roots`) and the Rust standard library.

## Workspace layout

```
apps/
  browser/           windowed shell (feature-gated GPU/windowing backend)
  browser-cli/       stage-by-stage inspection: tree, styles, layout, commands, render
  browser-headless/  deterministic PNG renderer (screenshots in CI)
crates/
  network/url         WHATWG-style URL parser, origin model
  network/http        HTTP/1.1 client, TLS, cookies, cache, redirects
  network/resource-loader  fetch + MIME sniffing + preload scanning
  html/dom            arena DOM, mutations, traversal, serialisation
  html/html-parser    tokenizer + tree builder → DOM
  html/selectors      selector parsing and matching
  css/css-parser      CSS tokenizer → stylesheet AST, component values
  css/style           cascade, inheritance, var(), media queries, computed style
  layout/text         bitmap font metrics, shaping information, line breaking
  layout/layout       box tree, block/inline/flex layout, scrolling, fragments
  graphics/paint      display list + software rasterizer + PNG encoder
  graphics/compositor layers, tiles, damage, clip stack, animation clocks
  graphics/gpu        device abstraction; WGSL shaders; software reference device
  javascript/js-parser  lexer, AST, recursive-descent parser
  javascript/js-runtime tree-walking interpreter, objects, closures, GC, promises
  web/event-loop      task/microtask scheduler, timers
  web/web-apis        document/element/event/timer/storage/location bindings
  web/navigation      session history, navigation actions, lifecycle
  platform/browser-core  engine facade: tabs, pages, navigation, rendering
  platform/security   same-origin, CORS, CSP, permissions, sandbox flags
  platform/ipc        component messages and channels
  platform/platform   window/input/clipboard/filesystem abstractions
```

## Dependency graph (acyclic, left depends on right)

```
apps  →  browser-core
browser-core → http, resource-loader, url, html-parser, dom, selectors, css,
               style, layout, text, paint, compositor, js, web-apis, navigation,
               security, ipc, platform, event-loop
web-apis     → js, dom, html-parser, selectors, url, event-loop
style        → css, dom, selectors
layout       → dom, style, text
paint        → layout, style, css, text
compositor   → layout, paint
gpu          → paint, layout, compositor
navigation   → url, event-loop
security     → url
http         → url
html-parser  → dom
selectors    → dom
```

Hard rules enforced by this graph:

- The DOM never depends on style, layout, paint or JavaScript.
- The CSS parser never depends on the DOM or any graphics crate.
- The JavaScript parser never depends on the browser or the DOM.
- Only `browser-core` (and `web-apis`) may know about several subsystems at once.

## Data flow of one page load

```
URL ─▶ http client ─▶ HTML text
                        │
                        ├─▶ html-parser ─▶ DOM (arena of nodes)
                        │
   <style> / <link> ────┴─▶ css-parser ─▶ style engine ─▶ StyledTree (NodeId → ComputedStyle)
                                                            │
                                                            ▼
                                        layout engine ─▶ LayoutTree (boxes, lines, fragments)
                                                            │
                                                            ▼
                                          paint ─▶ DisplayList ─▶ rasterizer / GPU ─▶ RGBA
                                                            │
                        javascript ─▶ DOM mutation ─▶ recompute style/layout/paint
```

Recomputation is whole-document today. The pieces needed for incremental work
(`StyledTree` keyed by node, damage rectangles, tile grids) exist, and the
compositor consumes them, but invalidation is coarse: any DOM mutation triggers a
full restyle and relayout. This is a documented limitation, not an accident.

## Public contracts

| Crate | Key types | Stability |
| --- | --- | --- |
| `browser-url` | `Url`, `Origin`, `UrlError` | stable |
| `browser-http` | `Client`, `Request`, `Response`, `Headers`, `CookieJar` | stable |
| `browser-dom` | `Document`, `Node`, `NodeId`, `Attributes`, `Namespace` | stable |
| `browser-html-parser` | `parse_document`, `parse_fragment`, `Tokenizer`, `Token` | stable |
| `browser-selectors` | `Selector`, `parse_selectors`, `matches_with_state`, `query_selector_all` | stable |
| `browser-css` | `Stylesheet`, `Rule`, `Declaration`, `Value`, `ComponentValue`, `Color`, `Length` | stable |
| `browser-style` | `StyleEngine`, `StyledTree`, `ComputedStyle`, `Property`, `Dim` | stable |
| `browser-layout` | `layout`, `LayoutTree`, `LayoutBox`, `Rect`, `Edges` | stable |
| `browser-paint` | `build`, `DisplayList`, `DisplayCommand`, `render`, `ImageBuffer` | stable |
| `browser-js` | `Runtime`, `Value`, `HostFunction`, `JsError` | stable |
| `browser-web-apis` | `install`, `HostState`, `dispatch_event` | evolving |
| `browser-core` | `Browser`, `Page`, `Settings`, `Snapshot` | stable |

`NodeId` is a plain index into the document arena. It is stable per document and is
used as the key for styles, layout boxes, event listeners and element wrappers, so
the JS bindings never need a second node identity.

## Key design decisions

1. **Arena DOM, not `Rc<RefCell<Node>>`.** Traversal and style matching touch the
   tree constantly; indices keep it cache-friendly and let layout borrow the
   document immutably while building boxes.
2. **Component values in the CSS AST.** The parser keeps ordered component values
   (idents, dimensions, functions, commas, whitespace) instead of a fixed property
   struct. Shorthand expansion and property interpretation live in `browser-style`,
   so adding a property never touches the tokenizer.
3. **Display list as the boundary.** The rasterizer and the GPU backend consume the
   same commands, which makes screenshot tests exact and gives the future `wgpu`
   backend a contract that is already exercised.
4. **Deterministic rendering.** The built-in monospace bitmap font, the software
   rasterizer and the PNG encoder produce byte-identical output for identical input,
   with no system fonts, no GPU and no threads in the render path.
5. **Explicit errors, no panics on untrusted input.** The HTML tokenizer recovers
   from malformed markup, CSS skips invalid declarations, and network errors become
   `HttpError`/`BrowserError` values.
6. **Honest scope.** Unsupported features (floats, grid, full table formatting,
   regex literals, native GPU path) are recorded in `docs/roadmap.md` and in code
   doc comments rather than silently faked.

## Concurrency and Git workflow

Development happens on `develop` only. Because every agent shares one working tree
and one index, Git operations are serialised: a single integrator (the orchestrator)
stages and commits each task's files atomically after validating them. Subagents
never run Git commands, so there is no index race, no lost work and the history
stays linear.

## Known architectural limitations

- No process isolation yet: pages and scripts run in the engine process. The
  `browser-security` crate provides the policy primitives, but there is no
  site-isolation boundary.
- One render thread; layout and paint are synchronous.
- No incremental style/layout invalidation.
- No native windowing backend in the default build (see `graphics/gpu`).
