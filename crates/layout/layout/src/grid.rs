//! CSS Grid subset.
//!
//! Supported:
//! - `grid-template-columns` / `grid-template-rows` track lists made of fixed
//!   lengths (`px`), percentages (resolved against the grid content width),
//!   `fr` units, `auto`, and `repeat(n, ...)`. Named grid lines such as
//!   `[main-start] 1fr [main-end]` are accepted and remembered.
//! - Named areas via `grid-template-areas` (quoted rows), including the
//!   implicit `<area>-start` / `<area>-end` named lines.
//! - Explicit placement via `grid-column` / `grid-row` (`1 / 3`, `span 2`,
//!   `<name>-start / <name>-end`, negative line numbers) and `grid-area`
//!   (`<name>` or `row-start / col-start / row-end / col-end`).
//! - Implicit tracks created by placements beyond the explicit grid, sized by
//!   `grid-auto-rows` / `grid-auto-columns` (default `auto`).
//! - Sparse row-major (or `grid-auto-flow: column`) auto placement that skips
//!   cells occupied by explicitly placed items.
//! - `gap`, `row-gap` and `column-gap`; per-track `auto` sizing from items.
//! - `minmax(min, max)`: a flexible (`Nfr`) or definite (length / percentage)
//!   `max` determines the track size. A definite `min` on a flexible `max`
//!   (`minmax(100px, 1fr)`) is honoured as a floor. Intrinsic maxima (and
//!   `fit-content()`, `min-content`, `max-content`) are modelled as `auto`.
//! - Item alignment: `justify-items` / `align-items` on the container and
//!   `justify-self` / `align-self` on items (`start`, `center`, `end`,
//!   `stretch`).
//! - Whole-track content distribution: `justify-content` / `align-content`
//!   (`start`, `end`, `center`, `space-between`, `space-around`,
//!   `space-evenly`, `stretch`) when the tracks do not fill the container.
//!   `stretch` grows `auto`-sized tracks; other values change gaps/offsets.
//! - `grid-auto-flow: dense` backfills holes left by explicitly placed items.
//! - `subgrid` for `grid-template-columns` / `grid-template-rows`: the axis
//!   inherits the explicit track lines (and gutter) of the nearest grid
//!   ancestor.
//! - `auto` tracks use the min-content and max-content contributions of their
//!   items; contributions from spanning items are distributed over the tracks
//!   they cross.
//!
//! Placement, track lists, gaps and alignment are all read from computed
//! style, so they work from stylesheets as well as inline `style` attributes.
//!
//! Limitations:
//! - Baseline alignment is not implemented. Intrinsic track keywords
//!   (`fit-content`, `min-content`, `max-content`) behave as `auto` for the
//!   track itself; only item contributions are measured. Malformed track lists
//!   fall back to a single `auto` column instead of failing.
//! - The style engine does not expose `grid` / `grid-template` shorthands (they
//!   are not registered properties), so the longhands must be declared:
//!   `grid-template-columns`, `grid-template-rows`, `grid-template-areas`,
//!   `grid-auto-flow`, `grid-auto-rows` / `grid-auto-columns`.
//! - `subgrid` inherits the parent's declared track list and resolves flexible
//!   tracks against the child's own content box. Subgrids spanning only part
//!   of the parent, and parent track sizing influenced by subgrid content, are
//!   not modelled. Implicit tracks of the parent are not inherited.

use std::collections::{HashMap, HashSet};

use browser_dom::Document;
use browser_style::interpret::is_intrinsic_sizing;
use browser_style::{AlignItems, BoxSizing, ComputedValue, Dim, Property, Side};

use crate::block::{layout_box, resolve, shrink_to_fit};
use crate::position::translate_subtree;
use crate::tree::{BoxType, LayoutTree};

/// Upper bound on generated tracks, so `repeat(999999, 1fr)` cannot allocate
/// without limit.
const MAX_TRACKS: usize = 256;

/// A definite floor carried by a flexible (`minmax(min, Nfr)`) track.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FrMin {
    Px(f32),
    Percent(f32),
}

