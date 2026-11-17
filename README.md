# Mag — a browser engine written from scratch in Rust

Mag is an independent browser engine. It does **not** embed Chromium, WebKit, Gecko,
Servo, V8, JavaScriptCore, SpiderMonkey or Boa. Every subsystem in this workspace —
URL parsing, HTTP, HTML tokenizer and tree builder, DOM, CSS, style engine, layout,
paint, compositor, JavaScript runtime and the web API surface — is implemented here.

## Quick start

```sh
cargo run -p browser-headless -- --html tests/fixtures/page.html --out out/page.png
cargo run -p browser-cli -- tree tests/fixtures/page.html
cargo test --workspace
```

## Layout

| Path | Purpose |
| --- | --- |
| `apps/browser` | Windowed browser shell |
| `apps/browser-cli` | Developer CLI (fetch, tree, style, layout, render) |
| `apps/browser-headless` | Deterministic headless renderer producing PNG |
| `crates/network` | URL, HTTP/1.1 (+TLS), resource loader |
| `crates/html` | DOM, HTML tokenizer/tree builder, selectors |
| `crates/css` | CSS parser, cascade and computed style |
| `crates/layout` | Text metrics and layout engine (block, inline, flex) |
| `crates/graphics` | Display list, compositor, software + GPU backends |
| `crates/javascript` | JavaScript lexer, parser, interpreter |
| `crates/web` | Event loop, web APIs, navigation |
| `crates/platform` | Browser core, security, IPC, OS integration |

See `docs/architecture.md` for the dependency graph and the contracts between crates,
and `docs/roadmap.md` for what is implemented versus planned.
