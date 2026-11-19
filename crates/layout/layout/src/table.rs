//! Table layout: rows and cells side by side with automatic column widths.
//!
//! This is an auto-table subset: cells are measured with their max-content width,
//! column widths are the maximum measurement per column and are then stretched or
//! shrunk to the container width. `colspan`/`rowspan` and fixed table layout are
//! not implemented (documented in `docs/roadmap.md`).

use browser_dom::Document;
use browser_style::{ComputedValue, Dim, Property, Side};

use crate::block::{layout_box, resolve, shrink_to_fit};
use crate::tree::{BoxType, LayoutTree};

/// Lays out a table container and returns its content height.
pub fn layout_table(tree: &mut LayoutTree, doc: &Document, index: usize) -> f32 {
    let content = tree.boxes[index].content;
    let mut rows = Vec::new();
    collect_rows(tree, index, &mut rows);
    if rows.is_empty() {
        return 0.0;
    }

    let columns = rows
        .iter()
        .map(|row| {
            tree.boxes[*row]
                .children
                .iter()
                .filter(|child| tree.boxes[**child].box_type == BoxType::TableCell)
                .count()
        })
        .max()
        .unwrap_or(0)
        .max(1);

    // 1. Natural (max-content) width per column.
    let mut natural = vec![0.0_f32; columns];
    for row in &rows {
        if !crate::budget::try_step() {
            break;
        }
        let cells = cells_of(tree, *row);
        for (column, cell) in cells.iter().enumerate() {
            let measured = shrink_to_fit(tree, *cell, content.width)
                + tree.boxes[*cell].borders.horizontal()
                + tree.boxes[*cell].paddings.horizontal();
            if let Some(slot) = natural.get_mut(column) {
                *slot = slot.max(measured);
            }
        }
    }

    // 2. Explicit cell widths (from CSS or HTML `width` attributes) take
    // precedence. Percentages resolve against the table's content width.
    let width = content.width.max(1.0);
    let mut specified: Vec<Option<f32>> = vec![None; columns];
    for row in &rows {
        if !crate::budget::try_step() {
            break;
        }
        for (column, cell) in cells_of(tree, *row).iter().enumerate() {
            if let Some(px) = tree.boxes[*cell].style.width().resolve(width) {
                let slot = &mut specified[column];
                if slot.is_none_or(|current| px > current) {
                    *slot = Some(px);
                }
            }
        }
    }

    // 3. Fit the column widths into the available content width. Columns with
    // an explicit width keep it and the leftover space goes to the auto
    // columns (e.g. the `nowrap` middle cell), instead of being shared equally
    // which lets the search box stretch across the whole table. When nothing
    // is explicitly sized, keep the original max-content distribution.
    let widths: Vec<f32> = if specified.iter().all(Option::is_none) {
        let total: f32 = natural.iter().sum();
        if total <= 0.0 {
            vec![width / columns as f32; columns]
        } else if total < width {
            let extra = (width - total) / columns as f32;
            natural.iter().map(|n| n + extra).collect()
        } else {
            natural
                .iter()
                .map(|n| (n * width / total).max(1.0))
                .collect()
        }
    } else {
        let fixed: f32 = specified.iter().flatten().sum();
        let auto_indices: Vec<usize> = (0..columns)
            .filter(|&index| specified[index].is_none())
            .collect();
        if auto_indices.is_empty() {
            // Every column is specified: honour the widths, shrinking
            // proportionally only when they overflow the table.
            if fixed <= width {
                specified.iter().map(|slot| slot.unwrap()).collect()
            } else {
                specified
                    .iter()
                    .map(|slot| (slot.unwrap() * width / fixed).max(1.0))
                    .collect()
            }
        } else {
            let remaining = (width - fixed).max(0.0);
            let auto_natural: f32 = auto_indices.iter().map(|&i| natural[i]).sum();
            let mut result = vec![0.0_f32; columns];
            for (index, slot) in result.iter_mut().enumerate() {
                if let Some(px) = specified[index] {
                    *slot = px;
                } else if auto_natural > 0.0 {
                    *slot = (remaining * natural[index] / auto_natural).max(1.0);
                } else {
                    *slot = (remaining / auto_indices.len() as f32).max(1.0);
                }
            }
            result
        }
    };

    // 4. Lay out each row, then place its cells side by side.
    let mut cursor_y = content.y;
    for row in &rows {
        if !crate::budget::try_step() {
            break;
        }
        let cells = cells_of(tree, *row);
        let mut row_bottom = cursor_y;
        let mut x = content.x;
        let mut tallest = 0.0_f32;
        for (column, cell) in cells.iter().enumerate() {
            let cell_width = widths.get(column).copied().unwrap_or(width);
            let margins = {
                let style = tree.boxes[*cell].style.clone();
                (
                    resolve(style.margin(Side::Left), cell_width),
                    resolve(style.margin(Side::Top), cell_width),
                )
            };
            let bottom = layout_table_cell(
                tree,
                doc,
                *cell,
                x + margins.0,
                cursor_y + margins.1,
                cell_width,
            );
            row_bottom = row_bottom.max(bottom);
            tallest = tallest.max(bottom - cursor_y - margins.1);
            x += cell_width;
            let _ = row_bottom;
        }
        let row_height = tallest.max(tree.boxes[*row].style.line_height_px());
        {
            let row_box = &mut tree.boxes[*row];
            row_box.rect =
                crate::box_model::Rect::new(content.x, cursor_y, content.width, row_height);
            row_box.content = row_box.rect;
        }
        cursor_y += row_height;
    }

    (cursor_y - content.y).max(0.0)
}