impl FrMin {
    fn resolve(self, base: f32) -> f32 {
        match self {
            FrMin::Px(px) => px.max(0.0),
            FrMin::Percent(percent) => (base * percent / 100.0).max(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Track {
    Fixed(f32),
    Percent(f32),
    /// A flexible track without a definite minimum.
    Fr(f32),
    /// A flexible track whose `minmax` minimum must be honoured.
    FrMin(f32, FrMin),
    Auto,
}

/// Distribution of free space between/around a set of grid tracks, as selected
/// by `justify-content` (inline axis) or `align-content` (block axis).
#[derive(Debug, Clone, Copy, PartialEq)]
enum ContentAlign {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Stretch,
}

fn content_align(value: Option<&str>) -> ContentAlign {
    match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "start" | "flex-start" => ContentAlign::Start,
        "end" | "flex-end" => ContentAlign::End,
        "center" => ContentAlign::Center,
        "space-between" => ContentAlign::SpaceBetween,
        "space-around" => ContentAlign::SpaceAround,
        "space-evenly" => ContentAlign::SpaceEvenly,
        "stretch" | "normal" => ContentAlign::Stretch,
        // An unset `justify-content` reads back as `None`; grid's initial
        // `normal` behaves as `stretch` (growing `auto` tracks only).
        _ => ContentAlign::Stretch,
    }
}

/// Returns the leading offset and the extra gap for content distribution.
/// `stretch` is handled separately because it grows tracks, not gaps.
fn distribute(align: ContentAlign, free: f32, count: usize) -> (f32, f32) {
    if count == 0 {
        return (0.0, 0.0);
    }
    match align {
        ContentAlign::Start | ContentAlign::Stretch => (0.0, 0.0),
        ContentAlign::End => (free, 0.0),
        ContentAlign::Center => (free / 2.0, 0.0),
        ContentAlign::SpaceBetween if count > 1 => (0.0, free / (count - 1) as f32),
        ContentAlign::SpaceBetween => (0.0, 0.0),
        ContentAlign::SpaceAround => {
            let each = free / count as f32;
            (each / 2.0, each)
        }
        ContentAlign::SpaceEvenly => {
            let each = free / (count + 1) as f32;
            (each, each)
        }
    }
}

/// A parsed track list together with the line names declared around tracks.
#[derive(Debug, Clone, Default)]
struct TrackList {
    tracks: Vec<Track>,
    /// `line_names[i]` are the names on grid line `i` (0-based). There is one
    /// more entry than tracks (the line after the last track).
    line_names: Vec<Vec<String>>,
}

impl TrackList {
    fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    fn len(&self) -> usize {
        self.tracks.len()
    }
}

/// Named areas parsed from `grid-template-areas`. Rectangles are stored as
/// 1-based grid line numbers `(row_start, col_start, row_end, col_end)`.
#[derive(Debug, Default)]
struct GridAreas {
    rects: HashMap<String, (usize, usize, usize, usize)>,
    rows: usize,
    cols: usize,
}

/// A single `grid-column` / `grid-row` component.
#[derive(Debug, Clone, Default, PartialEq)]
enum LineValue {
    #[default]
    Auto,
    Number(i32),
    Ident(String),
    Span(usize),
    SpanIdent(String),
}

#[derive(Debug, Clone, Default)]
struct AxisPlacement {
    start: LineValue,
    end: LineValue,
}

#[derive(Debug, Clone, Default)]
struct RawPlacement {
    col: AxisPlacement,
    row: AxisPlacement,
    area: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedAxis {
    start: Option<usize>,
    span: usize,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedPlacement {
    col: ResolvedAxis,
    row: ResolvedAxis,
}

/// The cell (0-based track start and span) assigned to an item.
#[derive(Debug, Clone, Copy)]
struct Cell {
    col: usize,
    row: usize,
    col_span: usize,
    row_span: usize,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            col_span: 1,
            row_span: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum InlineAlign {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockAlign {
    Start,
    Center,
    End,
    Stretch,
}

/// Lays out a grid container's children and returns the content height.
pub fn layout_grid(tree: &mut LayoutTree, doc: &Document, index: usize) -> f32 {
    let style = tree.boxes[index].style.clone();
    let content = tree.boxes[index].content;
    let children: Vec<usize> = tree.boxes[index]
        .children
        .iter()
        .copied()
        .filter(|child| !crate::tree::is_ignorable_child(tree, *child))
        .collect();
    let available = content.width.max(0.0);
    let item_count = children.len();

    // --- Templates (including `subgrid` inheritance) -----------------------
    let col_is_subgrid = is_subgrid(style.grid_template_columns().as_deref());
    let row_is_subgrid = is_subgrid(style.grid_template_rows().as_deref());
    let (mut col_tracks, inherited_col_gap) = if col_is_subgrid {
        inherited_axis_tracks(tree, index, true)
            .map(|(tracks, gap)| (tracks, Some(gap)))
            .unwrap_or_else(|| {
                (
                    parse_track_list(style.grid_template_columns().as_deref()),
                    None,
                )
            })
    } else {
        (
            parse_track_list(style.grid_template_columns().as_deref()),
            None,
        )
    };
    let (mut row_tracks, inherited_row_gap) = if row_is_subgrid {
        inherited_axis_tracks(tree, index, false)
            .map(|(tracks, gap)| (tracks, Some(gap)))
            .unwrap_or_else(|| {
                (
                    parse_track_list(style.grid_template_rows().as_deref()),
                    None,
                )
            })
    } else {
        (
            parse_track_list(style.grid_template_rows().as_deref()),
            None,
        )
    };
    // A subgrid uses the parent's gutters on the inherited axis.
    let column_gap = inherited_col_gap.unwrap_or_else(|| style.column_gap().max(0.0));
    let row_gap = inherited_row_gap.unwrap_or_else(|| style.row_gap().max(0.0));

    if col_tracks.is_empty() {
        col_tracks.tracks.push(Track::Fr(1.0));
        ensure_line_names(&mut col_tracks, 1);
    }
    if row_tracks.is_empty() {
        ensure_line_names(&mut row_tracks, 0);
    }

    let areas = parse_grid_areas(style.grid_template_areas().as_deref());
    // A subgrid axis already carries its ancestor's line names; only inject
    // area lines on axes declared locally.
    if !col_is_subgrid {
        inject_area_axis(&mut col_tracks, &areas, true);
    }
    if !row_is_subgrid {
        inject_area_axis(&mut row_tracks, &areas, false);
    }

    let mut auto_cols = parse_track_list(style.grid_auto_columns().as_deref()).tracks;
    let mut auto_rows = parse_track_list(style.grid_auto_rows().as_deref()).tracks;
    if auto_cols.is_empty() {
        auto_cols.push(Track::Auto);
    }
    if auto_rows.is_empty() {
        auto_rows.push(Track::Auto);
    }
    let auto_flow = style.grid_auto_flow().unwrap_or_default();
    let auto_flow_column = auto_flow
        .split_whitespace()
        .any(|k| k.eq_ignore_ascii_case("column"));
    let dense = auto_flow
        .split_whitespace()
        .any(|k| k.eq_ignore_ascii_case("dense"));

    // --- Per-item placement declarations -----------------------------------
    let mut raw_placements: Vec<Option<RawPlacement>> = Vec::with_capacity(item_count);
    for &child in &children {
        if !crate::budget::try_step() {
            break;
        }
        let child_style = &tree.boxes[child].style;
        let area = child_style.grid_area().as_deref().map(parse_area_value);
        let col = child_style.grid_column().as_deref().map(parse_axis_value);
        let row = child_style.grid_row().as_deref().map(parse_axis_value);
        // `grid-column` / `grid-row` override the corresponding `grid-area`
        // components, so merge them into the area placement when present.
        let mut placement = area.unwrap_or_default();
        if let Some(col) = col {
            placement.col = col;
            placement.area = None;
        }
        if let Some(row) = row {
            placement.row = row;
            placement.area = None;
        }
        raw_placements.push(Some(placement));
    }
    raw_placements.resize(item_count, None);

    // Ensure the explicit grid covers at least the number of items when there
    // are no explicit rows (mirrors the original row-major behaviour).
    let min_rows = item_count.div_ceil(col_tracks.len().max(1)).max(1);
    ensure_row_track(&mut row_tracks, &auto_rows, min_rows.saturating_sub(1));

    let cells = place_items(
        &raw_placements,
        &areas,
        &mut col_tracks,
        &mut row_tracks,
        &auto_cols,
        &auto_rows,
        auto_flow_column,
        dense,
    );

    let column_count = col_tracks.len();
    let row_count = row_tracks.len();

    // --- Column sizing -----------------------------------------------------
    let mut intrinsic = vec![0.0_f32; item_count];
    let mut min_intrinsic = vec![0.0_f32; item_count];
    for (position, &child) in children.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let b = &tree.boxes[child];
        let width = match b.style.width() {
            Dim::Auto => shrink_to_fit(tree, child, available),
            dim => resolve(dim, available),
        }
        .max(0.0);
        intrinsic[position] = width + b.margins.horizontal();
        min_intrinsic[position] = min_content_width(tree, child) + b.margins.horizontal();
    }

    // Size `auto` tracks from their items' intrinsic contributions. Single-track
    // items set the base size; spanning items distribute their surplus over the
    // auto tracks they cross (weighted by the size already assigned to them).
    // The min-content contribution is kept as a floor for every such track.
    let mut auto_widths = vec![0.0_f32; column_count];
    let mut auto_mins = vec![0.0_f32; column_count];
    for (position, cell) in cells.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let span_start = cell.col;
        let span_end = (cell.col + cell.col_span).min(column_count);
        let auto_cols: Vec<usize> = (span_start..span_end)
            .filter(|&c| matches!(col_tracks.tracks[c], Track::Auto))
            .collect();
        if auto_cols.is_empty() {
            continue;
        }
        let per_min = min_intrinsic[position] / auto_cols.len() as f32;
        for &column in &auto_cols {
            auto_mins[column] = auto_mins[column].max(per_min);
        }
        if auto_cols.len() == 1 {
            let column = auto_cols[0];
            auto_widths[column] = auto_widths[column].max(intrinsic[position]);
        }
    }
    for (position, cell) in cells.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let span_start = cell.col;
        let span_end = (cell.col + cell.col_span).min(column_count);
        let auto_cols: Vec<usize> = (span_start..span_end)
            .filter(|&c| matches!(col_tracks.tracks[c], Track::Auto))
            .collect();
        if auto_cols.len() < 2 {
            continue;
        }
        let current: f32 = auto_cols.iter().map(|&c| auto_widths[c]).sum();
        let surplus = (intrinsic[position] - current).max(0.0);
        if surplus <= 0.0 {
            continue;
        }
        let weight: f32 = auto_cols.iter().map(|&c| auto_widths[c].max(1.0)).sum();
        for &column in &auto_cols {
            auto_widths[column] += surplus * auto_widths[column].max(1.0) / weight;
        }
    }
    for column in 0..column_count {
        auto_widths[column] = auto_widths[column].max(auto_mins[column]);
    }

    let gaps_width = column_gap * column_count.saturating_sub(1) as f32;
    let mut fixed_width = gaps_width;
    let mut fr_units = 0.0_f32;
    let mut fr_mins = vec![0.0_f32; column_count];
    for (column, track) in col_tracks.tracks.iter().enumerate() {
        match track {
            Track::Fixed(px) => fixed_width += px.max(0.0),
            Track::Percent(percent) => fixed_width += (available * percent / 100.0).max(0.0),
            Track::Auto => fixed_width += auto_widths[column],
            Track::Fr(units) => fr_units += units.max(0.0),
            Track::FrMin(units, min) => {
                fr_units += units.max(0.0);
                fr_mins[column] = min.resolve(available);
            }
        }
    }
    // Definite `minmax` minima are taken out of the space shared by `fr`
    // tracks; each flexible track then receives at least its minimum.
    let fr_min_sum: f32 = fr_mins.iter().sum();
    let fr_space = (available - fixed_width - fr_min_sum).max(0.0);
    let fr_unit = if fr_units > 0.0 {
        fr_space / fr_units
    } else {
        0.0
    };
    let mut widths: Vec<f32> = col_tracks
        .tracks
        .iter()
        .enumerate()
        .map(|(column, track)| match track {
            Track::Fixed(px) => px.max(0.0),
            Track::Percent(percent) => (available * percent / 100.0).max(0.0),
            Track::Auto => auto_widths[column],
            Track::Fr(units) => fr_unit * units.max(0.0),
            Track::FrMin(units, _) => fr_mins[column] + fr_unit * units.max(0.0),
        })
        .collect();

    // --- Whole-track content distribution (`justify-content`) --------------
    let used_inline = widths.iter().sum::<f32>() + gaps_width;
    let free_inline = (available - used_inline).max(0.0);
    let justify = content_align(style.justify_content_raw().as_deref());
    let (col_offset, col_gap_extra) = distribute(justify, free_inline, column_count);
    if justify == ContentAlign::Stretch && free_inline > 0.0 {
        let auto_count = col_tracks
            .tracks
            .iter()
            .filter(|track| **track == Track::Auto)
            .count();
        if auto_count > 0 {
            let extra = free_inline / auto_count as f32;
            for (column, track) in col_tracks.tracks.iter().enumerate() {
                if *track == Track::Auto {
                    widths[column] += extra;
                }
            }
        }
    }
    let effective_column_gap = column_gap + col_gap_extra;

    // --- Child layout (needed for row sizing) ------------------------------
    let justify_items = inline_align(style.justify_items().as_deref());
    for (position, &child) in children.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let cell = cells[position];
        let cell_width: f32 = (cell.col..(cell.col + cell.col_span).min(column_count))
            .map(|c| widths[c])
            .sum::<f32>()
            + effective_column_gap * cell.col_span.saturating_sub(1) as f32;
        let justify_self = tree.boxes[child].style.justify_self();
        let effective_inline = inline_align_inherit(justify_self.as_deref(), justify_items);
        // An intrinsic width (`fit-content`, `min-content`, `max-content`) is
        // stored as raw text and reads back as `Dim::Auto`, but unlike a real
        // `auto` it must not stretch to fill the grid cell.
        let width_is_intrinsic = matches!(
            tree.boxes[child].style.get(Property::Width),
            Some(ComputedValue::Str(text)) if is_intrinsic_sizing(text)
        );
        let child_width_auto = tree.boxes[child].style.width() == Dim::Auto && !width_is_intrinsic;
        let layout_width = if effective_inline == InlineAlign::Stretch && child_width_auto {
            cell_width
        } else {
            intrinsic[position]
        };
        let _ = layout_box(tree, doc, child, 0.0, 0.0, layout_width.max(1.0));
    }

    // --- Row sizing --------------------------------------------------------
    let definite_height = definite_content_height(tree, index);
    let mut auto_heights = vec![0.0_f32; row_count];
    for (position, &child) in children.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let cell = cells[position];
        let b = &tree.boxes[child];
        let height = b.rect.height + b.margins.vertical();
        let auto_span = row_tracks
            .tracks
            .iter()
            .take((cell.row + cell.row_span).min(row_count))
            .skip(cell.row)
            .filter(|track| matches!(track, Track::Auto))
            .count();
        if auto_span == 0 {
            continue;
        }
        let per = height / auto_span as f32;
        for row in cell.row..(cell.row + cell.row_span).min(row_count) {
            if matches!(row_tracks.tracks[row], Track::Auto) {
                auto_heights[row] = auto_heights[row].max(per);
            }
        }
    }

    let mut row_heights = vec![0.0_f32; row_count];
    let mut fixed_height = 0.0_f32;
    let mut fr_row_units = 0.0_f32;
    let mut fr_row_mins = vec![0.0_f32; row_count];
    let row_base = definite_height.unwrap_or(available);
    for (row, track) in row_tracks.tracks.iter().enumerate() {
        let base = match track {
            Track::Fixed(px) => px.max(0.0),
            Track::Percent(percent) => (row_base * percent / 100.0).max(0.0),
            Track::Auto | Track::Fr(_) => auto_heights[row],
            Track::FrMin(_, min) => min.resolve(row_base).max(auto_heights[row]),
        };
        row_heights[row] = base;
        match track {
            Track::Fr(units) => fr_row_units += units.max(0.0),
            Track::FrMin(units, min) => {
                fr_row_units += units.max(0.0);
                fr_row_mins[row] = min.resolve(row_base);
            }
            _ => fixed_height += base,
        }
    }
    if let Some(height) = definite_height
        && fr_row_units > 0.0
    {
        let gaps_height = row_gap * row_count.saturating_sub(1) as f32;
        let min_sum: f32 = fr_row_mins.iter().sum();
        let remaining = (height - fixed_height - gaps_height - min_sum).max(0.0);
        let unit = remaining / fr_row_units;
        for (row, track) in row_tracks.tracks.iter().enumerate() {
            match track {
                Track::Fr(factor) => {
                    row_heights[row] = row_heights[row].max(unit * factor.max(0.0));
                }
                Track::FrMin(units, _) => {
                    row_heights[row] = fr_row_mins[row] + unit * units.max(0.0);
                }
                _ => {}
            }
        }
    }

    // --- Whole-track content distribution (`align-content`) ----------------
    let gaps_height = row_gap * row_count.saturating_sub(1) as f32;
    let used_block = row_heights.iter().sum::<f32>() + gaps_height;
    let free_block = definite_height.map_or(0.0, |h| (h - used_block).max(0.0));
    let align = content_align(style.align_content().as_deref());
    let (row_offset, row_gap_extra) = distribute(align, free_block, row_count);
    if align == ContentAlign::Stretch && free_block > 0.0 {
        let auto_count = row_tracks
            .tracks
            .iter()
            .filter(|track| **track == Track::Auto)
            .count();
        if auto_count > 0 {
            let extra = free_block / auto_count as f32;
            for (row, track) in row_tracks.tracks.iter().enumerate() {
                if *track == Track::Auto {
                    row_heights[row] += extra;
                }
            }
        }
    }
    let effective_row_gap = row_gap + row_gap_extra;

    // --- Placement ---------------------------------------------------------
    let mut column_x = Vec::with_capacity(column_count);
    let mut x = content.x + col_offset;
    for width in &widths {
        column_x.push(x);
        x += width + effective_column_gap;
    }
    let mut row_y = Vec::with_capacity(row_count);
    let mut y = content.y + row_offset;
    for height in &row_heights {
        row_y.push(y);
        y += height + effective_row_gap;
    }

    let align_items = block_align_from_style(style.align_items());
    for (position, &child) in children.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let cell = cells[position];
        let (Some(cell_x), Some(cell_y)) = (column_x.get(cell.col), row_y.get(cell.row)) else {
            continue;
        };
        let cell_width: f32 = (cell.col..(cell.col + cell.col_span).min(column_count))
            .map(|c| widths[c])
            .sum::<f32>()
            + effective_column_gap * cell.col_span.saturating_sub(1) as f32;
        let cell_height: f32 = (cell.row..(cell.row + cell.row_span).min(row_count))
            .map(|r| row_heights[r])
            .sum::<f32>()
            + effective_row_gap * cell.row_span.saturating_sub(1) as f32;

        let inline = inline_align_inherit(
            tree.boxes[child].style.justify_self().as_deref(),
            justify_items,
        );
        let block =
            block_align_inherit(tree.boxes[child].style.align_self().as_deref(), align_items);

        let rect = tree.boxes[child].rect;
        let dx = match inline {
            InlineAlign::Start | InlineAlign::Stretch => 0.0,
            InlineAlign::Center => (cell_width - rect.width) / 2.0,
            InlineAlign::End => cell_width - rect.width,
        };
        let dy = match block {
            BlockAlign::Start | BlockAlign::Stretch => 0.0,
            BlockAlign::Center => (cell_height - rect.height) / 2.0,
            BlockAlign::End => cell_height - rect.height,
        };
        if block == BlockAlign::Stretch && cell_height > rect.height {
            tree.boxes[child].rect.height = cell_height;
        }
        translate_subtree(tree, child, cell_x + dx - rect.x, cell_y + dy - rect.y);
    }

    let total: f32 = row_heights.iter().sum::<f32>() + row_gap * row_count.saturating_sub(1) as f32;
    total.max(0.0)
}

fn definite_content_height(tree: &LayoutTree, index: usize) -> Option<f32> {
    let b = tree.boxes.get(index)?;
    let style = &b.style;
    let border_box = style.box_sizing() == BoxSizing::BorderBox;
    let vertical_extra = b.borders.vertical() + b.paddings.vertical();
    match style.height() {
        Dim::Auto => None,
        Dim::Px(px) => Some(if border_box {
            (px - vertical_extra).max(0.0)
        } else {
            px.max(0.0)
        }),
        Dim::Percent(percent) => {
            // The containing block height is not available here; the grid
            // content width is used as the percentage base (documented).
            Some((b.content.width * percent / 100.0).max(0.0))
        }
        Dim::Calc { percent, px } => Some((b.content.width * percent / 100.0 + px).max(0.0)),
    }
}

// --- Subgrid inheritance ----------------------------------------------------

fn is_subgrid(raw: Option<&str>) -> bool {
    raw.is_some_and(|value| {
        value
            .split_whitespace()
            .next()
            .is_some_and(|keyword| keyword.eq_ignore_ascii_case("subgrid"))
    })
}

/// Finds the nearest ancestor box that establishes a grid formatting context.
fn nearest_grid_ancestor(tree: &LayoutTree, index: usize) -> Option<usize> {
    let mut current = index;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let parent = tree
            .boxes
            .iter()
            .position(|box_ref| box_ref.children.contains(&current))?;
        if matches!(tree.boxes[parent].box_type, BoxType::Grid) {
            return Some(parent);
        }
        current = parent;
    }
    None
}

