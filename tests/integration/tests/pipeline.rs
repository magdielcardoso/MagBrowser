//! End-to-end pipeline tests: parse → style → layout → paint.

use std::path::PathBuf;

use browser_core::{Browser, Settings};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn browser() -> Browser {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    Browser::new(settings)
}

#[test]
fn renders_the_reference_page() {
    let mut browser = browser();
    let html = fixture("page.html");
    let snapshot = browser.load_html(&html, None).unwrap();

    assert_eq!(snapshot.title, "Mag test page");
    assert!(snapshot.node_count > 20, "nodes: {}", snapshot.node_count);
    assert!(snapshot.content_height > 100.0);
    assert!(snapshot.command_count > 5);

    let image = browser.render();
    assert_eq!(image.width, 1024);
    assert_eq!(image.height, 768);
    // The page has a white background with text and coloured elements.
    assert!(image.coverage_vs(browser_css::Color::rgb(255, 255, 255)) > 0.01);

    // The hidden element must not be rendered.
    let hidden = browser.page().document.borrow().get_element_by_id("hidden");
    assert!(hidden.is_some());
    assert!(browser.page().layout.find_node(hidden.unwrap()).is_none());
}

#[test]
fn rendering_is_deterministic() {
    let mut browser = browser();
    browser.load_html(&fixture("page.html"), None).unwrap();
    let first = browser.render().to_png();
    let second = browser.render().to_png();
    assert_eq!(first, second);
    assert_eq!(
        &first[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );
}

#[test]
fn malformed_html_recovers_and_renders() {
    let mut browser = browser();
    let snapshot = browser.load_html(&fixture("malformed.html"), None).unwrap();
    assert!(snapshot.node_count > 5);
    let image = browser.render();
    assert!(image.coverage_vs(browser_css::Color::rgb(255, 255, 255)) > 0.0005);
}

#[test]
fn flex_row_places_children_horizontally() {
    let mut browser = browser();
    browser
        .load_html(
            "<html><body style='margin:0'>\
             <div id='row' style='display:flex; gap:10px'>\
               <span id='a' style='width:20px;height:10px;background:red'></span>\
               <span id='b' style='width:20px;height:10px;background:blue'></span>\
             </div></body></html>",
            None,
        )
        .unwrap();
    let page = browser.page();
    let document = page.document.borrow();
    let a = document.get_element_by_id("a").unwrap();
    let b = document.get_element_by_id("b").unwrap();
    drop(document);
    let a_box = page.layout.find_node(a).unwrap();
    let b_box = page.layout.find_node(b).unwrap();
    assert!(b_box.rect.x >= a_box.rect.right(), "children overlap");
    assert_eq!(b_box.rect.y, a_box.rect.y);
}

#[test]
fn scripts_mutate_the_dom_and_events_fire() {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    settings.enable_scripting = true;
    let mut browser = Browser::new(settings);
    browser.load_html(&fixture("script.html"), None).unwrap();
    assert!(browser.snapshot().text.contains("script ran"));
    assert!(
        browser
            .console()
            .iter()
            .any(|line| line.contains("script ready"))
    );

    // Clicking the button must run the listener and update the DOM text.
    let document = browser.page().document.borrow();
    let button = document.get_element_by_id("go").unwrap();
    drop(document);
    assert!(browser.tab_mut().page.dispatch_event(button, "click"));
    browser.tab_mut().page.recompute();
    assert!(browser.snapshot().text.contains("clicked 1"));
}

#[test]
fn links_navigate_and_history_works() {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    let mut browser = Browser::new(settings);
    let dir = std::env::temp_dir().join(format!("mag-links-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join("index.html");
    let next = dir.join("next.html");
    std::fs::write(
        &page,
        "<html><head><title>One</title></head><body><a href='next.html'>next</a></body></html>",
    )
    .unwrap();
    std::fs::write(
        &next,
        "<html><head><title>Two</title></head><body><p>second</p></body></html>",
    )
    .unwrap();

    browser
        .navigate(&format!("file://{}", page.display()))
        .unwrap();
    assert_eq!(browser.snapshot().title, "One");

    let anchor_rect = {
        let page = browser.page();
        let document = page.document.borrow();
        let anchor = document.get_elements_by_tag_name("a").remove(0);
        let text = document.first_child(anchor).unwrap();
        drop(document);
        page.layout
            .find_node(text)
            .map(|b| b.rect)
            .expect("text box")
    };
    let followed = browser
        .click(anchor_rect.x + 1.0, anchor_rect.y + 1.0)
        .expect("link under the pointer")
        .expect("navigation succeeds");
    assert_eq!(followed.title, "Two");
    assert!(browser.tab().history.can_go_back());

    let back = browser.back().unwrap();
    assert_eq!(back.title, "One");
    let forward = browser.forward().unwrap();
    assert_eq!(forward.title, "Two");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn external_scripts_are_loaded_and_executed() {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    settings.enable_scripting = true;
    let mut browser = Browser::new(settings);

    let dir = std::env::temp_dir().join(format!("mag-ext-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("app.js"),
        "document.getElementById('out').textContent = 'external ran';",
    )
    .unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<html><head><title>Ext</title></head><body>\
         <div id='out'>before</div><script src='app.js'></script></body></html>",
    )
    .unwrap();

    browser
        .navigate(&format!("file://{}", dir.join("index.html").display()))
        .unwrap();
    assert!(browser.snapshot().text.contains("external ran"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn decoded_images_are_painted() {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    let mut browser = Browser::new(settings);

    let dir = std::env::temp_dir().join(format!("mag-img-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 4x4 solid red PNG produced by the engine's own encoder.
    let mut image = browser_paint::ImageBuffer::new(4, 4);
    for index in (0..image.pixels.len()).step_by(4) {
        image.pixels[index] = 255;
        image.pixels[index + 3] = 255;
    }
    image.save_png(dir.join("red.png")).unwrap();

    std::fs::write(
        dir.join("index.html"),
        "<html><body style='margin:0'>\
         <img src='red.png' style='width:32px;height:32px;display:block'>\
         </body></html>",
    )
    .unwrap();

    browser
        .navigate(&format!("file://{}", dir.join("index.html").display()))
        .unwrap();
    assert_eq!(browser.page().images.len(), 1);
    let rendered = browser.render();
    assert_eq!(rendered.pixel(16, 16), [255, 0, 0, 255]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timers_run_and_mutate_the_dom() {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    settings.enable_scripting = true;
    let mut browser = Browser::new(settings);
    browser
        .load_html(
            "<html><body><div id='t'>start</div>\
             <script>setTimeout(function () { document.getElementById('t').textContent = 'later'; }, 0);</script>\
             </body></html>",
            None,
        )
        .unwrap();
    assert!(browser.snapshot().text.contains("later"));
}

#[test]
fn inner_html_is_parsed_into_dom() {
    let mut settings = Settings::headless();
    settings.enable_network = false;
    settings.enable_scripting = true;
    let mut browser = Browser::new(settings);
    browser
        .load_html(
            "<html><body><div id='host'></div>\
             <script>\
               document.getElementById('host').innerHTML = \"<b id='kid'>bold text</b>\";\
             </script></body></html>",
            None,
        )
        .unwrap();
    let page = browser.page();
    let document = page.document.borrow();
    assert!(document.get_element_by_id("kid").is_some());
    drop(document);
    assert!(browser.snapshot().text.contains("bold text"));
}

#[test]
fn location_assignment_navigates_to_another_document() {
    let dir = std::env::temp_dir().join(format!("mag-nav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("one.html"),
        "<html><head><title>One</title></head><body>\
         <script>location.href = 'two.html';</script></body></html>",
    )
    .unwrap();
    std::fs::write(
        dir.join("two.html"),
        "<html><head><title>Two</title></head><body><p>second</p></body></html>",
    )
    .unwrap();

    let mut settings = Settings::headless();
    settings.enable_network = false;
    settings.enable_scripting = true;
    let mut browser = Browser::new(settings);
    browser
        .navigate(&format!("file://{}", dir.join("one.html").display()))
        .unwrap();
    assert_eq!(browser.snapshot().title, "Two");
    assert!(browser.snapshot().text.contains("second"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tables_lay_cells_out_in_columns() {
    let mut browser = browser();
    let snapshot = browser.load_html(&fixture("table.html"), None).unwrap();
    assert!(snapshot.node_count > 10);

    let page = browser.page();
    let document = page.document.borrow();
    let cells = document.get_elements_by_tag_name("td");
    drop(document);
    assert!(cells.len() >= 6);

    let first = page.layout.find_node(cells[0]).unwrap().rect;
    let second = page.layout.find_node(cells[1]).unwrap().rect;
    let third = page.layout.find_node(cells[2]).unwrap().rect;
    assert_eq!(first.y, second.y, "cells of one row share a line");
    assert!(second.x >= first.right());
    assert!(third.x >= second.right());
    let body_right = {
        let document = page.document.borrow();
        let body = document.head_and_body().1.unwrap();
        drop(document);
        page.layout.find_node(body).unwrap().content.right()
    };
    assert!(
        (third.right() - body_right).abs() < 8.0,
        "table fills the body: {} vs {}",
        third.right(),
        body_right
    );
}
