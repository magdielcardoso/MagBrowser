//! User-agent default stylesheet.
//!
//! This is the engine's built-in style sheet, equivalent in spirit to the HTML
//! rendering spec's suggested defaults. It is intentionally small but covers every
//! element the HTML parser can produce.

/// Default styles applied with the `user-agent` origin.
pub const DEFAULT_STYLESHEET: &str = r#"
html, body, div, p, section, article, aside, header, footer, nav, main,
address, figure, figcaption, blockquote, pre, form, fieldset, legend,
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dt, dd, table, thead, tbody,
tfoot, tr, td, th, caption, details, summary, hr, center {
    display: block;
}

head, title, meta, link, style, script, base, template, noscript, param, source, track {
    display: none;
}

/* SVG definition/metadata elements are not rendered directly. The HTML parser
   does not special-case them, so their descendants (for example a <text>
   inside a <pattern>) can be laid out and leak stray text into the page. Hide
   the whole subtree, the same way non-rendering head content is hidden. */
defs, pattern, symbol, clipPath, mask, marker, linearGradient, radialGradient,
filter, title, desc, metadata {
    display: none;
}

li { display: list-item; }
table { display: table; }
tr { display: table-row; }
td, th { display: table-cell; vertical-align: middle; }
thead, tbody, tfoot { display: block; }

body { margin: 8px; }

h1 { font-size: 2em; font-weight: bold; margin-top: 0.67em; margin-bottom: 0.67em; }
h2 { font-size: 1.5em; font-weight: bold; margin-top: 0.83em; margin-bottom: 0.83em; }
h3 { font-size: 1.17em; font-weight: bold; margin-top: 1em; margin-bottom: 1em; }
h4 { font-size: 1em; font-weight: bold; margin-top: 1.33em; margin-bottom: 1.33em; }
h5 { font-size: 0.83em; font-weight: bold; margin-top: 1.67em; margin-bottom: 1.67em; }
h6 { font-size: 0.67em; font-weight: bold; margin-top: 2.33em; margin-bottom: 2.33em; }

p, pre, blockquote, dl, figure { margin-top: 1em; margin-bottom: 1em; }
blockquote { margin-left: 40px; margin-right: 40px; }

ul, ol { margin-top: 1em; margin-bottom: 1em; padding-left: 40px; }
ul { list-style-type: disc; }
ol { list-style-type: decimal; }
li { list-style-type: disc; }

pre, code, kbd, samp, tt { font-family: monospace; }
pre { white-space: pre; }
code { font-size: 0.95em; }

a:link { color: rgb(0, 0, 238); text-decoration: underline; }
a:visited { color: rgb(85, 26, 139); text-decoration: underline; }

strong, b { font-weight: bold; }
em, i, cite, var { font-style: italic; }
u, ins { text-decoration: underline; }
s, strike, del { text-decoration: line-through; }
big { font-size: 1.2em; }
small { font-size: 0.83em; }
sub, sup { font-size: 0.83em; }
sup { vertical-align: super; }
sub { vertical-align: sub; }

hr { border-top: 1px solid rgb(200, 200, 200); border-right: 0; border-bottom: 0; border-left: 0; margin-top: 0.5em; margin-bottom: 0.5em; }

img { display: inline-block; }
br { display: inline; }

input, select, textarea, button { display: inline-block; }
input[type=hidden] { display: none; }
button, input { font-size: 13px; }

/* Modern form defaults. These live in the user-agent origin, so any author
   declaration for the same property wins in the cascade regardless of
   specificity (e.g. a plain `input { border-radius: 0 }`). Sites that paint a
   control with a background image (sprites we cannot decode) therefore fall
   back to the colors below. */

/* Text-like fields: a comfortable, rounded, white entry field. */
input[type=text], input[type=search], input[type=tel], input[type=url],
input[type=email], input[type=password], input:not([type]) {
    background-color: rgb(255, 255, 255);
    border: 1px solid rgb(218, 220, 224);
    border-radius: 24px;
    padding: 8px 16px;
    color: rgb(32, 33, 36);
}

/* Push buttons: soft gray pills with a pointer cursor. */
input[type=submit], input[type=reset], input[type=button], button {
    background-color: rgb(248, 249, 250);
    border: 1px solid rgb(218, 220, 224);
    border-radius: 20px;
    padding: 8px 16px;
    color: rgb(32, 33, 36);
    cursor: pointer;
}

/* Checkboxes and radios: a definite 16x16 box (border-box so the border does
   not eat into the control), a visible border and a neutral fill. The checked
   state is shown with a colored fill; generated-content markers are not an
   option because replaced elements (img/input) never paint ::before/::after.
   These are UA declarations, so any author rule wins in the cascade. */
input[type=checkbox], input[type=radio] {
    width: 16px;
    height: 16px;
    box-sizing: border-box;
    border: 1px solid rgb(117, 117, 117);
    background-color: rgb(255, 255, 255);
    margin: 0;
    vertical-align: middle;
}
input[type=checkbox] { border-radius: 2px; }
input[type=radio] { border-radius: 50%; }
input[type=checkbox]:checked, input[type=radio]:checked {
    background-color: rgb(33, 150, 243);
    border-color: rgb(25, 118, 210);
}

