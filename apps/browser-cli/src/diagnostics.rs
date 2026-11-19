//! Rendering diagnostics that reach beyond the computed-style/layout views.
//!
//! Two helpers live here:
//!
//! * [`winning_declarations`] replays the cascade for one element and reports
//!   the declaration that won for each property, together with the selector and
//!   origin that produced it. This is the "why did this property end up this
//!   way?" view that is missing from a plain computed-style dump.
//! * [`layout_json`] serialises every layout box (border/content rects, edges,
//!   children, text) as JSON, which is convenient for diffing real sites.

use browser_core::page::Page;
use browser_css::{ComponentValue, Declaration, Origin, Rule, Stylesheet, Value};
use browser_dom::{Document, NodeId};
use browser_selectors::{ElementState, Specificity, matches_with_state, parse_selector};
use browser_style::cascade::Candidate;
use browser_style::{MediaContext, Property, cascade, media_matches};

/// The declaration that won the cascade for a single property.
#[derive(Debug, Clone)]
pub struct WinningDeclaration {
    /// Longhand property the declaration applies to.
    pub property: Property,
    /// Winning declared value, rendered back as CSS text.
    pub value: String,
    /// Selector that matched, or a pseudo-source such as `style attribute`.
    pub source: String,
    /// Origin of the winning declaration.
    pub origin: Origin,
    /// Specificity of the selector (inline styles use the inline flag instead).
    pub specificity: Specificity,
    /// Whether the declaration was marked `!important`.
    pub important: bool,
    /// Whether the declaration came from an inline `style` attribute.
    pub inline: bool,
}

impl WinningDeclaration {
    /// A short human-readable attribution, suitable for a trailing comment.
    pub fn attribution(&self) -> String {
        let mut out = format!(
            "{} `{}`, specificity {}-{}-{}",
            origin_label(self.origin),
            self.source,
            self.specificity.a,
            self.specificity.b,
            self.specificity.c
        );
        if self.important {
            out.push_str(", !important");
        }
        if self.inline {
            out.push_str(", inline");
        }
        out
    }
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::UserAgent => "user-agent",
        Origin::User => "user",
        Origin::Author => "author",
    }
}

/// A candidate paired with the source that produced it.
type SourcedCandidate = (Candidate, String);

/// Returns the winning declarations for `node` in `page`, in property order.
///
/// The cascade is replayed with the same inputs as the engine (user-agent
/// sheet, author sheets in order, then the inline `style` attribute). Custom
/// properties and media rules that do not match the current viewport are
/// ignored, mirroring the engine's own collection pass.
pub fn winning_declarations(page: &Page, node: NodeId) -> Vec<WinningDeclaration> {
    let viewport = page.viewport();
    let media = MediaContext {
        width: viewport.width,
        height: viewport.height,
        ..MediaContext::default()
    };
    let state = ElementState::default();
    let document = page.document.borrow();
    let mut candidates: Vec<SourcedCandidate> = Vec::new();
    let mut order = 0_usize;

    let ua = Stylesheet::parse(browser_style::ua::DEFAULT_STYLESHEET, Origin::UserAgent);
    collect_from_rules(
        &document,
        node,
        &ua.rules,
        Origin::UserAgent,
        &media,
        &state,
        &mut candidates,
        &mut order,
    );

    for css in page.author_css() {
        let sheet = Stylesheet::parse(css, Origin::Author);
        collect_from_rules(
            &document,
            node,
            &sheet.rules,
            Origin::Author,
            &media,
            &state,
            &mut candidates,
            &mut order,
        );
    }

    if let Some(css) = document.attr(node, "style") {
        let specificity = browser_style::specificity::inline_specificity();
        for declaration in browser_css::parse_inline_style(css) {
            order += 1;
            push_declaration(
                &declaration,
                Origin::Author,
                specificity,
                true,
                "style attribute",
                &mut candidates,
                order,
            );
        }
    }

    let winners = pick_winners(candidates);
    let mut out: Vec<WinningDeclaration> = winners
        .into_iter()
        .map(|(candidate, source)| WinningDeclaration {
            property: candidate.property,
            value: value_text(&candidate.value),
            source,
            origin: candidate.origin,
            specificity: candidate.specificity,
            important: candidate.important,
            inline: candidate.inline,
        })
        .collect();
    out.sort_by_key(|decl| decl.property.name());
    out
}

