//! Behavioural tests for the tokenizer, tree builder and public API.

use browser_dom::{Document, Namespace, NodeData, NodeId, serialize, serialize_children};

use crate::{
    ParseOptions, Token, Tokenizer, TreeBuilder, parse_document, parse_document_with,
    parse_fragment,
};

fn parsed(input: &str) -> Document {
    parse_document(input)
}

fn body_of(doc: &Document) -> NodeId {
    doc.head_and_body().1.expect("document must have a body")
}

fn body_html(input: &str) -> String {
    let doc = parsed(input);
    serialize_children(&doc, body_of(&doc))
}

fn head_html(input: &str) -> String {
    let doc = parsed(input);
    let (head, _) = doc.head_and_body();
    serialize_children(&doc, head.expect("document must have a head"))
}

fn text_of(input: &str) -> String {
    let doc = parsed(input);
    doc.text_content(doc.document_element().expect("html element"))
}

fn assert_body(input: &str, expected: &str) {
    assert_eq!(body_html(input), expected, "input: {input}");
}

fn find_tag(doc: &Document, name: &str) -> NodeId {
    doc.get_elements_by_tag_name(name)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("missing <{name}>"))
}

// -------------------------------------------------------------------------
// Document structure
// -------------------------------------------------------------------------

#[test]
fn creates_implicit_html_head_and_body() {
    let doc = parsed("<!doctype html><p>hi</p>");
    let (head, body) = doc.head_and_body();
    assert!(head.is_some());
    assert!(body.is_some());
    assert_eq!(serialize_children(&doc, body.unwrap()), "<p>hi</p>");
}

#[test]
fn empty_input_still_produces_a_full_structure() {
    let doc = parsed("");
    let html = doc.document_element().expect("html");
    assert_eq!(doc.element_name(html), Some("html"));
    assert_eq!(doc.node(html).children.len(), 2);
    let (head, body) = doc.head_and_body();
    assert!(head.is_some() && body.is_some());
}

#[test]
fn preserves_doctype_name_and_ids() {
    let doc = parsed("<!DOCTYPE html PUBLIC \"pub\" \"sys\"><p>x</p>");
    let doctype = doc
        .nodes()
        .find(|node| matches!(node.data, NodeData::Doctype { .. }))
        .expect("doctype");
    match &doctype.data {
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            assert_eq!(name, "html");
            assert_eq!(public_id, "pub");
            assert_eq!(system_id, "sys");
        }
        _ => unreachable!(),
    }
    assert!(serialize(&doc, doc.root()).starts_with("<!DOCTYPE html>"));
}

#[test]
fn title_sets_document_title_and_stays_in_head() {
    let doc = parsed("<head><title>Hello &amp; bye</title></head><body>x</body>");
    assert_eq!(doc.title, "Hello & bye");
    assert_eq!(head_html("<title>Hello</title>"), "<title>Hello</title>");
}

#[test]
fn only_the_first_title_updates_the_title() {
    let doc = parsed("<title>first</title><title>second</title>");
    assert_eq!(doc.title, "first");
}

#[test]
fn base_href_sets_document_base_url() {
    let doc = parsed("<head><base href=\"/assets/\"></head>");
    assert_eq!(doc.base_url.as_deref(), Some("/assets/"));
    let doc = parsed("<base href=\"/one/\"><base href=\"/two/\">");
    assert_eq!(doc.base_url.as_deref(), Some("/one/"));
}

#[test]
fn meta_charset_is_accepted_without_changing_structure() {
    let doc = parsed("<meta charset=\"utf-8\"><p>x</p>");
    let meta = find_tag(&doc, "meta");
    assert_eq!(doc.attr(meta, "charset"), Some("utf-8"));
    assert_eq!(body_html("<meta charset=\"utf-8\"><p>x</p>"), "<p>x</p>");
}

#[test]
fn merges_attributes_on_html_and_body() {
    let doc = parsed("<html lang=\"en\"><body class=\"page\"><p>x</p>");
    let html = doc.document_element().unwrap();
    assert_eq!(doc.attr(html, "lang"), Some("en"));
    let body = body_of(&doc);
    assert_eq!(doc.attr(body, "class"), Some("page"));
}

