//! Off-thread page rasterization pipeline.
//!
//! The window formerly rasterized the page on the UI thread on every redraw.
//! This module snapshots the render inputs into a [`PageRenderJob`] (all owned,
//! `Send` data), hands it to a single worker thread, and lets the UI pick up the
//! newest completed frame without ever blocking.
//!
//! Decoded images do not need a separate snapshot: [`DisplayList`] already
//! embeds them in `DisplayList::images`, so cloning the display list captures
//! both the paint commands and the images they reference.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use browser_core::{Browser, Page};
use browser_css::Color;
use browser_gpu::{BlendMode, DirtyRect, LayerFrame, LayerTransform};
use browser_layout::{Rect, Size};
use browser_paint::raster::render_cached;
use browser_paint::{DisplayList, ImageBuffer};

/// A self-contained snapshot of everything needed to rasterize one page frame.
///
/// Produced on the UI thread from a borrowed [`Page`], then moved to the worker
/// thread where [`PageRenderJob::render`] turns it into an [`ImageBuffer`].
#[derive(Debug, Clone)]
pub struct PageRenderJob {
    display_list: DisplayList,
    viewport: Size,
    scale: f32,
    background: Color,
}

impl PageRenderJob {
    /// Builds a job from its raw inputs.
    pub fn new(display_list: DisplayList, viewport: Size, scale: f32, background: Color) -> Self {
        Self {
            display_list,
            viewport,
            scale,
            background,
        }
    }

    /// Snapshots a page, using `background` as the rasterizer's clear colour.
    ///
    /// `browser.settings().background_color()` is the usual source for the
    /// background; the page's own background is not exposed by `browser-core`.
    pub fn from_page(page: &Page, background: Color, scale: f32) -> Self {
        Self::new(
            page.display_list.clone(),
            page.viewport(),
            scale,
            background,
        )
    }

    /// Snapshots the browser's active page at `scale` device pixels per CSS pixel.
    pub fn from_browser(browser: &Browser, scale: f32) -> Self {
        Self::from_page(browser.page(), browser.settings().background_color(), scale)
    }

    /// Rasterizes the snapshot.
    ///
    /// This mirrors `Page::render_scaled`: the CSS viewport is multiplied by
    /// `scale` to size the device canvas, then every display-list coordinate is
    /// scaled by the same factor.
    pub fn render(self) -> ImageBuffer {
        let viewport = Size {
            width: (self.viewport.width * self.scale).max(1.0),
            height: (self.viewport.height * self.scale).max(1.0),
        };
        render_cached(&self.display_list, viewport, self.background, self.scale)
    }
}

/// Worker loop: render jobs as they arrive, keeping only the newest queued one.
///
/// While a job is being rasterized, newly submitted jobs pile up in the channel.
/// Before the next render we drain that queue and discard everything but the
/// last job, so scrolling never lags behind a backlog of stale frames.
fn worker(submit: Receiver<PageRenderJob>, results: Sender<ImageBuffer>) {
    while let Ok(mut job) = submit.recv() {
        while let Ok(newer) = submit.try_recv() {
            job = newer;
        }
        let image = job.render();
        if results.send(image).is_err() {
            break;
        }
    }
}

/// A single-worker rendering pipeline with drop-stale-queues semantics.
///
/// All public methods are non-blocking with respect to rasterization: `submit`
/// writes to an unbounded channel and `try_take` only polls for finished work.
pub struct FramePipeline {
    submit: Sender<PageRenderJob>,
    results: Receiver<ImageBuffer>,
    latest: Option<ImageBuffer>,
    /// Jobs submitted but not yet received — used by the window to keep
    /// requesting redraws until the newest frame is actually presented.
    pending: usize,
    _worker: Option<JoinHandle<()>>,
}