/// Resolves a `subgrid` axis to the explicit track lines (and gutter) of the
/// nearest grid ancestor. Nested subgrids recurse to the first non-subgrid
/// ancestor. Returns `None` when the ancestor has no explicit template for the
/// axis, in which case the caller falls back to a local template.
fn inherited_axis_tracks(
    tree: &LayoutTree,
    index: usize,
    columns: bool,
) -> Option<(TrackList, f32)> {
    let ancestor = nearest_grid_ancestor(tree, index)?;
    let style = &tree.boxes[ancestor].style;
    let raw = if columns {
        style.grid_template_columns()
    } else {
        style.grid_template_rows()
    };
    let gap = if columns {
        style.column_gap()
    } else {
        style.row_gap()
    }
    .max(0.0);
    if is_subgrid(raw.as_deref()) {
        return inherited_axis_tracks(tree, ancestor, columns);
    }
    let mut tracks = parse_track_list(raw.as_deref());
    if tracks.is_empty() {
        return None;
    }
    let areas = parse_grid_areas(style.grid_template_areas().as_deref());
    inject_area_axis(&mut tracks, &areas, columns);
    Some((tracks, gap))
}

/// Estimates a min-content width (the longest unbreakable word plus insets),
/// used as a floor for `auto`-sized tracks.
fn min_content_width(tree: &LayoutTree, index: usize) -> f32 {
    fn walk(tree: &LayoutTree, index: usize) -> f32 {
        let box_ref = &tree.boxes[index];
        let mut width = if box_ref.box_type == BoxType::Text {
            box_ref
                .text
                .split_whitespace()
                .map(|word| {
                    browser_text::Font::monospace().measure(word, box_ref.style.font_size())
                })
                .fold(0.0_f32, f32::max)
        } else if let Dim::Px(px) = box_ref.style.width() {
            px.max(0.0)
        } else {
            box_ref
                .children
                .iter()
                .map(|&child| walk(tree, child))
                .fold(0.0_f32, f32::max)
        };
        for side in [Side::Left, Side::Right] {
            width += box_ref.style.padding(side).resolve(0.0).unwrap_or(0.0);
            width += box_ref.style.border_width(side);
            width += box_ref.style.margin(side).resolve(0.0).unwrap_or(0.0);
        }
        width
    }
    walk(tree, index)
}