#[test]
fn stray_content_after_head_moves_into_body() {
    assert_body("<head><title>t</title></head>text", "text");
}

#[test]
fn whitespace_between_head_and_body_is_kept_in_html() {
    let doc = parsed("<head></head>\n<body>x</body>");
    let html = doc.document_element().unwrap();
    let children = doc.children(html).to_vec();
    assert!(children.len() >= 3);
    assert_eq!(doc.text_content(html), "\nx");
}

// -------------------------------------------------------------------------
// Tokenizer behaviour through the public API
// -------------------------------------------------------------------------

#[test]
fn tokenizer_coalesces_text_and_decodes_entities() {
    let tokens: Vec<Token> = Tokenizer::new("<p>a&amp;b</p>").collect();
    assert_eq!(
        tokens,
        vec![
            Token::StartTag {
                name: "p".to_string(),
                attributes: Vec::new(),
                self_closing: false,
            },
            Token::Text("a&b".to_string()),
            Token::EndTag {
                name: "p".to_string(),
            },
            Token::Eof,
        ]
    );
}

#[test]
fn tokenizer_handles_weird_attribute_whitespace() {
    let tokens: Vec<Token> = Tokenizer::new("<div \n a = 'b'\tc =d  data-e=\"f\" >").collect();
    let Token::StartTag {
        name, attributes, ..
    } = &tokens[0]
    else {
        panic!("expected start tag");
    };
    assert_eq!(name, "div");
    assert_eq!(
        attributes,
        &vec![
            ("a".to_string(), "b".to_string()),
            ("c".to_string(), "d".to_string()),
            ("data-e".to_string(), "f".to_string()),
        ]
    );
}

#[test]
fn tokenizer_marks_void_self_closing_tags() {
    let tokens: Vec<Token> = Tokenizer::new("<br/><input type=\"text\"/>").collect();
    assert!(matches!(&tokens[0], Token::StartTag { name, self_closing: true, .. } if name == "br"));
    assert!(
        matches!(&tokens[1], Token::StartTag { name, self_closing: true, .. } if name == "input")
    );
}

#[test]
fn tokenizer_decodes_attribute_entities() {
    assert_body(
        "<a href=\"?a=1&amp;b=2\">x</a>",
        "<a href=\"?a=1&amp;b=2\">x</a>",
    );
    let doc = parsed("<a title=\"&lt;&#65;&#x42;\">x</a>");
    let anchor = find_tag(&doc, "a");
    assert_eq!(doc.attr(anchor, "title"), Some("<AB"));
}

#[test]
fn tokenizer_keeps_duplicate_attribute_first() {
    let doc = parsed("<div id=\"a\" id=\"b\"></div>");
    let div = find_tag(&doc, "div");
    assert_eq!(doc.attr(div, "id"), Some("a"));
}

#[test]
fn tokenizer_lowercases_tag_and_attribute_names() {
    let doc = parsed("<DIV DATA-X=\"Y\"></DIV>");
    let div = find_tag(&doc, "div");
    assert_eq!(doc.attr(div, "data-x"), Some("Y"));
    assert_eq!(doc.element_name(div), Some("div"));
}