impl FramePipeline {
    /// Starts the worker thread.
    pub fn new() -> Self {
        let (submit_tx, submit_rx) = mpsc::channel();
        let (results_tx, results_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("mag-frame-worker".to_string())
            .spawn(move || worker(submit_rx, results_tx))
            .expect("spawn frame worker");
        Self {
            submit: submit_tx,
            results: results_rx,
            latest: None,
            pending: 0,
            _worker: Some(handle),
        }
    }

    /// Queues a job for rasterization. Never blocks on the worker.
    pub fn submit(&mut self, job: PageRenderJob) {
        // The channel is unbounded, so send cannot wait for the worker.
        if self.submit.send(job).is_ok() {
            self.pending += 1;
        }
    }

    /// True while a submitted frame has not been received yet.
    pub fn has_pending(&self) -> bool {
        self.pending > 0
    }

    /// Polls for a completed frame, returning the newest one if any arrived.
    ///
    /// Stale completed frames are drained and discarded; the returned frame is
    /// also remembered as the pipeline's [`Self::latest`].
    pub fn try_take(&mut self) -> Option<ImageBuffer> {
        let mut newest = None;
        while let Ok(image) = self.results.try_recv() {
            newest = Some(image);
        }
        let received = if newest.is_some() { 1 } else { 0 };
        self.pending = self.pending.saturating_sub(received);
        let image = newest?;
        self.latest = Some(image.clone());
        Some(image)
    }

    /// The last completed frame, if one has been taken, without cloning it.
    pub fn latest(&self) -> Option<&ImageBuffer> {
        self.latest.as_ref()
    }
}

impl Default for FramePipeline {
    fn default() -> Self {
        Self::new()
    }
}

// -- GPU layer presentation -------------------------------------------------

/// The GPU-free view of a compositor snapshot needed to build a layer frame.
///
/// The window maps `LayerTree::composite_layers()` into these values; keeping
/// this struct here means the pipeline never has to depend on the compositor
/// crate, and the mapping is easy to unit-test without a GPU adapter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerView {
    /// Stable compositor layer id.
    pub id: u64,
    /// Unscrolled layer bounds in document coordinates.
    pub bounds: Rect,
    /// Opacity reported by the compositor snapshot.
    pub opacity: f32,
}

/// One layer's owned pixel crop, ready to be wrapped in a [`LayerFrame`].
#[derive(Debug, Clone)]
pub struct LayerData {
    /// Stable compositor layer id.
    pub id: u64,
    /// Tightly packed RGBA pixels of the crop.
    pub pixels: Vec<u8>,
    /// Crop width in device pixels.
    pub width: u32,
    /// Crop height in device pixels.
    pub height: u32,
    /// Destination rectangle in device pixels.
    pub dest: Rect,
    /// Opacity applied at presentation time.
    pub opacity: f32,
    /// Opacity reported by the compositor, retained for diagnostics and for a
    /// future path that rasterizes layers independently.
    pub snapshot_opacity: f32,
}

/// Clips `rect` to `bounds`, returning an empty rectangle when disjoint.
fn clamp_rect(rect: Rect, bounds: Rect) -> Rect {
    let x = rect.x.max(bounds.x);
    let y = rect.y.max(bounds.y);
    let right = rect.right().min(bounds.right());
    let bottom = rect.bottom().min(bounds.bottom());
    Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
}

/// Plans GPU layer frames from compositor snapshots and a finished window image.
///
/// The software rasterizer has already blended every compositor layer into
/// `image`. The bottom-most snapshot therefore supplies the full frame, and
/// each remaining snapshot supplies a crop of that same image at its visible
/// bounds. Re-drawing identical pixels is a visual no-op, which keeps the GPU
/// path pixel-identical to `present(&ImageBuffer)` while exercising the real
/// multi-layer upload/composite code. `opacity` is deliberately 1.0: applying
/// the compositor's opacity again would darken already-composited pixels.
///
/// `scale` is the device pixel ratio and `zoom` the logical page zoom; together
/// they map document coordinates in `bounds` onto device pixels in `image`.
pub fn plan_layers(
    image: &ImageBuffer,
    layers: impl IntoIterator<Item = LayerView>,
    page: Rect,
    scale: f32,
    zoom: f32,
) -> Vec<LayerData> {
    let full = Rect::new(0.0, 0.0, image.width as f32, image.height as f32);
    let mut plans = Vec::new();
    for (index, layer) in layers.into_iter().enumerate() {
        let requested = if index == 0 {
            full
        } else {
            let bounds = layer.bounds;
            Rect::new(
                (page.x + bounds.x * zoom) * scale,
                (page.y + bounds.y * zoom) * scale,
                (bounds.width * zoom * scale).max(0.0),
                (bounds.height * zoom * scale).max(0.0),
            )
        };
        let dest = clamp_rect(requested, full);
        if dest.is_empty() {
            continue;
        }
        let x = (dest.x.round().max(0.0) as u32).min(image.width.saturating_sub(1));
        let y = (dest.y.round().max(0.0) as u32).min(image.height.saturating_sub(1));
        let width = (dest.width.round().max(1.0) as u32).min(image.width - x);
        let height = (dest.height.round().max(1.0) as u32).min(image.height - y);
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for row in y..y + height {
            let start = (row as usize * image.width as usize + x as usize) * 4;
            pixels.extend_from_slice(&image.pixels[start..start + width as usize * 4]);
        }
        plans.push(LayerData {
            id: layer.id,
            pixels,
            width,
            height,
            dest: Rect::new(x as f32, y as f32, width as f32, height as f32),
            opacity: 1.0,
            snapshot_opacity: layer.opacity,
        });
    }
    plans
}