// --- Track parsing ----------------------------------------------------------

fn parse_track_list(raw: Option<&str>) -> TrackList {
    let Some(raw) = raw else {
        return TrackList::default();
    };
    let mut out = TrackList::default();
    append_tracks(raw, &mut out);
    out
}

fn ensure_line_names(list: &mut TrackList, line: usize) {
    while list.line_names.len() <= line {
        list.line_names.push(Vec::new());
    }
}

fn append_tracks(input: &str, out: &mut TrackList) {
    let mut pending_names: Vec<String> = Vec::new();
    for raw_token in split_top_level(input) {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }
        if token.starts_with('[') {
            let mut rest = token;
            while let Some(after) = rest.strip_prefix('[') {
                let Some((names, tail)) = after.split_once(']') else {
                    break;
                };
                for name in names.split_whitespace() {
                    let name = name.trim().to_ascii_lowercase();
                    if !name.is_empty() {
                        pending_names.push(name);
                    }
                }
                rest = tail.trim();
                if rest.is_empty() {
                    break;
                }
            }
            continue;
        }
        let line = out.tracks.len();
        ensure_line_names(out, line);
        out.line_names[line].append(&mut pending_names);

        let lower = token.to_ascii_lowercase();
        if let Some(inner) = lower
            .strip_prefix("repeat(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let Some((count_text, list)) = split_once_top_level(inner) else {
                continue;
            };
            let count = count_text
                .trim()
                .parse::<usize>()
                .ok()
                .unwrap_or(0)
                .min(MAX_TRACKS);
            let mut inner_list = TrackList::default();
            append_tracks(list, &mut inner_list);
            if inner_list.tracks.is_empty() {
                continue;
            }
            for _ in 0..count {
                if out.tracks.len() + inner_list.tracks.len() > MAX_TRACKS {
                    break;
                }
                let base = out.tracks.len();
                for (index, track) in inner_list.tracks.iter().enumerate() {
                    ensure_line_names(out, base + index);
                    if let Some(names) = inner_list.line_names.get(index) {
                        out.line_names[base + index].extend(names.iter().cloned());
                    }
                    out.tracks.push(*track);
                }
                ensure_line_names(out, out.tracks.len());
                if let Some(names) = inner_list.line_names.last() {
                    out.line_names[out.tracks.len()].extend(names.iter().cloned());
                }
            }
            continue;
        }
        if let Some(track) = parse_track(&lower) {
            out.tracks.push(track);
        }
    }
    ensure_line_names(out, out.tracks.len());
    out.line_names[out.tracks.len()].append(&mut pending_names);
}

fn split_top_level(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut brackets = 0_i32;
    let mut start = 0_usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            '[' => brackets += 1,
            ']' => brackets = (brackets - 1).max(0),
            c if c.is_whitespace() && depth == 0 && brackets == 0 => {
                if index > start {
                    out.push(&input[start..index]);
                }
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    if start < input.len() {
        out.push(&input[start..]);
    }
    out
}

fn split_once_top_level(input: &str) -> Option<(&str, &str)> {
    let mut depth = 0_i32;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            ',' if depth == 0 => return Some((&input[..index], &input[index + 1..])),
            _ => {}
        }
    }
    None
}

/// Parses a definite (non-intrinsic) track size: an `fr`, length or percentage
/// value. Shared by [`parse_track`] and [`parse_minmax`].
fn parse_definite_track(token: &str) -> Option<Track> {
    let token = token.trim();
    if let Some(prefix) = token.strip_suffix("fr") {
        let value = prefix.trim().parse::<f32>().ok()?;
        return value.is_finite().then_some(Track::Fr(value.max(0.0)));
    }
    if let Some(prefix) = token.strip_suffix('%') {
        let value = prefix.trim().parse::<f32>().ok()?;
        return value.is_finite().then_some(Track::Percent(value.max(0.0)));
    }
    if let Some(prefix) = token.strip_suffix("px") {
        let value = prefix.trim().parse::<f32>().ok()?;
        return value.is_finite().then_some(Track::Fixed(value.max(0.0)));
    }
    if token == "0" {
        return Some(Track::Fixed(0.0));
    }
    None
}

/// Parses `minmax(min, max)`, preferring a definite `max` (`Nfr`, a length or a
/// percentage) because it is the most faithful sizing the subset can express.
/// A flexible `max` keeps a definite, positive `min` as a floor (as in
/// `minmax(100px, 1fr)`). An intrinsic `max` falls back to a definite, positive
/// `min`, otherwise the track is sized from its items like `auto`.
fn parse_minmax(inner: &str) -> Option<Track> {
    let (min, max) = split_once_top_level(inner)?;
    let min = min.trim();
    let max = max.trim();
    // A flexible maximum: remember the definite minimum rather than discarding
    // it, so `minmax(100px, 1fr)` cannot shrink below 100px.
    if let Some(units) = parse_fr(max) {
        let floor = match parse_definite_track(min) {
            Some(Track::Fixed(px)) if px > 0.0 => Some(FrMin::Px(px)),
            Some(Track::Percent(percent)) if percent > 0.0 => Some(FrMin::Percent(percent)),
            _ => None,
        };
        return Some(match floor {
            Some(floor) => Track::FrMin(units, floor),
            None => Track::Fr(units),
        });
    }
    // A bare `0` is not a useful maximum (it would collapse the track).
    if let Some(track) = parse_definite_track(max)
        && !matches!(track, Track::Fixed(0.0))
    {
        return Some(track);
    }
    if let Some(track) = parse_definite_track(min)
        && !matches!(
            track,
            Track::Fixed(0.0) | Track::Percent(0.0) | Track::Fr(0.0)
        )
    {
        return Some(track);
    }
    // `min-content`, `max-content`, `auto` and `fit-content()` are all modelled
    // as `auto` by this subset.
    Some(Track::Auto)
}

fn parse_fr(token: &str) -> Option<f32> {
    let prefix = token.trim().strip_suffix("fr")?;
    let value = prefix.trim().parse::<f32>().ok()?;
    value.is_finite().then_some(value.max(0.0))
}

fn parse_track(token: &str) -> Option<Track> {
    let token = token.trim();
    if token.is_empty() || token == "none" {
        return None;
    }
    if let Some(track) = parse_definite_track(token) {
        return Some(track);
    }
    if let Some(inner) = token
        .strip_prefix("minmax(")
        .and_then(|value| value.strip_suffix(')'))
    {
        return parse_minmax(inner);
    }
    if matches!(
        token,
        "auto" | "min-content" | "max-content" | "fit-content"
    ) || token.starts_with("fit-content(")
    {
        return Some(Track::Auto);
    }
    None
}

// --- Named areas ------------------------------------------------------------

fn parse_grid_areas(raw: Option<&str>) -> GridAreas {
    let Some(raw) = raw else {
        return GridAreas::default();
    };
    let bytes = raw.as_bytes();
    let mut rows: Vec<Vec<&str>> = Vec::new();
    let mut i = 0_usize;
    while i < bytes.len() {
        let quote = bytes[i];
        if quote == b'"' || quote == b'\'' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let row: Vec<&str> = raw[start..i].split_whitespace().collect();
            if !row.is_empty() {
                rows.push(row);
            }
        }
        i += 1;
    }
    if rows.is_empty() {
        return GridAreas::default();
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut rects = HashMap::new();
    for (row, cells) in rows.iter().enumerate() {
        for (col, cell) in cells.iter().enumerate() {
            if *cell == "." || cell.eq_ignore_ascii_case("null") {
                continue;
            }
            let name = cell.to_ascii_lowercase();
            if rects.contains_key(&name) {
                continue;
            }
            let (end_row, end_col) = area_rect(&rows, row, col, cols);
            rects.insert(name, (row + 1, col + 1, end_row + 1, end_col + 1));
        }
    }
    GridAreas {
        rects,
        rows: rows.len(),
        cols,
    }
}

/// Finds the rectangular extent (exclusive end row/column) of an area cell,
/// as required by CSS. Ragged rows are treated as empty (`.`) beyond their end.
fn area_rect(rows: &[Vec<&str>], row: usize, col: usize, cols: usize) -> (usize, usize) {
    let name = rows[row][col];
    let mut end_col = col + 1;
    while end_col < cols && rows[row].get(end_col).copied() == Some(name) {
        end_col += 1;
    }
    let mut end_row = row + 1;
    'rows: while end_row < rows.len() {
        for c in col..end_col {
            if rows[end_row].get(c).copied() != Some(name) {
                break 'rows;
            }
        }
        end_row += 1;
    }
    let mut final_col = col + 1;
    'cols: while final_col < cols {
        for cells in rows.iter().take(end_row).skip(row) {
            if cells.get(final_col).copied() != Some(name) {
                break 'cols;
            }
        }
        final_col += 1;
    }
    (end_row, final_col)
}

