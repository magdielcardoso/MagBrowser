//! End-to-end regression tests for the "modern CSS" work being landed by the
//! engine agents. Every test here drives the full pipeline (parse → style →
//! layout → paint) through `browser_core::Browser` and asserts observable
//! behaviour (box geometry, decoded pixels or visible text), so the tests keep
//! compiling while the individual features are still being implemented.
//!
//! Covered features: `calc()` percentages, anti-aliased `border-radius`,
//! wrapping flex rows, `aspect-ratio` sizing and `input[value]` labels.

use std::path::PathBuf;

use browser_core::{Browser, Settings};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn browser() -> Browser {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    Browser::new(settings)
}

/// Finds the layout box of an element by id and returns its border-box rect.
fn rect_of(browser: &Browser, id: &str) -> browser_layout::Rect {
    let page = browser.page();
    let document = page.document.borrow();
    let node = document
        .get_element_by_id(id)
        .unwrap_or_else(|| panic!("no element #{id}"));
    drop(document);
    page.layout
        .find_node(node)
        .unwrap_or_else(|| panic!("no layout box for #{id}"))
        .rect
}

// ---------------------------------------------------------------------------
// 1. calc() lengths
// ---------------------------------------------------------------------------

#[test]
fn calc_width_resolves_percentage_minus_length() {
    let mut browser = browser();
    browser
        .load_html(&fixture("calc_width.html"), None)
        .unwrap();

    let parent = rect_of(&browser, "parent");
    let child = rect_of(&browser, "child");

    assert_eq!(parent.width, 200.0, "fixture parent must be 200px wide");
    // width: calc(100% - 20px) of a 200px parent -> 180px.
    assert!(
        (child.width - 180.0).abs() < 0.5,
        "calc(100% - 20px) should be 180px, got {} (parent {})",
        child.width,
        parent.width
    );
}

// ---------------------------------------------------------------------------
// 2. border-radius -> anti-aliased corners in the rendered PNG
// ---------------------------------------------------------------------------

#[test]
fn border_radius_produces_antialiased_corners_in_png() {
    let mut browser = browser();
    browser
        .load_html(&fixture("border_radius.html"), None)
        .unwrap();

    // Round-trip through the PNG encoder/decoder: the assertion is about the
    // pixels that actually end up in the rendered image.
    let png = browser.render().to_png();
    let image = browser_paint::ImageData::decode(&png).expect("decoded render PNG");

    let white: [u8; 4] = [255, 255, 255, 255];
    let red: [u8; 4] = [255, 0, 0, 255];
    let mut blended = 0usize;
    for y in 0..image.height {
        for x in 0..image.width {
            let p = image.get(x, y);
            if p != white && p != red {
                blended += 1;
            }
        }
    }

    assert!(
        blended > 0,
        "rounded corners must blend edge pixels between red and white; \
         found no intermediate pixels (corners look square)"
    );
}

// ---------------------------------------------------------------------------
// 3. flex-wrap: wrap -> multiple lines (fragment y positions)
// ---------------------------------------------------------------------------

#[test]
fn flex_wrap_places_items_on_multiple_lines() {
    let mut browser = browser();
    browser.load_html(&fixture("flex_wrap.html"), None).unwrap();

    let a = rect_of(&browser, "a");
    let b = rect_of(&browser, "b");
    let c = rect_of(&browser, "c");

    // First two items share the first line; the third wraps to the next one.
    assert!(
        (a.y - b.y).abs() < 0.5,
        "first two items should share a line: {} vs {}",
        a.y,
        b.y
    );
    assert!(
        c.y > a.y + 10.0,
        "third item should wrap onto a lower line: first y={}, third y={}",
        a.y,
        c.y
    );
}

// ---------------------------------------------------------------------------
// 4. aspect-ratio sizes an image that only specifies a width
// ---------------------------------------------------------------------------

#[test]
fn aspect_ratio_sets_image_height_from_width() {
    let dir = std::env::temp_dir().join(format!("mag-aspect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // A square (40x40) source image. The declared 2:1 aspect ratio in the
    // fixture must win over the intrinsic 1:1 ratio.
    let source = browser_paint::ImageBuffer::filled(40, 40, browser_css::Color::rgb(255, 0, 0));
    source.save_png(dir.join("photo.png")).unwrap();
    std::fs::write(dir.join("index.html"), fixture("aspect_ratio.html")).unwrap();

    let mut browser = browser();
    browser
        .navigate(&format!("file://{}", dir.join("index.html").display()))
        .unwrap();

    let image = rect_of(&browser, "pic");
    assert_eq!(image.width, 100.0, "declared width must be applied");
    assert!(
        (image.height - 50.0).abs() < 0.5,
        "aspect-ratio 2/1 with width 100px should give height 50px, got {}",
        image.height
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 5. <input value> text appears in the rendered page
// ---------------------------------------------------------------------------

#[test]
fn input_value_text_is_visible_in_rendered_page() {
    const VALUE: &str = "Hello from input value";

    let mut browser = browser();
    browser
        .load_html(&fixture("input_value.html"), None)
        .unwrap();

    // The value lives in the attribute, not as a text child, so the engine has
    // to surface it as laid-out text.
    let text = browser.snapshot().text;
    assert!(
        text.contains(VALUE),
        "input value text missing from rendered page text: {text:?}"
    );

    // It must also reach layout as real text fragments, not just a DOM
    // attribute. Word-level shaping may split the value across fragments, so
    // join them the same way the page's visible text does before matching.
    let fragment_text = browser
        .page()
        .layout
        .text_runs()
        .iter()
        .map(|run| run.fragment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        fragment_text.contains(VALUE),
        "input value should produce laid-out text fragments, got {fragment_text:?}"
    );
}