#[allow(unreachable_patterns)] // tolerate new at-rule kinds added upstream
#[allow(clippy::too_many_arguments)]
fn collect_from_rules(
    doc: &Document,
    node: NodeId,
    rules: &[Rule],
    origin: Origin,
    media: &MediaContext,
    state: &ElementState,
    candidates: &mut Vec<SourcedCandidate>,
    order: &mut usize,
) {
    for rule in rules {
        match rule {
            Rule::Style(style_rule) => {
                for selector_text in &style_rule.selectors {
                    let Ok(selector) = parse_selector(selector_text) else {
                        continue;
                    };
                    if !matches_with_state(doc, node, &selector, state) {
                        continue;
                    }
                    for declaration in &style_rule.declarations {
                        *order += 1;
                        push_declaration(
                            declaration,
                            origin,
                            selector.specificity,
                            false,
                            selector_text,
                            candidates,
                            *order,
                        );
                    }
                }
            }
            Rule::Media(media_rule) => {
                if media_matches(&media_rule.query, media) {
                    collect_from_rules(
                        doc,
                        node,
                        &media_rule.rules,
                        origin,
                        media,
                        state,
                        candidates,
                        order,
                    );
                }
            }
            Rule::Import(import) => {
                if let Some(query) = &import.media
                    && !media_matches(query, media)
                {
                    continue;
                }
            }
            // Keyframes, font faces and conditional/at-rule variants added by
            // the engine do not contribute cascade winners for this element.
            _ => {}
        }
    }
}

fn push_declaration(
    declaration: &Declaration,
    origin: Origin,
    specificity: Specificity,
    inline: bool,
    source: &str,
    candidates: &mut Vec<SourcedCandidate>,
    order: usize,
) {
    if browser_style::variables::is_custom_property(&declaration.name) {
        return;
    }
    for (property, value) in cascade::expand(declaration) {
        candidates.push((
            Candidate {
                property,
                value,
                important: declaration.important,
                origin,
                specificity,
                inline,
                order,
            },
            source.to_string(),
        ));
    }
}

/// Mirrors `browser_style::cascade::cascade`, keeping the source alongside the
/// winner. Equal weights let the later declaration win, exactly as in the
/// engine.
fn pick_winners(candidates: Vec<SourcedCandidate>) -> Vec<SourcedCandidate> {
    let mut winners: Vec<SourcedCandidate> = Vec::new();
    for candidate in candidates {
        match winners
            .iter_mut()
            .find(|(w, _)| w.property == candidate.0.property)
        {
            Some(current) => {
                if candidate.0.weight() >= current.0.weight() {
                    *current = candidate;
                }
            }
            None => winners.push(candidate),
        }
    }
    winners
}

/// Renders a parsed CSS value back as text so the diagnostic reads like CSS.
fn value_text(value: &Value) -> String {
    value.components.iter().map(value_component_text).collect()
}

#[allow(unreachable_patterns)] // tolerate new CSS value kinds added upstream
fn value_component_text(component: &ComponentValue) -> String {
    match component {
        ComponentValue::Ident(text) => text.clone(),
        ComponentValue::Number(number) => number.to_string(),
        ComponentValue::Percentage(value) => format!("{value}%"),
        ComponentValue::Dimension(number, unit) => format!("{number}{unit}"),
        ComponentValue::String(text) => format!("\"{}\"", text.replace('"', "\\\"")),
        ComponentValue::Url(url) => format!("url({url})"),
        ComponentValue::Hash(hash) => format!("#{hash}"),
        ComponentValue::Function(name, args) => {
            let args: Vec<String> = args.iter().map(value_component_text).collect();
            format!("{}({})", name, args.join(""))
        }
        ComponentValue::Delim(ch) => ch.to_string(),
        ComponentValue::Comma => ",".to_string(),
        ComponentValue::Whitespace => " ".to_string(),
        _ => format!("{component:?}"),
    }
}