/// Wraps planned layer data into GPU frames, in back-to-front slice order.
pub fn as_layer_frames(plans: &[LayerData]) -> Vec<LayerFrame<'_>> {
    plans
        .iter()
        .map(|plan| LayerFrame {
            id: plan.id,
            pixels: &plan.pixels,
            width: plan.width,
            height: plan.height,
            dest: plan.dest,
            opacity: plan.opacity,
            transform: LayerTransform::from_rect(plan.width, plan.height, plan.dest),
            blend: BlendMode::Normal,
            dirty: Some(DirtyRect::new(0, 0, plan.width, plan.height)),
            content_hash: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::Settings;
    use std::time::{Duration, Instant};

    fn browser_with(html: &str) -> Browser {
        let mut browser = Browser::new(Settings::headless());
        browser.load_html(html, None).unwrap();
        browser
    }

    fn wait_for_frame(pipeline: &mut FramePipeline, timeout: Duration) -> ImageBuffer {
        let start = Instant::now();
        loop {
            if let Some(image) = pipeline.try_take() {
                return image;
            }
            assert!(
                start.elapsed() < timeout,
                "worker did not produce a frame in time"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn job_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<PageRenderJob>();
    }

    #[test]
    fn job_render_matches_page_render_scaled() {
        let browser = browser_with(
            "<html><head><title>T</title></head>\
             <body style='background:#fff;margin:0'><h1>Hello</h1><p>content</p></body></html>",
        );
        for scale in [1.0_f32, 1.5, 2.0] {
            let expected = browser.page().render_scaled(scale);
            let actual = PageRenderJob::from_browser(&browser, scale).render();
            assert_eq!(actual, expected, "render mismatch at scale {scale}");
        }
    }

    #[test]
    fn submit_and_try_take_are_non_blocking() {
        let browser = browser_with("<html><body><p>hi</p></body></html>");
        let job = PageRenderJob::from_browser(&browser, 1.0);
        let expected = browser.page().render_scaled(1.0);
        let mut pipeline = FramePipeline::new();

        assert!(pipeline.latest().is_none());
        let start = Instant::now();
        assert!(pipeline.try_take().is_none());
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "try_take blocked waiting for a frame"
        );

        let start = Instant::now();
        for _ in 0..200 {
            pipeline.submit(job.clone());
        }
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "submit blocked on rasterization"
        );

        let image = wait_for_frame(&mut pipeline, Duration::from_secs(10));
        assert_eq!(image, expected);
        assert_eq!(pipeline.latest(), Some(&expected));
    }

    #[test]
    fn newest_submitted_job_wins() {
        let first = browser_with("<html><body style='background:#ff0000'></body></html>");
        let second = browser_with("<html><body style='background:#0000ff'></body></html>");
        let mut pipeline = FramePipeline::new();

        pipeline.submit(PageRenderJob::from_browser(&first, 1.0));
        pipeline.submit(PageRenderJob::from_browser(&second, 1.0));

        wait_for_frame(&mut pipeline, Duration::from_secs(10));
        let expected = second.page().render_scaled(1.0);
        assert_eq!(pipeline.latest(), Some(&expected));
    }

    #[test]
    fn layer_plan_builds_frames_from_compositor_views() {
        let image = ImageBuffer::filled(8, 6, Color::rgb(20, 40, 60));
        let views = [
            LayerView {
                id: 0,
                bounds: Rect::new(0.0, 0.0, 8.0, 6.0),
                opacity: 1.0,
            },
            LayerView {
                id: 1,
                bounds: Rect::new(2.0, 1.0, 3.0, 2.0),
                opacity: 0.5,
            },
        ];
        let plans = plan_layers(&image, views, Rect::new(0.0, 0.0, 8.0, 6.0), 1.0, 1.0);
        assert_eq!(plans.len(), 2);
        // The bottom layer carries the whole composed frame.
        assert_eq!((plans[0].width, plans[0].height), (8, 6));
        assert_eq!(plans[0].pixels, image.pixels);
        // The overlay carries only its visible bounds, already composited.
        assert_eq!((plans[1].width, plans[1].height), (3, 2));
        assert_eq!((plans[1].dest.x, plans[1].dest.y), (2.0, 1.0));
        assert_eq!(plans[1].opacity, 1.0);
        assert_eq!(plans[1].snapshot_opacity, 0.5);

        let frames = as_layer_frames(&plans);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].id, 1);
        assert_eq!(frames[1].pixels.len(), 3 * 2 * 4);
        assert!(frames[1].transform.is_finite());
        assert!(!frames[1].is_empty());
    }

    #[test]
    fn empty_compositor_falls_back_to_a_single_frame() {
        let image = ImageBuffer::filled(2, 2, Color::rgb(1, 2, 3));
        assert!(plan_layers(&image, [], Rect::new(0.0, 0.0, 2.0, 2.0), 1.0, 1.0).is_empty());
    }
}