/// Adds the implicit `<area>-start` / `<area>-end` line names of
/// `grid-template-areas` to one axis of a track list.
fn inject_area_axis(tracks: &mut TrackList, areas: &GridAreas, columns: bool) {
    for (name, &(row_start, col_start, row_end, col_end)) in &areas.rects {
        let (begin, finish) = if columns {
            (col_start.saturating_sub(1), col_end.saturating_sub(1))
        } else {
            (row_start.saturating_sub(1), row_end.saturating_sub(1))
        };
        ensure_line_names(tracks, finish);
        tracks.line_names[begin].push(format!("{name}-start"));
        tracks.line_names[finish].push(format!("{name}-end"));
    }
    let wanted = if columns { areas.cols } else { areas.rows };
    while tracks.len() < wanted && tracks.len() < MAX_TRACKS {
        tracks.tracks.push(Track::Auto);
        ensure_line_names(tracks, tracks.len());
    }
}

// --- Placement parsing ------------------------------------------------------

fn parse_line_value(value: &str) -> LineValue {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        return LineValue::Auto;
    }
    let mut words = value.split_whitespace();
    if words.next().is_some_and(|w| w.eq_ignore_ascii_case("span"))
        && let Some(rest) = words.next()
    {
        if let Ok(count) = rest.parse::<usize>() {
            return LineValue::Span(count.max(1));
        }
        return LineValue::SpanIdent(rest.to_ascii_lowercase());
    }
    if let Ok(number) = value.parse::<i32>() {
        return LineValue::Number(number);
    }
    LineValue::Ident(value.to_ascii_lowercase())
}

fn parse_axis_value(raw: &str) -> AxisPlacement {
    let mut parts = raw.split('/').map(str::trim);
    AxisPlacement {
        start: parts
            .next()
            .map(parse_line_value)
            .unwrap_or(LineValue::Auto),
        end: parts
            .next()
            .map(parse_line_value)
            .unwrap_or(LineValue::Auto),
    }
}

fn parse_area_value(raw: &str) -> RawPlacement {
    let parts: Vec<&str> = raw.split('/').map(str::trim).collect();
    let is_name = parts.len() == 1
        && !parts[0].is_empty()
        && !parts[0].eq_ignore_ascii_case("auto")
        && parts[0].parse::<i32>().is_err()
        && !parts[0].to_ascii_lowercase().starts_with("span");
    if is_name {
        return RawPlacement {
            area: Some(parts[0].to_ascii_lowercase()),
            ..RawPlacement::default()
        };
    }
    RawPlacement {
        col: AxisPlacement {
            start: parts
                .get(1)
                .map(|v| parse_line_value(v))
                .unwrap_or(LineValue::Auto),
            end: parts
                .get(3)
                .map(|v| parse_line_value(v))
                .unwrap_or(LineValue::Auto),
        },
        row: AxisPlacement {
            start: parts
                .first()
                .map(|v| parse_line_value(v))
                .unwrap_or(LineValue::Auto),
            end: parts
                .get(2)
                .map(|v| parse_line_value(v))
                .unwrap_or(LineValue::Auto),
        },
        area: None,
    }
}

fn line_to_index(number: i32, track_count: usize) -> usize {
    // Lines are numbered 1..=N+1; a negative number counts back from the end
    // line, so -1 is the (N+1)th line. Return the 0-based track index.
    let total_lines = (track_count + 1) as i32;
    let line = if number > 0 {
        number
    } else {
        total_lines + number + 1
    };
    (line - 1).clamp(0, track_count as i32) as usize
}

fn resolve_line_name(name: &str, list: &TrackList) -> Option<usize> {
    list.line_names
        .iter()
        .position(|names| names.iter().any(|candidate| candidate == name))
}

fn resolve_axis(value: &AxisPlacement, list: &TrackList) -> ResolvedAxis {
    let leading_span = match &value.start {
        LineValue::Span(count) => Some(*count),
        _ => None,
    };
    let mut start = match &value.start {
        LineValue::Number(number) => Some(line_to_index(*number, list.len())),
        LineValue::Ident(name) => resolve_line_name(name, list),
        LineValue::Auto | LineValue::Span(_) | LineValue::SpanIdent(_) => None,
    };
    let mut end = match &value.end {
        LineValue::Number(number) => Some(line_to_index(*number, list.len())),
        LineValue::Ident(name) => resolve_line_name(name, list),
        LineValue::Auto | LineValue::Span(_) | LineValue::SpanIdent(_) => None,
    };
    let trailing_span = match &value.end {
        LineValue::Span(count) => Some(*count),
        _ => None,
    };
    if let (Some(s), Some(e)) = (start, end)
        && e < s
    {
        std::mem::swap(&mut start, &mut end);
    }
    let span = if let Some(count) = leading_span.or(trailing_span) {
        count.max(1)
    } else if let (Some(s), Some(e)) = (start, end) {
        e.saturating_sub(s).max(1)
    } else {
        1
    };
    if start.is_none() {
        start = end.map(|e| e.saturating_sub(1));
    }
    ResolvedAxis { start, span }
}

fn resolve_area(name: &str, areas: &GridAreas) -> Option<ResolvedPlacement> {
    let &(row_start, col_start, row_end, col_end) = areas.rects.get(name)?;
    let col_begin = col_start.saturating_sub(1);
    let col_finish = col_end.saturating_sub(1);
    let row_begin = row_start.saturating_sub(1);
    let row_finish = row_end.saturating_sub(1);
    Some(ResolvedPlacement {
        col: ResolvedAxis {
            start: Some(col_begin),
            span: col_finish.saturating_sub(col_begin).max(1),
        },
        row: ResolvedAxis {
            start: Some(row_begin),
            span: row_finish.saturating_sub(row_begin).max(1),
        },
    })
}

// --- Placement engine -------------------------------------------------------

fn ensure_col_track(tracks: &mut TrackList, auto: &[Track], index: usize) {
    while tracks.len() <= index && tracks.len() < MAX_TRACKS {
        let track = auto
            .get(tracks.len() % auto.len().max(1))
            .copied()
            .unwrap_or(Track::Auto);
        tracks.tracks.push(track);
        ensure_line_names(tracks, tracks.len());
    }
}

fn ensure_row_track(tracks: &mut TrackList, auto: &[Track], index: usize) {
    while tracks.len() <= index && tracks.len() < MAX_TRACKS {
        let track = auto
            .get(tracks.len() % auto.len().max(1))
            .copied()
            .unwrap_or(Track::Auto);
        tracks.tracks.push(track);
        ensure_line_names(tracks, tracks.len());
    }
}

fn is_free(
    occupied: &[Vec<bool>],
    row: usize,
    col: usize,
    row_span: usize,
    col_span: usize,
) -> bool {
    for r in row..row + row_span {
        for c in col..col + col_span {
            if occupied
                .get(r)
                .and_then(|cells| cells.get(c))
                .copied()
                .unwrap_or(false)
            {
                return false;
            }
        }
    }
    true
}