/// Serialises a layout tree to JSON with one object per box.
///
/// Rects use `{x,y,width,height}`; edges use `{top,right,bottom,left}`.
/// Non-finite floats are written as `0.0` so the result is always valid JSON.
pub fn layout_json(tree: &browser_layout::LayoutTree) -> String {
    let mut out = String::from("{\n");
    push_field(&mut out, 1, "document_height", &num(tree.document_height));
    push_line(
        &mut out,
        1,
        &format!("\"box_count\": {},", tree.boxes.len()),
    );
    push_line(&mut out, 1, &format!("\"truncated\": {},", tree.truncated));
    push_line(
        &mut out,
        1,
        &format!(
            "\"root\": {},",
            tree.root.map_or("null".to_string(), |r| r.to_string())
        ),
    );
    out.push_str("  \"boxes\": [\n");
    let last = tree.boxes.len().saturating_sub(1);
    for (index, b) in tree.boxes.iter().enumerate() {
        out.push_str("    {\n");
        push_line(&mut out, 3, &format!("\"index\": {index},"));
        push_line(
            &mut out,
            3,
            &format!(
                "\"node\": {},",
                b.node.map_or("null".to_string(), |n| n.0.to_string())
            ),
        );
        push_line(
            &mut out,
            3,
            &format!("\"type\": \"{}\",", box_type_name(b.box_type)),
        );
        push_field(&mut out, 3, "rect", &rect_json(&b.rect));
        push_field(&mut out, 3, "content", &rect_json(&b.content));
        push_field(&mut out, 3, "margins", &edges_json(&b.margins));
        push_field(&mut out, 3, "borders", &edges_json(&b.borders));
        push_field(&mut out, 3, "padding", &edges_json(&b.paddings));
        let children: Vec<String> = b.children.iter().map(usize::to_string).collect();
        push_line(
            &mut out,
            3,
            &format!("\"children\": [{}],", children.join(", ")),
        );
        push_line(
            &mut out,
            3,
            &format!("\"text\": \"{}\",", escape_json(&b.text)),
        );
        push_line(&mut out, 3, &format!("\"z_index\": {},", b.z_index));
        push_line(&mut out, 3, &format!("\"positioned\": {},", b.positioned));
        push_line(
            &mut out,
            3,
            &format!(
                "\"scroll_offset\": [{}, {}],",
                num(b.scroll_offset.0),
                num(b.scroll_offset.1)
            ),
        );
        push_line(
            &mut out,
            3,
            &format!("\"scroll_height\": {}", num(b.scroll_height)),
        );
        out.push_str("    }");
        if index != last {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn push_field(out: &mut String, indent: usize, name: &str, value: &str) {
    push_line(out, indent, &format!("\"{name}\": {value},"));
}

fn push_line(out: &mut String, indent: usize, line: &str) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    out.push_str(line);
    out.push('\n');
}

#[allow(unreachable_patterns)] // tolerate new box kinds added upstream
fn box_type_name(kind: browser_layout::BoxType) -> &'static str {
    match kind {
        browser_layout::BoxType::Block => "block",
        browser_layout::BoxType::Inline => "inline",
        browser_layout::BoxType::InlineBlock => "inline-block",
        browser_layout::BoxType::Text => "text",
        browser_layout::BoxType::ListItem => "list-item",
        browser_layout::BoxType::Flex => "flex",
        browser_layout::BoxType::Grid => "grid",
        browser_layout::BoxType::Table => "table",
        browser_layout::BoxType::TableRow => "table-row",
        browser_layout::BoxType::TableCell => "table-cell",
        browser_layout::BoxType::Image => "image",
        browser_layout::BoxType::LineBreak => "line-break",
        browser_layout::BoxType::AnonymousBlock => "anonymous-block",
        _ => "other",
    }
}

fn rect_json(rect: &browser_layout::Rect) -> String {
    format!(
        "{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}",
        num(rect.x),
        num(rect.y),
        num(rect.width),
        num(rect.height)
    )
}

fn edges_json(edges: &browser_layout::Edges) -> String {
    format!(
        "{{\"top\":{},\"right\":{},\"bottom\":{},\"left\":{}}}",
        num(edges.top),
        num(edges.right),
        num(edges.bottom),
        num(edges.left)
    )
}

fn num(value: f32) -> String {
    if value.is_finite() {
        format!("{value:.2}")
    } else {
        "0.0".to_string()
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::{Browser, Settings};

    fn page(html: &str) -> Browser {
        let mut browser = Browser::new(Settings::headless());
        browser.load_html(html, None).unwrap();
        browser
    }

    #[test]
    fn winning_declaration_prefers_id_selector() {
        let browser = page(
            "<html><head><style>\
             .card { color: blue; } \
             #hero { color: red; font-size: 20px; }\
             </style></head><body><p id='hero' class='card'>hi</p></body></html>",
        );
        let page = browser.page();
        let doc = page.document.borrow();
        let node = doc.get_elements_by_tag_name("p")[0];
        drop(doc);

        let winners = winning_declarations(page, node);
        let color = winners
            .iter()
            .find(|w| w.property == Property::Color)
            .unwrap();
        assert_eq!(color.value, "red");
        assert_eq!(color.source, "#hero");
        assert_eq!(color.specificity, Specificity::new(1, 0, 0));
        assert!(color.attribution().contains("author"));
    }

    #[test]
    fn winning_declaration_inline_beats_selector() {
        let browser = page(
            "<html><head><style>p { color: red; }</style></head>\
             <body><p style='color: green'>hi</p></body></html>",
        );
        let page = browser.page();
        let doc = page.document.borrow();
        let node = doc.get_elements_by_tag_name("p")[0];
        drop(doc);

        let winners = winning_declarations(page, node);
        let color = winners
            .iter()
            .find(|w| w.property == Property::Color)
            .unwrap();
        assert_eq!(color.value, "green");
        assert!(color.inline);
        assert_eq!(color.source, "style attribute");
    }

    #[test]
    fn winning_declaration_important_beats_specificity() {
        let browser = page(
            "<html><head><style>\
             #hero { color: red; } \
             p { color: blue !important; }\
             </style></head><body><p id='hero'>hi</p></body></html>",
        );
        let page = browser.page();
        let doc = page.document.borrow();
        let node = doc.get_elements_by_tag_name("p")[0];
        drop(doc);

        let winners = winning_declarations(page, node);
        let color = winners
            .iter()
            .find(|w| w.property == Property::Color)
            .unwrap();
        assert_eq!(color.value, "blue");
        assert!(color.important);
    }

    #[test]
    fn layout_json_has_boxes_with_rects() {
        let browser =
            page("<html><body><div style='width:120px;height:40px'>hi</div></body></html>");
        let json = layout_json(&browser.page().layout);
        assert!(json.starts_with('{') && json.trim_end().ends_with('}'));
        for field in [
            "\"document_height\"",
            "\"box_count\"",
            "\"boxes\"",
            "\"rect\"",
            "\"content\"",
            "\"children\"",
            "\"width\":120.00",
        ] {
            assert!(json.contains(field), "missing {field} in JSON:\n{json}");
        }
    }

    #[test]
    fn layout_json_escapes_text() {
        let browser = page("<html><body><p>a\"b\\c</p></body></html>");
        let json = layout_json(&browser.page().layout);
        assert!(json.contains("a\\\"b\\\\c"), "JSON:\n{json}");
    }
}
