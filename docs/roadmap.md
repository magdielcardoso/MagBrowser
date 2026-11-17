# Roadmap

Status legend: **done** = implemented and tested, **partial** = working subset
with documented gaps, **planned** = designed but not implemented.

## Phase 0 — Foundation — done

- Cargo workspace with 25 crates, edition 2024, rustfmt/clippy configuration.
- Git repository on `develop`, linear history, atomic commits.
- `cargo fmt --all --check`, `cargo check --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings` are the acceptance gate.

## Phase 1 — Networking, HTML, DOM — done

- URL parsing, relative resolution, percent encoding, origins.
- HTTP/1.1 client over TCP with TLS (`rustls`), chunked and length-delimited
  bodies, redirects, cookies, `Cache-Control` cache, timeouts and size limits.
- HTML tokenizer and tree builder: insertion modes, void elements, optional end
  tags, rawtext/RCDATA, entities (named, numeric, legacy), adoption-agency
  recovery, foreign content (SVG/MathML).
- DOM arena: attributes, mutations, traversal iterators, serialisation, text
  content, queries by id/class/tag.
- CLI: `mag-cli tree|html|fetch`.

## Phase 2 — CSS and style — done

- CSS tokenizer and parser with component values, at-rules (`@media`, `@import`,
  `@keyframes`, `@font-face`), `!important`, colour parsing (hex/rgb/hsl/named).
- Selectors: type/class/id/attribute, the four combinators, `:not/:is/:where`,
  `nth-child` formulas, pseudo-elements, specificity per Selectors 4.
- Cascade with origins, importance, inline styles and document order; shorthand
  expansion; inheritance; CSS-wide keywords; `var()` with fallbacks; media
  queries; a user-agent stylesheet covering the HTML defaults.

## Phase 3 — Layout — done (subset)

- Box tree with anonymous block wrapping.
- Block flow with percentage edges, `box-sizing`, min/max sizes, sibling margin
  collapsing; inline flow with greedy line breaking, `text-align`, `white-space`,
  inline-blocks and `<br>`; basic flexbox; absolute/fixed positioning; relative
  offsets; overflow clipping and scroll ranges; hit testing and paint order.
- Floats (left/right with per-line float bands and `clear`) and a CSS Grid subset
  (`px`/`%`/`fr`/`auto`/`repeat()`, gaps, row-major placement).
- **Not done:** full table layout (column widths, row spanning), `vertical-align`
  beyond keywords, named grid lines/areas, bidirectional text.

## Phase 4 — Rendering — done (software), GPU planned

- Display list, software rasterizer, bitmap-font text, borders, backgrounds,
  images (placeholder), clipping, scrolling.
- Dependency-free PNG encoder; deterministic screenshots; headless CLI.
- Compositor: layers, tiles, damage rectangles, clip stack, animation timeline.
- GPU abstraction with embedded WGSL shaders and a software reference device.
- Image decoding wired into `<img>`: PNG (all filters, palette/grey/RGBA, bit
  depths 1/2/4/8, Adam7 interlacing), GIF (LZW, transparency, interlacing, first
  frame of animations) and uncompressed 24/32-bit BMP. JPEG and WebP are detected
  but not decoded yet.
- Synthetic bold/italic and list markers in the rasterizer.
- Native desktop window: `winit 0.30` + `wgpu 30` behind the `window` feature,
  with engine-drawn chrome and a `--window-selfcheck` PNG mode.
- **Planned:** damage-based partial repaint, HiDPI-perfect composition, real font
  loading/shaping, WebP decoding.

## Phase 5 — Navigation — done

- Address-bar style URL normalisation, `<a href>` hit testing and following,
  back/forward/reload, session history, tabs.

## Phase 6 — JavaScript — done (subset)

- Lexer, parser, AST, interpreter, values, operators, scopes, closures, objects,
  arrays, prototypes, exceptions, mark-and-sweep GC, microtasks and promises.
- Regex literals with a backtracking engine, bit shifts (`<< >> >>>`),
  destructuring with defaults/rest and default/rest parameters.
- **Not done:** classes, generators, async/await, getters/setters in literals,
  object spread, `for`-head destructuring, `eval`.

## Phase 7 — DOM + JavaScript — done

- `document`, element wrappers with `getAttribute`/`setAttribute`/`appendChild`/
  `insertBefore`/`removeChild`, `textContent`, `innerHTML`, `children`, `value`,
  `id`, `className`, `tagName`, `querySelector(All)`, `getElementsByTagName`.
- `addEventListener`/`removeEventListener`/`dispatchEvent` with bubbling,
  `setTimeout`/`setInterval`/`requestAnimationFrame`, `localStorage`,
  `location` (read + navigation requests), `console`.
- Mutations are visible after recomputation (`browser-core` re-runs style, layout
  and paint); `location.href =` triggers a real navigation.

## Phase 8 — Compatibility — partial / planned

- Done: Flexbox (single line), media queries, cookies, cache, redirects, CORS/CSP/
  permissions/sandbox primitives, IPC channels.
- Partial: tables (block fallback), forms (elements render and expose `value`, but
  no submission/validation), `innerHTML` (fragment parsing, no `<template>`).
- Planned: `fetch()` from scripts, IndexedDB-like storage, grid layout, native
  GPU/windowing, process isolation, incremental rendering, Web Platform Tests and
  Test262 conformance reporting.

## Known gaps that affect correctness

1. No incremental style/layout invalidation: every mutation re-renders the page.
2. PNG-only image support: no decoder for network images yet.
3. The desktop window requires `--features window` (adds `wgpu`/`winit`); the
   default build renders to PNG buffers.
4. Scripts from external `<script src>` are not fetched or executed.
5. No process isolation, so untrusted web content should not be loaded with
   scripting enabled.

## Benchmarking

`tools/benchmark/run.sh` measures the full pipeline (parse → style → layout →
paint) on local fixtures and reports timings and node counts. Optimisation work
must be justified by those numbers; correctness comes first.