#[test]
fn tokenizer_handles_bogus_comments_and_cdata_as_comments() {
    let doc = parsed("<?xml version=\"1.0\"?><![CDATA[a<b]]>");
    let comments: Vec<String> = doc
        .nodes()
        .filter_map(|node| match &node.data {
            NodeData::Comment(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(comments, vec!["?xml version=\"1.0\"?", "[CDATA[a<b]]"]);
}

#[test]
fn tokenizer_ends_cleanly_at_eof_in_every_construct() {
    for input in ["<", "<div", "<!-- x", "<!DOCTYPE", "<div a=\"", "<div a='x"] {
        let doc = parsed(input);
        assert!(doc.document_element().is_some(), "input: {input}");
    }
}

// -------------------------------------------------------------------------
// Void elements and optional end tags
// -------------------------------------------------------------------------

#[test]
fn void_elements_never_get_children_or_end_tags() {
    assert_body(
        "<br><img src=\"x.png\"><hr><input><wbr>",
        "<br><img src=\"x.png\"><hr><input><wbr>",
    );
    let doc = parsed("<br>text");
    let br = find_tag(&doc, "br");
    assert!(doc.children(br).is_empty());
}

#[test]
fn br_end_tag_is_treated_as_a_br_start_tag() {
    assert_body("a</br>b", "a<br>b");
}

#[test]
fn p_is_closed_implicitly_by_block_elements() {
    assert_body("<p>a<div>b</div>", "<p>a</p><div>b</div>");
    assert_body("<p>a<p>b", "<p>a</p><p>b</p>");
}

#[test]
fn p_is_closed_implicitly_by_lists_and_headings() {
    assert_body("<p>a<ul><li>b", "<p>a</p><ul><li>b</li></ul>");
    assert_body("<p>a<h1>b", "<p>a</p><h1>b</h1>");
}

#[test]
fn list_items_close_each_other() {
    assert_body(
        "<ul><li>a<li>b<li>c</ul>",
        "<ul><li>a</li><li>b</li><li>c</li></ul>",
    );
}

#[test]
fn definition_items_close_each_other() {
    assert_body(
        "<dl><dt>a<dd>b<dt>c</dl>",
        "<dl><dt>a</dt><dd>b</dd><dt>c</dt></dl>",
    );
}

#[test]
fn options_and_optgroups_close_each_other() {
    assert_body(
        "<select><option>a<option>b<optgroup label=g><option>c</select>",
        "<select><option>a</option><option>b</option><optgroup label=\"g\"><option>c</option></optgroup></select>",
    );
}

#[test]
fn headings_close_previous_headings() {
    assert_body("<h1>a<h2>b", "<h1>a</h1><h2>b</h2>");
}

// -------------------------------------------------------------------------
// Tables
// -------------------------------------------------------------------------

#[test]
fn table_gets_implicit_tbody_row_and_cells() {
    assert_body(
        "<table><tr><td>a<td>b</table>",
        "<table><tbody><tr><td>a</td><td>b</td></tr></tbody></table>",
    );
}

#[test]
fn table_sections_and_headers_are_supported() {
    assert_body(
        "<table><thead><tr><th>h<tbody><tr><td>d</table>",
        "<table><thead><tr><th>h</th></tr></thead><tbody><tr><td>d</td></tr></tbody></table>",
    );
}

#[test]
fn table_caption_and_colgroup_are_supported() {
    assert_body(
        "<table><caption>c</caption><colgroup><col><col></colgroup><tr><td>x</table>",
        "<table><caption>c</caption><colgroup><col><col></colgroup><tbody><tr><td>x</td></tr></tbody></table>",
    );
}

#[test]
fn col_creates_an_implicit_colgroup() {
    assert_body(
        "<table><col></table>",
        "<table><colgroup><col></colgroup></table>",
    );
}

#[test]
fn stray_table_end_tags_do_not_break_parsing() {
    assert_body("<table></td></tr></table>", "<table></table>");
}

// -------------------------------------------------------------------------
// Formatting and misnesting
// -------------------------------------------------------------------------

#[test]
fn misnested_formatting_is_recovered() {
    assert_body("<b><i>x</b>y</i>", "<b><i>x</i></b><i>y</i>");
    assert_body("<b><i></b></i>", "<b><i></i></b><i></i>");
}

#[test]
fn formatting_is_reconstructed_after_misnested_blocks() {
    assert_body("<b><i><p>x</b>y</i>", "<b><i><p>x</p></i></b><i>y</i>");
}

#[test]
fn nested_formatting_survives_normal_end_tags() {
    assert_body("<b><i><u>x</u></i></b>", "<b><i><u>x</u></i></b>");
}

#[test]
fn misnested_block_end_tags_close_to_the_right_element() {
    assert_body("<div><p></div>", "<div><p></p></div>");
    assert_body("<div><span></div>", "<div><span></span></div>");
}

#[test]
fn stray_end_tags_are_ignored() {
    assert_body("x</foo>y", "xy");
    let result = parse_document_with("x</foo>y", &ParseOptions::default());
    assert!(!result.errors.is_empty());
}

// -------------------------------------------------------------------------
// Rawtext and RCDATA elements
// -------------------------------------------------------------------------

#[test]
fn script_content_is_rawtext() {
    let doc = parsed("<script>if (a < b) { x = '</scr' + 'ipt>'; }</script>");
    let script = find_tag(&doc, "script");
    let content = doc.text_content(script);
    assert!(content.contains("a < b"));
    assert!(content.contains("</scr"));
    assert_eq!(doc.children(script).len(), 1);
}

#[test]
fn style_content_is_rawtext() {
    assert_eq!(
        head_html("<style>a > b { color: red }</style>"),
        "<style>a &gt; b { color: red }</style>"
    );
    assert_eq!(
        body_html("<body><style>a > b { color: red }</style>"),
        "<style>a &gt; b { color: red }</style>"
    );
}

#[test]
fn xmp_and_iframe_are_rawtext() {
    assert_body("<xmp><p>x</p></xmp>", "<xmp>&lt;p&gt;x&lt;/p&gt;</xmp>");
    assert_body(
        "<iframe><p>x</p></iframe>",
        "<iframe>&lt;p&gt;x&lt;/p&gt;</iframe>",
    );
}

#[test]
fn textarea_decodes_entities_but_not_tags() {
    let doc = parsed("<textarea>a &amp; b <b>x</b></textarea>");
    let textarea = find_tag(&doc, "textarea");
    assert_eq!(doc.text_content(textarea), "a & b <b>x</b>");
    assert!(doc.get_elements_by_tag_name("b").is_empty());
}

#[test]
fn title_is_rcdata_and_decodes_entities() {
    let doc = parsed("<title>a &lt; b &amp; c</title>");
    assert_eq!(doc.title, "a < b & c");
}

#[test]
fn noscript_is_rawtext_with_scripting_enabled() {
    let doc = parsed("<head><noscript><link href=x></noscript></head>");
    let noscript = find_tag(&doc, "noscript");
    assert_eq!(doc.text_content(noscript), "<link href=x>");
    assert!(doc.get_elements_by_tag_name("link").iter().all(|id| {
        // The only link-like content is text, so no element should exist.
        doc.element_name(*id) != Some("link")
    }));
}

#[test]
fn noscript_content_is_parsed_with_scripting_disabled() {
    let result = parse_document_with(
        "<head><noscript><link href=x></noscript></head><p>y</p>",
        &ParseOptions {
            scripting: false,
            ..ParseOptions::default()
        },
    );
    let (head, body) = result.document.head_and_body();
    let head = head.unwrap();
    let body = body.unwrap();
    assert!(serialize_children(&result.document, head).contains("<noscript>"));
    assert!(serialize_children(&result.document, body).contains("<p>y</p>"));
}

// -------------------------------------------------------------------------
// Templates, SVG and MathML
// -------------------------------------------------------------------------

#[test]
fn template_content_is_parsed_and_template_closes() {
    assert_body(
        "<body><template><div>x</div></template><p>y</p>",
        "<template><div>x</div></template><p>y</p>",
    );
}

#[test]
fn template_in_head_restores_head_mode() {
    let doc = parsed("<head><template><p>x</p></template><meta name=a></head><body>y</body>");
    let (head, body) = doc.head_and_body();
    let head = head.unwrap();
    assert!(serialize_children(&doc, head).contains("<template>"));
    assert!(serialize_children(&doc, head).contains("<meta name=\"a\">"));
    assert_eq!(serialize_children(&doc, body.unwrap()), "y");
}

#[test]
fn svg_subtree_uses_the_svg_namespace() {
    let doc = parsed("<svg><circle r=\"1\"/><rect></rect></svg>");
    let svg = find_tag(&doc, "svg");
    assert_eq!(doc.namespace(svg), Some(Namespace::Svg));
    let circle = find_tag(&doc, "circle");
    let rect = find_tag(&doc, "rect");
    assert_eq!(doc.namespace(circle), Some(Namespace::Svg));
    assert_eq!(doc.namespace(rect), Some(Namespace::Svg));
    assert!(doc.children(circle).is_empty());
}

#[test]
fn svg_foreignobject_is_an_html_integration_point() {
    let doc = parsed("<svg><foreignObject><div>x</div></foreignObject></svg>");
    let div = find_tag(&doc, "div");
    assert_eq!(doc.namespace(div), Some(Namespace::Html));
    assert_eq!(doc.text_content(div), "x");
}

#[test]
fn mathml_subtree_uses_the_mathml_namespace() {
    let doc = parsed("<math><mi>x</mi><mtext><b>y</b></mtext></math>");
    let math = find_tag(&doc, "math");
    let mi = find_tag(&doc, "mi");
    assert_eq!(doc.namespace(math), Some(Namespace::MathMl));
    assert_eq!(doc.namespace(mi), Some(Namespace::MathMl));
    let bold = find_tag(&doc, "b");
    assert_eq!(doc.namespace(bold), Some(Namespace::Html));
}

// -------------------------------------------------------------------------
// Entities
// -------------------------------------------------------------------------

#[test]
fn decodes_numeric_decimal_and_hex_references() {
    assert_eq!(text_of("&#65;&#x42;&#X43;"), "ABC");
}

#[test]
fn decodes_common_named_entities() {
    assert_eq!(
        text_of("&copy;&reg;&hellip;&mdash;&times;&divide;&euro;&nbsp;"),
        "\u{a9}\u{ae}\u{2026}\u{2014}\u{d7}\u{f7}\u{20ac}\u{a0}"
    );
}

#[test]
fn decodes_legacy_references_without_semicolons() {
    assert_eq!(text_of("&ampx"), "&x");
    assert_eq!(text_of("&copy 2026"), "\u{a9} 2026");
}

#[test]
fn unknown_references_remain_literal() {
    assert_eq!(text_of("&doesnotexist;"), "&doesnotexist;");
    assert_eq!(text_of("a & b"), "a & b");
}

#[test]
fn numeric_control_references_are_remapped_and_invalid_ones_replaced() {
    assert_eq!(text_of("&#x80;"), "\u{20ac}");
    assert_eq!(text_of("&#0;"), "\u{fffd}");
    assert_eq!(text_of("&#x110000;"), "\u{fffd}");
}

// -------------------------------------------------------------------------
// Malformed input and limits
// -------------------------------------------------------------------------

#[test]
fn unterminated_tag_at_eof_does_not_produce_an_element() {
    let doc = parsed("<div class=\"x\"");
    assert!(doc.get_elements_by_tag_name("div").is_empty());
    assert!(doc.document_element().is_some());
}

#[test]
fn unterminated_comment_is_kept() {
    let doc = parsed("<!-- never closed");
    assert!(
        doc.nodes()
            .any(|node| matches!(&node.data, NodeData::Comment(text) if text == " never closed"))
    );
}

#[test]
fn less_than_at_eof_is_text() {
    assert_body("<", "&lt;");
    assert_body("a < b", "a &lt; b");
}

#[test]
fn duplicate_and_stray_tags_keep_the_tree_well_formed() {
    let doc = parsed("<html><head></head><body></body></html></body></html>");
    let html = doc.document_element().unwrap();
    assert_eq!(doc.element_name(html), Some("html"));
    assert!(doc.head_and_body().1.is_some());
}

#[test]
fn deep_nesting_respects_max_depth_without_panicking() {
    let depth = 400;
    let input = format!("{}x", "<div>".repeat(depth));
    let options = ParseOptions {
        max_depth: 16,
        ..ParseOptions::default()
    };
    let result = parse_document_with(&input, &options);
    assert!(result.document.document_element().is_some());
    assert_eq!(
        result
            .document
            .text_content(result.document.document_element().unwrap()),
        "x"
    );
    assert!(!result.errors.is_empty());
}

#[test]
fn extremely_deep_nesting_is_safe_with_default_options() {
    let input = format!("{}x", "<span>".repeat(5000));
    let doc = parsed(&input);
    assert_eq!(text_of(&input), "x");
    assert!(doc.document_element().is_some());
}

#[test]
fn lossy_byte_input_does_not_panic() {
    let bytes = [0xff, 0xfe, b'<', b'd', b'i', b'v', b'>', 0x80, 0xc3, 0x28];
    let input = String::from_utf8_lossy(&bytes);
    let doc = parsed(&input);
    assert!(doc.document_element().is_some());
}

#[test]
fn pseudo_random_byte_soup_never_panics() {
    // Small deterministic LCG; the point is exercising many state transitions.
    let mut state: u64 = 0x5eed_1234_abcd_0001;
    for _ in 0..300 {
        let mut bytes = Vec::new();
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let length = (state >> 33) as usize % 200;
        for _ in 0..length {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            bytes.push((state >> 40) as u8);
        }
        let input = String::from_utf8_lossy(&bytes);
        let doc = parsed(&input);
        assert!(doc.document_element().is_some());
    }
}

#[test]
fn errors_record_the_tokenizer_position() {
    let result = parse_document_with("<p>ok</p></bogus>", &ParseOptions::default());
    assert!(!result.errors.is_empty());
    assert!(result.errors.iter().any(|error| error.position > 0));
}

// -------------------------------------------------------------------------
// Public builder API and fragments
// -------------------------------------------------------------------------

#[test]
fn tree_builder_can_be_fed_manually() {
    let mut builder = TreeBuilder::new(ParseOptions::default());
    builder.feed(Token::StartTag {
        name: "p".to_string(),
        attributes: Vec::new(),
        self_closing: false,
    });
    builder.feed(Token::Character('h'));
    builder.feed(Token::Text("i".to_string()));
    builder.feed(Token::EndTag {
        name: "p".to_string(),
    });
    let result = builder.finish();
    let body = result.document.head_and_body().1.unwrap();
    assert_eq!(serialize_children(&result.document, body), "<p>hi</p>");
}

#[test]
fn fragment_parsing_returns_detached_nodes() {
    let mut doc = Document::new();
    let body = doc.create_element("body");
    doc.append_child(doc.root(), body);
    let context = doc.create_element("p");
    doc.append_child(body, context);

    let nodes = parse_fragment(&mut doc, context, "<b>x</b> tail");
    assert_eq!(nodes.len(), 2);
    for node in &nodes {
        assert_eq!(doc.parent(*node), None);
    }
    for node in nodes {
        doc.append_child(context, node);
    }
    assert_eq!(serialize_children(&doc, context), "<b>x</b> tail");
}

#[test]
fn fragment_parsing_uses_the_context_insertion_mode() {
    let mut doc = Document::new();
    let body = doc.create_element("body");
    doc.append_child(doc.root(), body);
    let context = doc.create_element("table");
    doc.append_child(body, context);

    let nodes = parse_fragment(&mut doc, context, "<tr><td>x</td></tr>");
    assert_eq!(nodes.len(), 1);
    doc.append_child(context, nodes[0]);
    assert_eq!(
        serialize_children(&doc, context),
        "<tbody><tr><td>x</td></tr></tbody>"
    );
}

#[test]
fn fragment_parsing_in_svg_context_keeps_the_namespace() {
    let mut doc = Document::new();
    let body = doc.create_element("body");
    doc.append_child(doc.root(), body);
    let context = doc.create_element_ns("svg", Namespace::Svg);
    doc.append_child(body, context);

    let nodes = parse_fragment(&mut doc, context, "<circle r=\"1\"/>");
    assert_eq!(nodes.len(), 1);
    assert_eq!(doc.namespace(nodes[0]), Some(Namespace::Svg));
    doc.append_child(context, nodes[0]);
    assert_eq!(doc.attr(nodes[0], "r"), Some("1"));
}

#[test]
fn parse_document_with_and_without_options_agree_on_valid_input() {
    let input =
        "<!doctype html><html><head><title>t</title></head><body><p class=a>x</p></body></html>";
    let a = parse_document(input);
    let b = parse_document_with(input, &ParseOptions::default()).document;
    let body_a = body_of(&a);
    let body_b = body_of(&b);
    assert_eq!(
        serialize_children(&a, body_a),
        serialize_children(&b, body_b)
    );
    assert_eq!(a.title, b.title);
}
