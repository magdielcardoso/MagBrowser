//! Per-stage pipeline timing for the headless renderer.
//!
//! The engine's `Browser::load_html`/`navigate` runs parse, style, layout and
//! display-list construction in one black-box call. To give useful numbers we
//! replay each pure stage on the already-loaded page, timing the same engine
//! primitives the renderer uses. The replay does not change what is rendered;
//! it only measures how long each stage takes on this document.

use std::time::{Duration, Instant};

use browser_core::page::Page;

/// Wall-clock time spent in each pipeline stage of one replay.
#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineTrace {
    /// HTML parsing.
    pub parse: Duration,
    /// Selector matching and cascade.
    pub style: Duration,
    /// Box tree construction and geometry.
    pub layout: Duration,
    /// Display-list construction.
    pub paint: Duration,
}

impl PipelineTrace {
    /// Replays the four stages against `page` and returns their timings.
    ///
    /// `source` is the original HTML text, used only for the parse measurement.
    pub fn replay(page: &Page, source: &str) -> Self {
        let parse = time(|| {
            let _ = browser_html_parser::parse_document(source);
        });

        let viewport = page.viewport();
        let style = time(|| {
            let mut engine = browser_style::StyleEngine::new(browser_style::Viewport {
                width: viewport.width,
                height: viewport.height,
            });
            for css in page.author_css() {
                engine.add_css(css, browser_style::Origin::Author);
            }
            let document = page.document.borrow();
            let _ = engine.compute(&document);
        });

        let layout_started = Instant::now();
        let tree = {
            let document = page.document.borrow();
            browser_layout::layout(&document, &page.styles, viewport)
        };
        let layout = layout_started.elapsed();

        let paint = time(|| {
            let _ = browser_paint::build_with_images(&tree, &page.images);
        });

        Self {
            parse,
            style,
            layout,
            paint,
        }
    }

    /// Writes the trace to standard error, one line per stage plus a total.
    pub fn print(&self) {
        let total = self.total();
        eprintln!("[trace] parse   {:>10}", ms(self.parse));
        eprintln!("[trace] style   {:>10}", ms(self.style));
        eprintln!("[trace] layout  {:>10}", ms(self.layout));
        eprintln!("[trace] paint   {:>10}", ms(self.paint));
        eprintln!("[trace] total   {:>10}", ms(total));
    }

    /// Sum of every stage, exposed for tests and assertions.
    pub fn total(&self) -> Duration {
        self.parse + self.style + self.layout + self.paint
    }
}

fn time<F: FnOnce()>(work: F) -> Duration {
    let started = Instant::now();
    work();
    started.elapsed()
}

fn ms(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::{Browser, Settings};

    #[test]
    fn replay_measures_every_stage() {
        let mut browser = Browser::new(Settings::headless());
        browser
            .load_html(
                "<html><head><style>p{color:red}</style></head><body><p>hi</p></body></html>",
                None,
            )
            .unwrap();
        let trace = PipelineTrace::replay(browser.page(), "<html><body><p>hi</p></body></html>");
        // Timings are wall-clock, but each stage must have been executed.
        assert!(trace.parse < Duration::from_secs(60));
        assert!(trace.total() >= trace.style);
    }
}