/* Keep form controls at least 16px on narrow viewports so mobile browsers do
   not auto-zoom when a field gains focus. Author CSS can still override it. */
@media (max-width: 600px) {
    input, select, textarea, button { font-size: 16px; }
}

center { text-align: center; }
th { font-weight: bold; text-align: center; }
td { text-align: left; }

caption { text-align: center; }

[hidden] { display: none; }
"#;

#[cfg(test)]
mod tests {
    use browser_css::Color;
    use browser_dom::{Document, NodeId};

    use crate::{BorderStyle, Dim, Display, Origin, Side, StyleEngine, Viewport};

    /// Builds `<html><body><input type=… [checked]></body></html>`.
    fn input_document(kind: &str, checked: bool) -> (Document, NodeId) {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        let input = doc.create_element("input");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        doc.append_child(body, input);
        doc.set_attr(input, "type", kind);
        if checked {
            doc.set_attr(input, "checked", "");
        }
        (doc, input)
    }

    #[test]
    fn ua_hides_non_rendering_svg_metadata_elements() {
        // Mixed case on purpose: type selectors must match case-insensitively
        // (`clipPath` in CSS vs. the parser's lower-cased element name).
        let tags = [
            "defs",
            "pattern",
            "symbol",
            "clipPath",
            "mask",
            "marker",
            "linearGradient",
            "radialGradient",
            "filter",
            "title",
            "desc",
            "metadata",
        ];
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let mut nodes = Vec::new();
        for tag in tags {
            let element = doc.create_element(tag);
            doc.append_child(body, element);
            nodes.push((tag, element));
        }
        // The text content that used to leak out lives inside a pattern.
        let pattern_text = doc.create_element("text");
        doc.append_child(nodes[1].1, pattern_text);
        // Ordinary SVG text must stay renderable.
        let visible_text = doc.create_element("text");
        doc.append_child(body, visible_text);

        let engine = StyleEngine::new(Viewport::default());
        let styled = engine.compute(&doc);
        for (tag, node) in nodes {
            assert!(
                styled.get(node).unwrap().is_display_none(),
                "<{tag}> should be display:none in the UA sheet"
            );
        }
        assert_ne!(
            styled.get(visible_text).unwrap().display(),
            Display::None,
            "plain <text> must not be hidden by the SVG metadata rule"
        );
    }

    #[test]
    fn ua_gives_checkboxes_and_radios_a_visible_control_box() {
        for kind in ["checkbox", "radio"] {
            let (doc, input) = input_document(kind, false);
            let engine = StyleEngine::new(Viewport::default());
            let styled = engine.compute(&doc);
            let style = styled.get(input).unwrap();
            assert_eq!(style.width(), Dim::Px(16.0), "{kind} width");
            assert_eq!(style.height(), Dim::Px(16.0), "{kind} height");
            assert!(style.border_width(Side::Top) >= 1.0, "{kind} border width");
            assert_eq!(
                style.border_style(Side::Top),
                BorderStyle::Solid,
                "{kind} border style"
            );
            assert_ne!(
                style.background_color(),
                Color::transparent(),
                "{kind} needs a fill"
            );
        }
    }

    #[test]
    fn ua_rounds_radios_more_than_checkboxes() {
        let (radio_doc, radio) = input_document("radio", false);
        let (checkbox_doc, checkbox) = input_document("checkbox", false);
        let engine = StyleEngine::new(Viewport::default());
        let radio_radius = engine
            .compute(&radio_doc)
            .get(radio)
            .unwrap()
            .border_radius();
        let checkbox_radius = engine
            .compute(&checkbox_doc)
            .get(checkbox)
            .unwrap()
            .border_radius();
        assert!(
            radio_radius.iter().any(|&r| r >= 50.0),
            "radios should be fully round, got {radio_radius:?}"
        );
        assert!(
            checkbox_radius.iter().any(|&r| r > 0.0) && checkbox_radius.iter().all(|&r| r < 50.0),
            "checkboxes should be only slightly rounded, got {checkbox_radius:?}"
        );
    }

    #[test]
    fn checked_controls_get_a_visible_marker() {
        for kind in ["checkbox", "radio"] {
            let (unchecked_doc, unchecked) = input_document(kind, false);
            let (checked_doc, checked) = input_document(kind, true);
            let engine = StyleEngine::new(Viewport::default());
            let plain = engine
                .compute(&unchecked_doc)
                .get(unchecked)
                .unwrap()
                .background_color();
            let marked = engine
                .compute(&checked_doc)
                .get(checked)
                .unwrap()
                .background_color();
            assert_ne!(
                plain, marked,
                "{kind} :checked should visibly change the background"
            );
        }
    }

    #[test]
    fn author_css_overrides_ua_control_styles() {
        // Author origin outranks UA origin even when the UA selector is more
        // specific (`input[type=checkbox]:checked` vs `input[type=checkbox]`).
        let (doc, input) = input_document("checkbox", true);
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(
            "input[type=checkbox] { width: 32px; height: 24px; border-radius: 0; \
             background-color: rgb(1, 2, 3); }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        let style = styled.get(input).unwrap();
        assert_eq!(style.width(), Dim::Px(32.0));
        assert_eq!(style.height(), Dim::Px(24.0));
        assert_eq!(style.border_radius(), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(style.background_color(), Color::rgb(1, 2, 3));
    }
}
