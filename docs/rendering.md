# Rendering pipeline

The render path is: **DOM → computed style → layout tree → display list → raster**.

Each stage has its own crate, tests and public types, and each one can be observed
with the CLI (`mag-cli tree|styles|layout|commands|render`).

## 1. Style (`browser-style`)

`StyleEngine` owns a user-agent stylesheet (the browser defaults), plus user and
author sheets, and computes a `StyledTree` keyed by `NodeId`.

- Selector matching is right-to-left via `browser-selectors`; specificity,
  origin and `!important` are combined into a single sortable weight
  (`cascade::Candidate::weight`).
- Shorthands (`margin`, `padding`, `border`, `background`, `overflow`, `flex`,
  `gap`, `inset`, …) are expanded before the cascade, so the cascade only ever
  sees longhands.
- `var()` is substituted at computed-value time, with cycle protection; a missing
  custom property without a fallback invalidates the declaration (as the spec
  requires).
- `calc()` is evaluated at computed-value time for lengths and percentages: nested
  expressions, `+ - * /`, absolute units and `em`/`rem`/viewport units all resolve
  against the font size / containing block.
- HTML presentational hints (`<img width>`/`<img height>`) are injected as
  low-priority UA declarations before author CSS, so author rules win.
- `font-size` is resolved before every other property, because `em` values depend
  on the element's own computed font size.
- Inheritance is applied last for properties that were not declared, using
  `Property::is_inherited`.
- Media queries are evaluated by the engine (`min-width`, `max-width`, `height`,
  `orientation`, `prefers-color-scheme`, media types), not by the CSS parser.

## 2. Layout (`browser-layout`)

`TreeBuilder` turns the DOM into a box tree: block boxes, inline boxes,
inline-blocks, text boxes, list items, tables (with automatic column layout), flex
containers, images and anonymous blocks that wrap inline runs inside block
containers.

`layout_box` is the core algorithm:

1. Resolve margins, borders and paddings (percentages against the containing block
   width, `box-sizing: border-box` subtracts them from the specified size).
2. Resolve `width` (`auto` fills the containing block, percentages are relative).
   `min-width`/`max-width` clamp the result.
3. Lay out children:
   - block flow: stack vertically, collapsing adjacent sibling margins
     (`max` of positives, sum with negatives);
   - inline flow: greedy line breaking with proportional Inter font metrics, producing
     `LineBox`es with `TextFragment`s; `text-align` shifts each line;
   - flex: `row`/`column` (and `-reverse`) with gaps, `justify-content`,
     `align-items` and `flex-grow`; `flex-wrap: wrap|wrap-reverse` packs items into
     multiple greedily formed lines with per-line justification and alignment
     (`align-content`, `order` and baseline alignment are not implemented);
   - tables: automatic layout measures max-content cell widths, takes the
     per-column maximum and stretches/shrinks columns to the container
     (`colspan`/`rowspan` are not supported).
4. Resolve `height` (`auto` uses the content height), apply `min-height`/
   `max-height`, compute `scroll_height`, and set a clip rectangle for
   `overflow: hidden|scroll|auto`.
5. Position out-of-flow children (`absolute`/`fixed`) from `top/right/bottom/left`.
6. Apply `position: relative` offsets to the whole subtree.

Floats with `width: auto` are measured with `shrink_to_fit` plus their margins.
`<input>` elements are replaced boxes whose label is synthesized from the
`value`/`placeholder` attribute (except for hidden/image/checkbox/radio types) as
a text child, so button and field text paints. Grid containers use the subset in
`grid.rs`: `px`/`%`/`fr`/`auto`/`repeat()` tracks with gaps and row-major
placement; explicit placement, named lines/areas and item spanning are not
supported.

The result is a flat `Vec<LayoutBox>` plus the root index. `hit_test` returns the
deepest box containing a point; `paint_order` returns descendants before parents,
sorted by `z-index`.

## 3. Paint (`browser-paint`)

`build` walks the layout tree in paint order and emits a `DisplayList`:

- `FillRect` for element backgrounds,
- `DrawBorder` for borders (solid, dashed and dotted; double/groove render solid),
- `DrawText` for each `TextFragment` (position, baseline, size, colour, underline /
  line-through, synthetic bold/italic),
- `DrawImage` for image boxes (decoded PNG/GIF/BMP when available, otherwise a
  deterministic checkerboard placeholder),
- `PushClip`/`PopClip` around scroll containers,
- child offsets include the container's scroll offset.

Three newer commands — `DrawRoundedRect` (anti-aliased rounded corners with an
optional border ring), `DrawShadow` (soft, offset shadow approximated with
concentric rounded rectangles) and `DrawLinearGradient` (stop-based gradient at an
angle) — are defined in `DisplayCommand` and executed by the rasterizer. They are
**not yet emitted** by `build`/`paint_box`, which still produces only the commands
above; the CSS side (`border-radius`, `box-shadow`, gradients) is work in progress
(see `docs/roadmap.md`).

The software rasterizer executes the commands into an RGBA `ImageBuffer`:

- source-over alpha blending, per-pixel clip testing,
- glyphs rasterised from the embedded Inter outlines with 4x4 anti-aliased coverage,
- dashed/dotted borders by segmenting the edge,
- nearest-neighbour image scaling.

`ImageBuffer::to_png` is a dependency-free PNG encoder (stored deflate blocks,
correct CRC32/Adler32), which is what makes screenshot tests exact and portable.

## 4. Compositor (`browser-compositor`)

`LayerTree::build` creates one layer for the root and one per scroll container,
each with a `TileGrid` (256 px tiles, dirty flags). `take_damage` produces the
damage rectangles that an incremental renderer would repaint. `ClipStack` and
`AnimationTimeline` provide the clip intersection and animation clocks used by the
scroll and transition paths. Damage-driven partial repaint is not wired into the
rasterizer yet; both sides exist and are tested independently.

## 5. GPU (`browser-gpu`)

`Device` is the boundary: `render(display_list, viewport) -> ImageBuffer`,
`create_texture`, `upload_texture`. `SoftwareDevice` implements it with the
rasterizer; `NativeDevice`/`NativePresenter` (feature `wgpu-backend`) add a real
`wgpu` surface with a single fullscreen textured-quad pipeline. Composition is done
on the CPU (deterministic), uploaded as one sRGB texture and presented with
`PresentMode::Fifo`, so the window and `--window-selfcheck` PNG are pixel-identical.

### Desktop window (`apps/browser`)

`--features window` opens a `winit` 0.30 window with chrome drawn by the engine's
own primitives and the embedded Inter font:

```
tab strip (36 px)  →  tabs + "+"
toolbar   (44 px)  →  back / forward / reload chips, address field, caret
page area          →  display list clipped inside a 1 px frame
status pill        →  Pronto / Carregando… / Erro
```

The chrome geometry lives in `chrome/layout.rs` (pure functions, tested), the
composition in `chrome/render.rs`, and input handling in `window_app.rs`. Chrome
shapes are supersampled 2x and reduced for smooth edges, while icons are
anti-aliased polygons and all text is drawn with the same `browser_text` glyph
cache as page text. The window is composed at the device pixel ratio, so HiDPI
displays get native resolution instead of nearest-neighbour upscaling. A live
window was smoke-tested on macOS; CI validates the same composition through
`--window-selfcheck <out.png>`.

## Determinism and testing

- No system fonts, no GPU, no locale-dependent formatting in the render path.
- All layout maths uses `f32` with explicit rounding at pixel boundaries.
- Golden tests assert specific pixels and full-image coverage fractions.
- `apps/browser-headless --json` reports node/command counts for regression tests.
