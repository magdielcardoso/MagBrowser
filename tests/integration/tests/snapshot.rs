//! Golden-image regression test: the software renderer is deterministic, so the
//! PNG bytes must match the committed snapshot exactly.

use std::path::PathBuf;

use browser_core::{Browser, Settings};

fn path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn golden_render_matches_committed_snapshot() {
    let html = std::fs::read_to_string(path("tests/fixtures/golden.html")).expect("fixture");
    let snapshot = std::fs::read(path("tests/snapshots/golden.png")).expect("snapshot");

    let settings = Settings {
        viewport_width: 320.0,
        viewport_height: 240.0,
        ..Settings::headless()
    };
    let mut browser = Browser::new(settings);
    browser.load_html(&html, None).unwrap();
    let rendered = browser.render();
    let png = rendered.to_png();

    if png != snapshot {
        let actual = path("tests/snapshots/golden.actual.png");
        let _ = rendered.save_png(&actual);
        panic!(
            "render differs from tests/snapshots/golden.png ({} vs {} bytes); \
             wrote {} for inspection",
            png.len(),
            snapshot.len(),
            actual.display()
        );
    }
}

#[test]
fn snapshot_is_a_valid_png() {
    let snapshot = std::fs::read(path("tests/snapshots/golden.png")).expect("snapshot");
    assert_eq!(
        &snapshot[..8],
        &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
    );
    assert!(browser_paint::ImageData::decode(&snapshot).is_ok());
}