/// Collects table rows, descending through section wrappers (`thead`/`tbody`/
/// `tfoot`, which the HTML parser inserts and the UA sheet renders as blocks).
fn collect_rows(tree: &LayoutTree, index: usize, out: &mut Vec<usize>) {
    for child in &tree.boxes[index].children {
        match tree.boxes[*child].box_type {
            BoxType::TableRow => out.push(*child),
            BoxType::Text => {}
            _ => collect_rows(tree, *child, out),
        }
    }
}

fn cells_of(tree: &LayoutTree, row: usize) -> Vec<usize> {
    tree.boxes[row]
        .children
        .iter()
        .copied()
        .filter(|child| tree.boxes[*child].box_type == BoxType::TableCell)
        .collect()
}

/// Lays out a cell at a column width that has already been resolved.
///
/// A cell's own `width` (for example `width="25%"`) was used to size its
/// column above. Passing that same box as the containing block again would
/// apply the percentage a second time, so the cell is laid out with
/// `width: auto` and its original value restored afterwards.
fn layout_table_cell(
    tree: &mut LayoutTree,
    doc: &Document,
    cell: usize,
    x: f32,
    y: f32,
    cell_width: f32,
) -> f32 {
    let original = tree.boxes[cell]
        .style
        .get(Property::Width)
        .cloned()
        .unwrap_or(ComputedValue::Dim(Dim::Auto));
    tree.boxes[cell]
        .style
        .set(Property::Width, ComputedValue::Dim(Dim::Auto));
    let bottom = layout_box(tree, doc, cell, x, y, cell_width);
    tree.boxes[cell].style.set(Property::Width, original);
    bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Size;
    use crate::box_model::Rect;
    use browser_style::Origin;
    use browser_style::{StyleEngine, Viewport};

    fn table_page() -> (LayoutTree, browser_dom::NodeId, Vec<browser_dom::NodeId>) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let table = doc.create_element("table");
        doc.append_child(body, table);
        let mut cells = Vec::new();
        for row_content in [["a", "bb", "ccc"], ["dddd", "e", "f"]] {
            let row = doc.create_element("tr");
            doc.append_child(table, row);
            for text in row_content {
                let cell = doc.create_element("td");
                doc.append_child(row, cell);
                doc.append_text(cell, text);
                cells.push(cell);
            }
        }
        let mut engine = StyleEngine::new(Viewport {
            width: 600.0,
            height: 400.0,
        });
        engine.add_css(
            "body { margin: 0; } table { width: 600px; } td { padding: 0; border: 0; }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            Size {
                width: 600.0,
                height: 400.0,
            },
        );
        (tree, table, cells)
    }

    #[test]
    fn cells_in_a_row_are_side_by_side() {
        let (tree, _table, cells) = table_page();
        let a = tree.find_node(cells[0]).unwrap();
        let b = tree.find_node(cells[1]).unwrap();
        let c = tree.find_node(cells[2]).unwrap();
        assert_eq!(a.rect.y, b.rect.y);
        assert!(b.rect.x >= a.rect.right());
        assert!(c.rect.x >= b.rect.right());
        let total = a.rect.width + b.rect.width + c.rect.width;
        assert!((total - 600.0).abs() < 1.0, "columns must fill the table");
        assert!(a.rect.width > 50.0);
    }

    #[test]
    fn rows_stack_vertically() {
        let (tree, _table, cells) = table_page();
        let first = tree.find_node(cells[0]).unwrap();
        let second = tree.find_node(cells[3]).unwrap();
        assert!(second.rect.y > first.rect.y);
    }

    #[test]
    fn table_height_covers_all_rows() {
        let (tree, table, _cells) = table_page();
        let table_box = tree.find_node(table).unwrap();
        assert!(table_box.content.height > 30.0);
    }

    /// Builds the shape of Google's search form: two 25% outer columns and an
    /// auto-width, `nowrap` middle cell carrying the search box.
    fn search_table_page() -> (LayoutTree, Vec<browser_dom::NodeId>) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let table = doc.create_element("table");
        doc.set_attr(table, "cellpadding", "0");
        doc.set_attr(table, "cellspacing", "0");
        doc.append_child(body, table);
        let row = doc.create_element("tr");
        doc.append_child(table, row);

        let mut cells = Vec::new();
        let columns = [
            (Some("25%"), None, false, "\u{a0}"),
            (None, Some("center"), true, "search box"),
            (Some("25%"), None, false, "Pesquisa avançada"),
        ];
        for (width, align, nowrap, text) in columns {
            let cell = doc.create_element("td");
            if let Some(width) = width {
                doc.set_attr(cell, "width", width);
            }
            if let Some(align) = align {
                doc.set_attr(cell, "align", align);
            }
            if nowrap {
                doc.set_attr(cell, "nowrap", "");
            }
            doc.append_text(cell, text);
            doc.append_child(row, cell);
            cells.push(cell);
        }

        let mut engine = StyleEngine::new(Viewport {
            width: 600.0,
            height: 400.0,
        });
        engine.add_css(
            "body { margin: 0; } table { width: 600px; } td { border: 0; }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            Size {
                width: 600.0,
                height: 400.0,
            },
        );
        (tree, cells)
    }

    #[test]
    fn percentage_columns_give_the_remaining_space_to_the_auto_cell() {
        let (tree, cells) = search_table_page();
        let left = tree.find_node(cells[0]).unwrap();
        let middle = tree.find_node(cells[1]).unwrap();
        let right = tree.find_node(cells[2]).unwrap();

        assert!(
            (left.rect.width - 150.0).abs() < 1.0,
            "25% column should be 150px, got {}",
            left.rect.width
        );
        assert!(
            (right.rect.width - 150.0).abs() < 1.0,
            "25% column should be 150px, got {}",
            right.rect.width
        );
        assert!(
            (middle.rect.width - 300.0).abs() < 1.0,
            "auto column should get the leftover 300px, got {}",
            middle.rect.width
        );
        assert!(middle.rect.x >= left.rect.right());
        assert!(right.rect.x >= middle.rect.right());
    }

    #[test]
    fn single_cell_table_does_not_panic() {
        let mut tree = LayoutTree::new();
        let table = tree.push(crate::tree::LayoutBox::new(
            None,
            BoxType::Table,
            browser_style::ComputedStyle::initial(),
        ));
        tree.boxes[table].rect = Rect::new(0.0, 0.0, 100.0, 0.0);
        tree.boxes[table].content = tree.boxes[table].rect;
        tree.root = Some(table);
        let doc = Document::new();
        assert_eq!(layout_table(&mut tree, &doc, table), 0.0);
    }
}
