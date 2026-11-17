//! CSS Grid subset.
//!
//! Supported:
//! - `grid-template-columns` / `grid-template-rows` track lists made of fixed
//!   lengths (`px`), percentages (resolved against the grid content width),
//!   `fr` units, `auto`, and `repeat(n, ...)`.
//! - `gap`, `row-gap` and `column-gap`.
//! - Row-major automatic placement of the children into the defined columns.
//! - Auto rows sized to the tallest child in the row.
//!
//! Not supported (documented limitations): explicit `grid-column` /
//! `grid-row` placement, named lines/areas, `grid-auto-flow: column`,
//! `minmax()` beyond treating it as `auto`, `justify-items`/`align-items`, and
//! grid item spanning. Malformed track lists fall back to a single `auto`
//! column instead of failing.

use browser_dom::Document;
use browser_style::{BoxSizing, Dim};

use crate::block::{layout_box, resolve, shrink_to_fit};
use crate::position::translate_subtree;
use crate::tree::LayoutTree;

/// Upper bound on generated tracks, so `repeat(999999, 1fr)` cannot allocate
/// without limit.
const MAX_TRACKS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Track {
    Fixed(f32),
    Percent(f32),
    Fr(f32),
    Auto,
}

/// Lays out a grid container's children and returns the content height.
pub fn layout_grid(tree: &mut LayoutTree, doc: &Document, index: usize) -> f32 {
    let style = tree.boxes[index].style.clone();
    let content = tree.boxes[index].content;
    let children = tree.boxes[index].children.clone();
    let column_gap = style.column_gap().max(0.0);
    let row_gap = style.row_gap().max(0.0);

    let mut columns = parse_track_list(style.grid_template_columns().as_deref());
    if columns.is_empty() {
        // `none` (or an unparseable track list) behaves like a single
        // stretched column.
        columns.push(Track::Fr(1.0));
    }
    let column_count = columns.len();

    let needed_rows = children.len().div_ceil(column_count).max(1);
    let mut rows = parse_track_list(style.grid_template_rows().as_deref());
    if rows.len() < needed_rows {
        rows.resize(needed_rows, Track::Auto);
    }
    let row_count = rows.len();

    let available = content.width.max(0.0);

    // --- Column sizing -----------------------------------------------------
    let mut auto_widths = vec![0.0_f32; column_count];
    for (position, child) in children.iter().enumerate() {
        let column = position % column_count;
        if !matches!(columns[column], Track::Auto) {
            continue;
        }
        let child_style = tree.boxes[*child].style.clone();
        let width = match child_style.width() {
            Dim::Auto => shrink_to_fit(tree, doc, *child, available),
            dim => resolve(dim, available),
        }
        .max(0.0);
        auto_widths[column] = auto_widths[column].max(width);
    }

    let gaps_width = column_gap * column_count.saturating_sub(1) as f32;
    let mut fixed_width = gaps_width;
    let mut fr_units = 0.0;
    for (column, track) in columns.iter().enumerate() {
        match track {
            Track::Fixed(px) => fixed_width += px.max(0.0),
            Track::Percent(percent) => fixed_width += (available * percent / 100.0).max(0.0),
            Track::Auto => fixed_width += auto_widths[column],
            Track::Fr(units) => fr_units += units.max(0.0),
        }
    }
    let fr_space = (available - fixed_width).max(0.0);
    let fr_unit = if fr_units > 0.0 {
        fr_space / fr_units
    } else {
        0.0
    };
    let widths: Vec<f32> = columns
        .iter()
        .enumerate()
        .map(|(column, track)| match track {
            Track::Fixed(px) => px.max(0.0),
            Track::Percent(percent) => (available * percent / 100.0).max(0.0),
            Track::Auto => auto_widths[column],
            Track::Fr(units) => fr_unit * units.max(0.0),
        })
        .collect();

    // --- Row sizing --------------------------------------------------------
    let definite_height = definite_content_height(tree, index);
    let mut row_content = vec![0.0_f32; row_count];
    for (position, child) in children.iter().enumerate() {
        let column = position % column_count;
        let row = position / column_count;
        let width = widths.get(column).copied().unwrap_or(0.0);
        let _ = layout_box(tree, doc, *child, 0.0, 0.0, width.max(1.0));
        if let Some(slot) = row_content.get_mut(row) {
            let b = &tree.boxes[*child];
            let height = b.rect.height + b.margins.vertical();
            *slot = slot.max(height);
        }
    }

    let mut row_heights = vec![0.0_f32; row_count];
    let mut fixed_height = 0.0_f32;
    let mut fr_row_units = 0.0_f32;
    for (row, track) in rows.iter().enumerate() {
        let base = match track {
            Track::Fixed(px) => px.max(0.0),
            Track::Percent(percent) => {
                let base = definite_height.unwrap_or(available);
                (base * percent / 100.0).max(0.0)
            }
            Track::Auto | Track::Fr(_) => row_content.get(row).copied().unwrap_or(0.0),
        };
        row_heights[row] = base;
        match track {
            Track::Fr(units) => fr_row_units += units.max(0.0),
            _ => fixed_height += base,
        }
    }
    if let Some(height) = definite_height
        && fr_row_units > 0.0
    {
        let gaps_height = row_gap * row_count.saturating_sub(1) as f32;
        let remaining = (height - fixed_height - gaps_height).max(0.0);
        let unit = remaining / fr_row_units;
        for (row, track) in rows.iter().enumerate() {
            if let Track::Fr(factor) = track {
                row_heights[row] = row_heights[row].max(unit * factor.max(0.0));
            }
        }
    }

    // --- Placement ---------------------------------------------------------
    let mut column_x = Vec::with_capacity(column_count);
    let mut x = content.x;
    for width in &widths {
        column_x.push(x);
        x += width + column_gap;
    }
    let mut row_y = Vec::with_capacity(row_count);
    let mut y = content.y;
    for height in &row_heights {
        row_y.push(y);
        y += height + row_gap;
    }

    for (position, child) in children.iter().enumerate() {
        let column = position % column_count;
        let row = position / column_count;
        let (Some(cell_x), Some(cell_y)) = (column_x.get(column), row_y.get(row)) else {
            continue;
        };
        let rect = tree.boxes[*child].rect;
        translate_subtree(tree, *child, cell_x - rect.x, cell_y - rect.y);
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
    }
}