fn occupy(
    occupied: &mut Vec<Vec<bool>>,
    cols: &mut TrackList,
    rows: &mut TrackList,
    auto_cols: &[Track],
    auto_rows: &[Track],
    cell: Cell,
) {
    ensure_col_track(cols, auto_cols, cell.col + cell.col_span - 1);
    ensure_row_track(rows, auto_rows, cell.row + cell.row_span - 1);
    while occupied.len() < rows.len() {
        occupied.push(vec![false; occupied.first().map_or(0, Vec::len)]);
    }
    for row in occupied.iter_mut() {
        row.resize(cols.len(), false);
    }
    for row in occupied
        .iter_mut()
        .take((cell.row + cell.row_span).min(rows.len()))
        .skip(cell.row)
    {
        for slot in row
            .iter_mut()
            .take((cell.col + cell.col_span).min(cols.len()))
            .skip(cell.col)
        {
            *slot = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn find_slot(
    occupied: &[Vec<bool>],
    want_col: Option<usize>,
    want_row: Option<usize>,
    col_span: usize,
    row_span: usize,
    cursor: (usize, usize),
    column_flow: bool,
    col_limit: usize,
    row_limit: usize,
) -> Option<(usize, usize)> {
    if let Some(col) = want_col {
        // A definite column may need new rows, regardless of flow direction.
        for row in cursor.0..MAX_TRACKS {
            if row + row_span > MAX_TRACKS {
                break;
            }
            if is_free(occupied, row, col, row_span, col_span) {
                return Some((row, col));
            }
        }
        for row in 0..cursor.0.min(MAX_TRACKS) {
            if is_free(occupied, row, col, row_span, col_span) {
                return Some((row, col));
            }
        }
        return None;
    }
    if let Some(row) = want_row {
        // A definite row may need new columns, regardless of flow direction.
        for col in cursor.1..MAX_TRACKS {
            if col + col_span > MAX_TRACKS {
                break;
            }
            if is_free(occupied, row, col, row_span, col_span) {
                return Some((row, col));
            }
        }
        for col in 0..cursor.1.min(MAX_TRACKS) {
            if is_free(occupied, row, col, row_span, col_span) {
                return Some((row, col));
            }
        }
        return None;
    }
    if column_flow {
        // Columns are the flow axis: new columns may be created, but rows are
        // bounded by the existing grid (new rows only come from row flow).
        for col in cursor.1..col_limit {
            let first_row = if col == cursor.1 { cursor.0 } else { 0 };
            for row in first_row..row_limit {
                if row + row_span > row_limit {
                    break;
                }
                if is_free(occupied, row, col, row_span, col_span) {
                    return Some((row, col));
                }
            }
        }
    } else {
        // Rows are the flow axis: new rows may be created, but columns are
        // bounded by the existing explicit grid.
        for row in cursor.0..row_limit {
            let first_col = if row == cursor.0 { cursor.1 } else { 0 };
            for col in first_col..col_limit {
                if col + col_span > col_limit {
                    break;
                }
                if is_free(occupied, row, col, row_span, col_span) {
                    return Some((row, col));
                }
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn place_items(
    placements: &[Option<RawPlacement>],
    areas: &GridAreas,
    cols: &mut TrackList,
    rows: &mut TrackList,
    auto_cols: &[Track],
    auto_rows: &[Track],
    column_flow: bool,
    dense: bool,
) -> Vec<Cell> {
    let count = placements.len();
    let mut cells = vec![Cell::default(); count];
    let mut placed = vec![false; count];
    let mut resolved: Vec<Option<ResolvedPlacement>> = vec![None; count];

    for (index, raw) in placements.iter().enumerate() {
        if !crate::budget::try_step() {
            break;
        }
        let Some(raw) = raw else { continue };
        resolved[index] = if let Some(name) = &raw.area {
            resolve_area(name, areas)
        } else {
            Some(ResolvedPlacement {
                col: resolve_axis(&raw.col, cols),
                row: resolve_axis(&raw.row, rows),
            })
        };
    }

    let mut occupied: Vec<Vec<bool>> = vec![vec![false; cols.len()]; rows.len()];

    // Definitely placed items occupy their cells first.
    for index in 0..count {
        if !crate::budget::try_step() {
            break;
        }
        let Some(placement) = resolved[index] else {
            continue;
        };
        let (Some(col), Some(row)) = (placement.col.start, placement.row.start) else {
            continue;
        };
        let cell = Cell {
            col: col.min(MAX_TRACKS.saturating_sub(1)),
            row: row.min(MAX_TRACKS.saturating_sub(1)),
            col_span: placement.col.span.max(1),
            row_span: placement.row.span.max(1),
        };
        occupy(&mut occupied, cols, rows, auto_cols, auto_rows, cell);
        cells[index] = cell;
        placed[index] = true;
    }

    // Auto (and semi-auto) items fill the remaining free cells.
    let mut cursor = (0_usize, 0_usize);
    for index in 0..count {
        if !crate::budget::try_step() {
            break;
        }
        if placed[index] {
            continue;
        }
        let (want_col, want_row, col_span, row_span) = match resolved[index] {
            Some(placement) => (
                placement.col.start,
                placement.row.start,
                placement.col.span.max(1),
                placement.row.span.max(1),
            ),
            None => (None, None, 1, 1),
        };
        // Sparse packing advances a single auto-placement cursor; dense
        // packing restarts the search from the beginning so items backfill
        // holes left earlier in the grid.
        let search_cursor = if dense { (0, 0) } else { cursor };
        let Some((row, col)) = find_slot(
            &occupied,
            want_col,
            want_row,
            col_span,
            row_span,
            search_cursor,
            column_flow,
            if column_flow { MAX_TRACKS } else { cols.len() },
            if column_flow { rows.len() } else { MAX_TRACKS },
        ) else {
            continue;
        };
        let cell = Cell {
            col,
            row,
            col_span,
            row_span,
        };
        occupy(&mut occupied, cols, rows, auto_cols, auto_rows, cell);
        cells[index] = cell;
        cursor = if column_flow {
            (row + row_span, col)
        } else {
            (row, col + col_span)
        };
    }
    cells
}

// --- Alignment helpers ------------------------------------------------------

fn inline_align(value: Option<&str>) -> InlineAlign {
    match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "start" | "left" | "flex-start" | "self-start" => InlineAlign::Start,
        "end" | "right" | "flex-end" | "self-end" => InlineAlign::End,
        "center" => InlineAlign::Center,
        _ => InlineAlign::Stretch,
    }
}

fn inline_align_inherit(value: Option<&str>, parent: InlineAlign) -> InlineAlign {
    match value {
        None => parent,
        Some(raw) if raw.trim().eq_ignore_ascii_case("auto") => parent,
        Some(raw) => inline_align(Some(raw)),
    }
}

fn inline_block_align(value: Option<&str>) -> BlockAlign {
    match value.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "start" | "flex-start" | "self-start" | "baseline" => BlockAlign::Start,
        "end" | "flex-end" | "self-end" => BlockAlign::End,
        "center" => BlockAlign::Center,
        _ => BlockAlign::Stretch,
    }
}

fn block_align_inherit(value: Option<&str>, parent: BlockAlign) -> BlockAlign {
    match value {
        None => parent,
        Some(raw) if raw.trim().eq_ignore_ascii_case("auto") => parent,
        Some(raw) => inline_block_align(Some(raw)),
    }
}

fn block_align_from_style(align: AlignItems) -> BlockAlign {
    match align {
        AlignItems::FlexStart | AlignItems::Baseline => BlockAlign::Start,
        AlignItems::FlexEnd => BlockAlign::End,
        AlignItems::Center => BlockAlign::Center,
        AlignItems::Stretch => BlockAlign::Stretch,
    }
}

#[cfg(test)]
mod tests {
    use browser_dom::Document;
    use browser_style::{Origin, StyleEngine, Viewport};

    use crate::test_util::layout_grid;
    use crate::{LayoutTree, Size, layout};

    use super::{
        GridAreas, TrackList, inline_align, line_to_index, parse_area_value, parse_axis_value,
        parse_grid_areas, parse_track_list, resolve_axis,
    };

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.05,
            "expected {expected}, got {actual}"
        );
    }

    /// Like `test_util::layout_grid`, but grid-specific declarations live in
    /// the inline `style` attribute (which the style engine computes into the
    /// same computed properties a stylesheet rule would set).
    fn layout_grid_inline(
        grid_css: &str,
        items: &[(&str, &str)],
    ) -> (Document, LayoutTree, Vec<browser_dom::NodeId>) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let grid = doc.create_element("div");
        doc.set_attr(grid, "id", "g");
        doc.set_attr(grid, "style", grid_css);
        doc.append_child(body, grid);
        let mut ids = vec![grid];
        for (id, style) in items {
            let child = doc.create_element("div");
            doc.set_attr(child, "id", id);
            doc.set_attr(child, "style", style);
            doc.append_child(grid, child);
            ids.push(child);
        }
        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css("body { margin: 0; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = layout(
            &doc,
            &styled,
            Size {
                width: 800.0,
                height: 600.0,
            },
        );
        (doc, tree, ids)
    }

    /// Like `layout_grid_inline`, but each item receives a text child so
    /// intrinsic (shrink-to-fit) widths are measurable.
    fn layout_grid_with_item_text(
        grid_css: &str,
        items: &[(&str, &str, &str)],
    ) -> (Document, LayoutTree, Vec<browser_dom::NodeId>) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let grid = doc.create_element("div");
        doc.set_attr(grid, "id", "g");
        doc.set_attr(grid, "style", grid_css);
        doc.append_child(body, grid);
        let mut ids = vec![grid];
        for (id, style, text) in items {
            let child = doc.create_element("div");
            doc.set_attr(child, "id", id);
            doc.set_attr(child, "style", style);
            doc.append_child(grid, child);
            let text_node = doc.create_text(text);
            doc.append_child(child, text_node);
            ids.push(child);
        }
        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css("body { margin: 0; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = layout(
            &doc,
            &styled,
            Size {
                width: 800.0,
                height: 600.0,
            },
        );
        (doc, tree, ids)
    }

    #[test]
    fn two_fixed_columns() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 400px; grid-template-columns: 100px 200px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.width, 100.0);
        approx(b.rect.x, 100.0);
        approx(b.rect.width, 200.0);
        let _ = doc;
    }

    #[test]
    fn fr_and_fixed_mix() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 400px; grid-template-columns: 100px 1fr; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.width, 100.0);
        approx(b.rect.x, 100.0);
        approx(b.rect.width, 300.0);
        let _ = doc;
    }

    #[test]
    fn percentage_columns() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 400px; grid-template-columns: 25% 75%; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.width, 100.0);
        approx(b.rect.x, 100.0);
        approx(b.rect.width, 300.0);
        let _ = doc;
    }

    #[test]
    fn repeat_three_fr_columns() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: repeat(3, 1fr); }",
            &[("div", "a", ""), ("div", "b", ""), ("div", "c", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        let c = tree.find_node(ids[4]).unwrap();
        approx(a.rect.width, 100.0);
        approx(b.rect.x, 100.0);
        approx(c.rect.x, 200.0);
        let _ = doc;
    }

    #[test]
    fn gaps_offset_columns_and_rows() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: 1fr 1fr; column-gap: 20px; row-gap: 10px; } #a, #b { height: 20px; }",
            &[
                ("div", "a", ""),
                ("div", "b", ""),
                ("div", "c", ""),
                ("div", "d", ""),
            ],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        let c = tree.find_node(ids[4]).unwrap();
        approx(a.rect.width, 140.0);
        approx(b.rect.x, 160.0);
        approx(c.rect.y, 30.0);
        let _ = doc;
    }

    #[test]
    fn auto_row_uses_tallest_child() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: 1fr 1fr; } #a { height: 50px; } #b { height: 80px; } #c { height: 10px; }",
            &[("div", "a", ""), ("div", "b", ""), ("div", "c", "")],
        );
        let c = tree.find_node(ids[4]).unwrap();
        approx(c.rect.y, 80.0);
        let _ = doc;
    }

    #[test]
    fn row_major_placement_positions() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: 100px 100px 100px; } #a, #b, #c, #d { height: 20px; }",
            &[
                ("div", "a", ""),
                ("div", "b", ""),
                ("div", "c", ""),
                ("div", "d", ""),
            ],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        let c = tree.find_node(ids[4]).unwrap();
        let d = tree.find_node(ids[5]).unwrap();
        approx(a.rect.x, 0.0);
        approx(a.rect.y, 0.0);
        approx(b.rect.x, 100.0);
        approx(b.rect.y, 0.0);
        approx(c.rect.x, 200.0);
        approx(c.rect.y, 0.0);
        approx(d.rect.x, 0.0);
        approx(d.rect.y, 20.0);
        let _ = doc;
    }

    #[test]
    fn grid_inside_constrained_container() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; width: 200px; } #g { display: grid; grid-template-columns: 1fr 1fr; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.width, 100.0);
        approx(b.rect.x, 100.0);
        let _ = doc;
    }

    #[test]
    fn auto_column_uses_content_width() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; font-size: 10px; } #g { display: grid; width: 400px; grid-template-columns: auto 100px; }",
            &[("div", "a", "hello"), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        assert!(a.rect.width > 0.0);
        approx(b.rect.x, a.rect.width);
        let _ = doc;
    }

    #[test]
    fn none_track_list_falls_back_to_one_column() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: none; } #a, #b { height: 10px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.width, 300.0);
        approx(b.rect.y, 10.0);
        let _ = doc;
    }

    #[test]
    fn malformed_repeat_does_not_panic() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: repeat(2, 1fr; } #a { height: 10px; }",
            &[("div", "a", "")],
        );
        let g = tree.find_node(ids[1]).unwrap();
        assert_eq!(g.rect.height, 10.0);
        let a = tree.find_node(ids[2]).unwrap();
        assert!(a.rect.width > 0.0);
        assert!(a.rect.right() <= 300.0 + 0.01);
        let _ = doc;
    }

    #[test]
    fn three_fixed_tracks_place_fourth_item_on_second_row() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 300px; grid-template-columns: 10px 10px 10px; } #a, #b, #c, #d { height: 5px; }",
            &[
                ("div", "a", ""),
                ("div", "b", ""),
                ("div", "c", ""),
                ("div", "d", ""),
            ],
        );
        let d = tree.find_node(ids[5]).unwrap();
        approx(d.rect.x, 0.0);
        approx(d.rect.y, 5.0);
        let _ = doc;
    }

    #[test]
    fn template_rows_fixed_heights_are_used() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 200px; grid-template-columns: 1fr; grid-template-rows: 30px 60px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let b = tree.find_node(ids[3]).unwrap();
        approx(b.rect.y, 30.0);
        let g = tree.find_node(ids[1]).unwrap();
        approx(g.content.height, 90.0);
        let _ = doc;
    }

    // --- New tests: named lines, areas and spans --------------------------

    #[test]
    fn placement_from_stylesheet_rule() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } \
             #g { display: grid; width: 300px; grid-template-columns: 100px 100px 100px; \
                  grid-template-rows: 20px 20px; } \
             #a { grid-column: 2; grid-row: 1; height: 5px; } \
             #b { grid-column: 3; grid-row: 2; height: 5px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.x, 100.0);
        approx(a.rect.y, 0.0);
        approx(b.rect.x, 200.0);
        approx(b.rect.y, 20.0);
        let _ = doc;
    }

    #[test]
    fn explicit_line_placement() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: repeat(3, 100px);",
            &[
                ("a", "grid-column: 1 / 3; height: 10px;"),
                ("b", "grid-column: 3 / 4; height: 10px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.x, 0.0);
        approx(a.rect.width, 200.0);
        approx(b.rect.x, 200.0);
        let _ = doc;
    }

    #[test]
    fn span_two_columns() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: repeat(3, 1fr);",
            &[
                ("a", "grid-column: span 2; height: 10px;"),
                ("b", "height: 10px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.width, 200.0);
        approx(b.rect.x, 200.0);
        approx(b.rect.width, 100.0);
        let _ = doc;
    }

    #[test]
    fn auto_placement_skips_explicit_items() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: repeat(2, 150px);",
            &[
                ("a", "grid-column: 2; height: 10px;"),
                ("b", "height: 10px;"),
                ("c", "height: 10px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        let c = tree.find_node(ids[3]).unwrap();
        approx(a.rect.x, 150.0);
        approx(b.rect.x, 0.0);
        approx(b.rect.y, 10.0);
        approx(c.rect.x, 150.0);
        approx(c.rect.y, 10.0);
        let _ = doc;
    }

    #[test]
    fn named_areas_place_items() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 1fr 1fr; grid-template-rows: 20px 20px; \
             grid-template-areas: \"head head\" \"side main\";",
            &[
                ("head", "grid-area: head; height: 5px;"),
                ("side", "grid-area: side; height: 5px;"),
                ("main", "grid-area: main; height: 5px;"),
            ],
        );
        let head = tree.find_node(ids[1]).unwrap();
        let side = tree.find_node(ids[2]).unwrap();
        let main = tree.find_node(ids[3]).unwrap();
        approx(head.rect.x, 0.0);
        approx(head.rect.y, 0.0);
        approx(head.rect.width, 300.0);
        approx(side.rect.x, 0.0);
        approx(side.rect.y, 20.0);
        approx(side.rect.width, 150.0);
        approx(main.rect.x, 150.0);
        approx(main.rect.y, 20.0);
        let _ = doc;
    }

    #[test]
    fn named_area_lines_resolve_to_spans() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 1fr 1fr; \
             grid-template-areas: \"head head\" \"foot foot\";",
            &[("a", "grid-column: head-start / head-end; height: 5px;")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        approx(a.rect.x, 0.0);
        approx(a.rect.width, 300.0);
        let _ = doc;
    }

    #[test]
    fn implicit_rows_use_grid_auto_rows() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 1fr; grid-auto-rows: 25px;",
            &[("a", "grid-row: 3; height: 5px;"), ("b", "height: 5px;")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.y, 50.0);
        // The auto-placement cursor has advanced past the explicit item, so
        // the following item lands in the next implicit (25px) row.
        approx(b.rect.y, 75.0);
        let _ = doc;
    }

    #[test]
    fn justify_self_aligns_item_within_cell() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 300px;",
            &[
                (
                    "a",
                    "grid-column: 1; width: 100px; justify-self: end; height: 5px;",
                ),
                ("b", "width: 100px; justify-self: center; height: 5px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.x, 200.0);
        approx(b.rect.x, 100.0);
        let _ = doc;
    }

    #[test]
    fn justify_items_aligns_every_item() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 300px; justify-items: center;",
            &[
                ("a", "width: 100px; height: 5px;"),
                ("b", "width: 100px; height: 5px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.x, 100.0);
        approx(b.rect.x, 100.0);
        let _ = doc;
    }

    #[test]
    fn align_self_aligns_item_vertically() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 300px; grid-template-rows: 40px 40px;",
            &[
                ("a", "grid-row: 1; height: 10px; align-self: end;"),
                ("b", "grid-row: 2; height: 10px; align-self: center;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.y, 30.0);
        approx(b.rect.y, 55.0);
        let _ = doc;
    }

    #[test]
    fn column_flow_places_down_then_across() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 150px 150px; grid-auto-flow: column;",
            &[
                ("a", "height: 10px;"),
                ("b", "height: 10px;"),
                ("c", "height: 10px;"),
                ("d", "height: 10px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        let c = tree.find_node(ids[3]).unwrap();
        let d = tree.find_node(ids[4]).unwrap();
        approx(a.rect.x, 0.0);
        approx(a.rect.y, 0.0);
        approx(b.rect.x, 0.0);
        approx(b.rect.y, 10.0);
        approx(c.rect.x, 150.0);
        approx(c.rect.y, 0.0);
        approx(d.rect.x, 150.0);
        approx(d.rect.y, 10.0);
        let _ = doc;
    }

    #[test]
    fn negative_line_places_from_the_end() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: repeat(3, 100px);",
            &[("a", "grid-column: -2 / -1; height: 5px;")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        approx(a.rect.x, 200.0);
        approx(a.rect.width, 100.0);
        let _ = doc;
    }

    #[test]
    fn parse_tracks_with_named_lines() {
        let list = parse_track_list(Some("[a] 1fr [b c] 100px [d]"));
        assert_eq!(list.len(), 2);
        assert_eq!(list.line_names[0], ["a"]);
        assert_eq!(list.line_names[1], ["b", "c"]);
        assert_eq!(list.line_names[2], ["d"]);
    }

    #[test]
    fn parse_negative_and_named_lines() {
        let list = parse_track_list(Some("[start] 1fr 1fr [end]"));
        assert_eq!(line_to_index(-1, 2), 2);
        assert_eq!(line_to_index(1, 2), 0);
        assert_eq!(super::resolve_line_name("start", &list), Some(0));
        assert_eq!(super::resolve_line_name("end", &list), Some(2));
    }

    #[test]
    fn parse_area_shorthand_and_span_values() {
        let area = parse_area_value("header");
        assert_eq!(area.area.as_deref(), Some("header"));
        let four = parse_area_value("1 / 2 / 3 / 4");
        assert!(four.area.is_none());
        let axis = parse_axis_value("2 / span 3");
        assert_eq!(axis.start, super::LineValue::Number(2));
        assert_eq!(axis.end, super::LineValue::Span(3));
    }

    #[test]
    fn resolve_axis_handles_span_and_named_lines() {
        let list = parse_track_list(Some("[a] 1fr 1fr 1fr [z]"));
        let span = resolve_axis(
            &parse_axis_value("span 2"),
            &TrackList {
                tracks: list.tracks.clone(),
                line_names: list.line_names.clone(),
            },
        );
        assert_eq!(span.span, 2);
        assert_eq!(span.start, None);
        let named = resolve_axis(&parse_axis_value("a / z"), &list);
        assert_eq!(named.start, Some(0));
        assert_eq!(named.span, 3);
    }

    #[test]
    fn parse_areas_builds_rectangles() {
        let areas: GridAreas = parse_grid_areas(Some("\"a a b\" \"a a b\" \"c c c\""));
        assert_eq!(areas.rects.get("a"), Some(&(1, 1, 3, 3)));
        assert_eq!(areas.rects.get("b"), Some(&(1, 3, 3, 4)));
        assert_eq!(areas.rects.get("c"), Some(&(3, 1, 4, 4)));
    }

    #[test]
    fn inline_align_defaults_to_stretch() {
        assert_eq!(inline_align(None), super::InlineAlign::Stretch);
        assert_eq!(inline_align(Some("center")), super::InlineAlign::Center);
        assert_eq!(inline_align(Some("end")), super::InlineAlign::End);
    }

    // --- New tests: minmax(), span and intrinsic widths --------------------

    #[test]
    fn parse_repeat_minmax_zero_fr_as_six_equal_fr_tracks() {
        let list = parse_track_list(Some("repeat(6, minmax(0, 1fr))"));
        assert_eq!(list.len(), 6);
        assert!(
            list.tracks
                .iter()
                .all(|track| *track == super::Track::Fr(1.0)),
            "expected six 1fr tracks, got {:?}",
            list.tracks
        );
    }

    #[test]
    fn repeated_minmax_fr_columns_are_equal_width() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 600px; grid-template-columns: repeat(6, minmax(0, 1fr));",
            &[
                ("a", "height: 5px;"),
                ("b", "height: 5px;"),
                ("c", "height: 5px;"),
                ("d", "height: 5px;"),
                ("e", "height: 5px;"),
                ("f", "height: 5px;"),
            ],
        );
        for (index, id) in ids.iter().enumerate().skip(1) {
            let box_ref = tree.find_node(*id).unwrap();
            approx(box_ref.rect.width, 100.0);
            approx(box_ref.rect.x, (index as f32 - 1.0) * 100.0);
        }
        let _ = doc;
    }

    #[test]
    fn grid_column_span_two_occupies_two_minmax_tracks() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 600px; grid-template-columns: repeat(6, minmax(0, 1fr));",
            &[
                ("a", "grid-column: span 2; height: 5px;"),
                ("b", "height: 5px;"),
                ("c", "height: 5px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        let c = tree.find_node(ids[3]).unwrap();
        approx(a.rect.x, 0.0);
        approx(a.rect.width, 200.0);
        approx(b.rect.x, 200.0);
        approx(b.rect.width, 100.0);
        approx(c.rect.x, 300.0);
        approx(c.rect.width, 100.0);
        let _ = doc;
    }

    #[test]
    fn fit_content_width_does_not_stretch() {
        let (doc, tree, ids) = layout_grid_with_item_text(
            "display: grid; width: 600px; grid-template-columns: repeat(2, minmax(0, 1fr));",
            &[
                ("a", "width: fit-content; height: 10px;", "short"),
                ("b", "height: 10px;", ""),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        assert!(
            a.rect.width > 0.0 && a.rect.width < 300.0,
            "fit-content item should shrink to its content, got width {}",
            a.rect.width
        );
        // The sibling has ordinary `auto` width and still stretches to its cell.
        approx(b.rect.x, 300.0);
        approx(b.rect.width, 300.0);
        let _ = doc;
    }

    // --- New tests: content distribution, dense, subgrid, fr minima ---------

    #[test]
    fn justify_content_center_distributes_track_set() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: 100px 100px; \
             justify-content: center;",
            &[("a", "height: 5px;"), ("b", "height: 5px;")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        // The 100px of free space is split into 50px offsets on both sides.
        approx(a.rect.x, 50.0);
        approx(b.rect.x, 150.0);
        let _ = doc;
    }

    #[test]
    fn align_content_center_offsets_rows() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; height: 100px; grid-template-columns: 300px; \
             grid-template-rows: 20px 20px; align-content: center;",
            &[("a", "height: 5px;"), ("b", "height: 5px;")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        approx(a.rect.y, 30.0);
        approx(b.rect.y, 50.0);
        let _ = doc;
    }

    #[test]
    fn dense_packing_backfills_holes() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; grid-template-columns: repeat(3, 100px); \
             grid-template-rows: 20px 20px; grid-auto-flow: row dense;",
            &[
                ("a", "grid-column: 2 / 4; grid-row: 1; height: 5px;"),
                ("b", "grid-column: span 2; height: 5px;"),
                ("c", "height: 5px;"),
            ],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        let c = tree.find_node(ids[3]).unwrap();
        approx(a.rect.x, 100.0);
        approx(b.rect.x, 0.0);
        approx(b.rect.y, 20.0);
        // Sparse packing would have put `c` at (200, 20); dense backfills the
        // hole left by `a` on the first row.
        approx(c.rect.x, 0.0);
        approx(c.rect.y, 0.0);
        let _ = doc;
    }

    #[test]
    fn minmax_fr_honours_definite_min() {
        let (doc, tree, ids) = layout_grid_inline(
            "display: grid; width: 300px; \
             grid-template-columns: minmax(100px, 1fr) 1fr;",
            &[("a", "height: 5px;"), ("b", "height: 5px;")],
        );
        let a = tree.find_node(ids[1]).unwrap();
        let b = tree.find_node(ids[2]).unwrap();
        // The 100px floor is reserved first, then the remaining 200px are
        // shared equally: 100 + 100 = 200 for the first track.
        approx(a.rect.width, 200.0);
        approx(b.rect.x, 200.0);
        approx(b.rect.width, 100.0);
        let _ = doc;
    }

    /// Builds a grid with a nested subgrid child and two items inside it.
    fn layout_subgrid(
        parent_css: &str,
        sub_css: &str,
        items: &[(&str, &str)],
    ) -> (Document, LayoutTree, Vec<browser_dom::NodeId>) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let grid = doc.create_element("div");
        doc.set_attr(grid, "id", "g");
        doc.set_attr(grid, "style", parent_css);
        doc.append_child(body, grid);
        let sub = doc.create_element("div");
        doc.set_attr(sub, "id", "sub");
        doc.set_attr(sub, "style", sub_css);
        doc.append_child(grid, sub);
        let mut ids = vec![grid, sub];
        for (id, style) in items {
            let child = doc.create_element("div");
            doc.set_attr(child, "id", id);
            doc.set_attr(child, "style", style);
            doc.append_child(sub, child);
            ids.push(child);
        }
        let mut engine = StyleEngine::new(Viewport {
            width: 800.0,
            height: 600.0,
        });
        engine.add_css("body { margin: 0; }", Origin::Author);
        let styled = engine.compute(&doc);
        let tree = layout(
            &doc,
            &styled,
            Size {
                width: 800.0,
                height: 600.0,
            },
        );
        (doc, tree, ids)
    }

    #[test]
    fn subgrid_inherits_parent_tracks() {
        let (doc, tree, ids) = layout_subgrid(
            "display: grid; width: 300px; grid-template-columns: 100px 50px 150px;",
            "display: grid; grid-template-columns: subgrid; grid-column: 1 / -1; height: 10px;",
            &[
                ("a", "grid-column: 1; height: 5px;"),
                ("b", "grid-column: 2; height: 5px;"),
            ],
        );
        let a = tree.find_node(ids[2]).unwrap();
        let b = tree.find_node(ids[3]).unwrap();
        approx(a.rect.x, 0.0);
        approx(a.rect.width, 100.0);
        approx(b.rect.x, 100.0);
        approx(b.rect.width, 50.0);
        let _ = doc;
    }
}