fn parse_track_list(raw: Option<&str>) -> Vec<Track> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let mut tracks = Vec::new();
    parse_tracks(raw, &mut tracks);
    tracks
}

fn parse_tracks(input: &str, out: &mut Vec<Track>) {
    for token in split_top_level(input) {
        if out.len() >= MAX_TRACKS {
            break;
        }
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() || token == "none" {
            continue;
        }
        if let Some(inner) = token
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
            let mut inner_tracks = Vec::new();
            parse_tracks(list, &mut inner_tracks);
            if inner_tracks.is_empty() {
                continue;
            }
            for _ in 0..count {
                if out.len() + inner_tracks.len() > MAX_TRACKS {
                    break;
                }
                out.extend_from_slice(&inner_tracks);
            }
            continue;
        }
        if let Some(track) = parse_track(&token) {
            out.push(track);
        }
    }
}

fn split_top_level(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0_usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            c if c.is_whitespace() && depth == 0 => {
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

fn parse_track(token: &str) -> Option<Track> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
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
    if matches!(token, "auto" | "min-content" | "max-content")
        || token.starts_with("minmax(")
        || token.starts_with("fit-content(")
    {
        return Some(Track::Auto);
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::test_util::layout_grid;

    fn approx(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.05,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn two_fixed_columns() {
        let (doc, tree, ids) = layout_grid(
            "body { margin: 0; } #g { display: grid; width: 400px; grid-template-columns: 100px 200px; }",
            &[("div", "a", ""), ("div", "b", "")],
        );
        // ids: body, g, a, b
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
        // ids: body, g, a, b
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
        // Malformed input must not panic; the fallback keeps the item inside
        // the grid.
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
}
