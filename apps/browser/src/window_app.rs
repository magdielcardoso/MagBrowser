//! Native window application (`winit` + `wgpu`).
//!
//! The event loop owns the browser, the chrome state and the GPU presenter.
//! Redraws are requested only when something changed (input, resize, load),
//! and every frame is composed by [`crate::chrome::render`] before being
//! uploaded as a single texture. Platform failures are reported and close the
//! window instead of panicking.
//!
//! Navigation is synchronous in the engine. To keep a heavy page from looking
//! like a frozen window, the loading state is committed and a redraw requested
//! *before* the blocking call, and every exit path (success, error or panic)
//! restores a usable chrome state. Escape returns the window to that state.

use std::sync::Arc;

use browser_core::{Browser, BrowserError, Snapshot};
use browser_gpu::{NativeDevice, NativePresenter};
use browser_layout::{Rect, Size};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::chrome::render::{FramePipeline, PageRenderJob, render_window_with_page};
use crate::chrome::{ChromeLayout, ChromePart, LoadStatus, UiState};
use crate::devtools::{DevTools, LogLevel, RequestStatus, ResourceType};
use crate::target::resolve_target;

/// Runs the native window until it is closed.
pub fn run(
    browser: Browser,
    target: Option<String>,
    width: f32,
    height: f32,
) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|error| format!("event loop: {error}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = WindowApp::new(browser, target, width, height);
    event_loop
        .run_app(&mut app)
        .map_err(|error| format!("event loop: {error}"))?;
    match app.fatal {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// Outcome of one synchronous navigation, including engine panics.
enum NavigationOutcome {
    Loaded(Snapshot),
    Failed(BrowserError),
    Panicked,
}

struct WindowApp {
    browser: Browser,
    ui: UiState,
    chrome: ChromeLayout,
    window: Option<Arc<Window>>,
    _device: Option<NativeDevice>,
    presenter: Option<NativePresenter>,
    pending_target: Option<String>,
    modifiers: ModifiersState,
    cursor: (f32, f32),
    requested: (f32, f32),
    fatal: Option<String>,
    present_error: bool,
    devtools: DevTools,
    last_navigation_ms: u32,
    /// Rasterizes page display lists off the UI thread.
    pipeline: FramePipeline,
    /// True when the page content changed and a fresh frame must be submitted.
    render_dirty: bool,
}

impl WindowApp {
    fn new(browser: Browser, target: Option<String>, width: f32, height: f32) -> Self {
        let requested = (width.max(320.0), height.max(240.0));
        let chrome = ChromeLayout::new(
            Size {
                width: requested.0,
                height: requested.1,
            },
            browser.tabs().len(),
            LoadStatus::Ready,
        );
        Self {
            browser,
            ui: UiState::new(),
            chrome,
            window: None,
            _device: None,
            presenter: None,
            pending_target: target,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            requested,
            fatal: None,
            present_error: false,
            devtools: DevTools::new(),
            last_navigation_ms: 0,
            pipeline: FramePipeline::new(),
            render_dirty: true,
        }
    }

    fn logical_size(&self) -> Size {
        let Some(window) = &self.window else {
            return Size {
                width: self.requested.0,
                height: self.requested.1,
            };
        };
        let scale = window.scale_factor().max(0.5) as f32;
        let physical = window.inner_size();
        Size {
            width: (physical.width as f32 / scale).max(1.0),
            height: (physical.height as f32 / scale).max(1.0),
        }
    }

    fn rebuild_chrome(&mut self) {
        let size = self.logical_size();
        self.chrome = ChromeLayout::new(size, self.browser.tabs().len(), self.ui.status);
    }

    fn refresh(&mut self) {
        self.rebuild_chrome();
        self.render_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.chrome.width, self.chrome.height)
    }

    /// True when a window-space point lands on the docked devtools panel.
    fn pointer_in_devtools(&self, x: f32, y: f32) -> bool {
        self.devtools.open && self.devtools.panel_rect(self.window_rect()).contains(x, y)
    }

    fn current_url(&self) -> String {
        self.browser
            .page()
            .url
            .as_ref()
            .map_or_else(String::new, |url| url.as_str().to_string())
    }

    /// Adds the just-finished navigation to the devtools Network log.
    fn record_navigation(&mut self, url: &str) {
        let status = match self.ui.status {
            LoadStatus::Loading => RequestStatus::Loading,
            LoadStatus::Error => RequestStatus::Error,
            LoadStatus::Ready => RequestStatus::Ready,
        };
        self.devtools.record_request(
            "GET",
            url,
            status,
            self.last_navigation_ms,
            ResourceType::Document,
        );
    }

    fn resize_page(&mut self) {
        let page = self.chrome.page_size();
        self.browser
            .resize(page.width.max(1.0), page.height.max(1.0));
    }

    /// Commits the loading state to the UI and asks for a frame immediately.
    ///
    /// The engine navigation that follows is synchronous, so this is the only
    /// chance to paint "Carregando…" before the event loop is blocked.
    fn begin_navigation(&mut self) {
        self.ui.status = LoadStatus::Loading;
        self.ui.error = None;
        self.rebuild_chrome();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Runs a blocking engine operation without ever leaving the chrome in a
    /// loading state. A panic inside parsing, styling or script execution is
    /// contained here so the window stays usable afterwards.
    fn run_navigation<F>(&mut self, operation: F)
    where
        F: FnOnce(&mut Browser) -> Result<Snapshot, BrowserError>,
    {
        self.begin_navigation();
        let started = std::time::Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation(&mut self.browser)
        }));
        self.last_navigation_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        let outcome = match result {
            Ok(Ok(snapshot)) => NavigationOutcome::Loaded(snapshot),
            Ok(Err(error)) => NavigationOutcome::Failed(error),
            Err(_) => NavigationOutcome::Panicked,
        };
        self.finish_navigation(outcome);
    }

    /// Returns the window to a usable state after any navigation outcome.
    fn finish_navigation(&mut self, outcome: NavigationOutcome) {
        match outcome {
            NavigationOutcome::Loaded(snapshot) => self.navigation_ok(snapshot),
            NavigationOutcome::Failed(error) => self.navigation_error(error),
            NavigationOutcome::Panicked => {
                eprintln!("Mag: navigation panicked; restoring the window state");
                self.devtools
                    .push_diagnostic(LogLevel::Error, "navigation panicked");
                self.ui.status = LoadStatus::Error;
                self.ui.error = Some("A página não pôde ser carregada.".to_string());
                self.ui.address.blur();
                self.refresh();
            }
        }
    }

    /// Escape fallback for an in-flight navigation.
    ///
    /// The engine exposes no cancellation hook, so a blocking parse cannot be
    /// interrupted from here. Dismissing the address bar and restoring the
    /// ready state still leaves the window responsive after a heavy load.
    fn cancel_navigation(&mut self) {
        self.ui.address.blur();
        self.ui.pressed = None;
        self.ui.status = LoadStatus::Ready;
        self.ui.error = None;
        self.refresh();
    }

    fn navigate(&mut self, input: &str) {
        let input = input.trim().to_string();
        if input.is_empty() {
            return;
        }
        let resolved = resolve_target(&input);
        self.run_navigation(|browser| browser.navigate(&resolved));
        self.record_navigation(&resolved);
    }

    fn navigation_ok(&mut self, snapshot: Snapshot) {
        self.ui.status = LoadStatus::Ready;
        self.ui.error = None;
        if snapshot.is_truncated() {
            self.devtools.push_diagnostic(
                LogLevel::Warn,
                format!("navigation truncated: {:?}", snapshot.truncation_reason),
            );
        }
        self.ui.address.blur();
        self.address_sync();
        self.update_title();
        self.refresh();
    }

    fn navigation_error(&mut self, error: BrowserError) {
        self.ui.status = LoadStatus::Error;
        self.ui.error = Some(error.to_string());
        self.devtools
            .push_diagnostic(LogLevel::Error, format!("navigation failed: {error}"));
        eprintln!("Mag: {error}");
        self.refresh();
    }

    fn address_sync(&mut self) {
        self.ui.address.sync(&self.browser);
        if self.ui.address.text().is_empty()
            && let Some(target) = &self.pending_target
        {
            self.ui.address.set_exact(target);
        }
    }

    fn update_title(&mut self) {
        if let Some(window) = &self.window {
            let title = self.browser.tab().title();
            window.set_title(&format!("Mag — {title}"));
        }
    }

    fn click_at(&mut self, x: f32, y: f32) {
        if self.pointer_in_devtools(x, y) {
            let panel = self.devtools.panel_rect(self.window_rect());
            if self.devtools.click(panel, x, y) {
                self.refresh();
                return;
            }
        }
        match self.chrome.hit(x, y) {
            Some(ChromePart::Back) => {
                self.run_navigation(|browser| browser.back());
                let url = self.current_url();
                self.record_navigation(&url);
            }
            Some(ChromePart::Forward) => {
                self.run_navigation(|browser| browser.forward());
                let url = self.current_url();
                self.record_navigation(&url);
            }
            Some(ChromePart::Reload) => {
                self.run_navigation(|browser| browser.reload());
                let url = self.current_url();
                self.record_navigation(&url);
            }
            Some(ChromePart::Address) => {
                self.ui.address.focus();
                self.refresh();
            }
            Some(ChromePart::Tab(index)) => {
                let id = self.browser.tabs().ids().get(index).copied();
                if let Some(id) = id
                    && self.browser.select_tab(id).is_ok()
                {
                    self.ui.status = LoadStatus::Ready;
                    self.address_sync();
                    self.update_title();
                }
                self.refresh();
            }
            Some(ChromePart::CloseTab(index)) => {
                let id = self.browser.tabs().ids().get(index).copied();
                if let Some(id) = id
                    && self.browser.tabs_mut().close(id)
                {
                    self.ui.status = LoadStatus::Ready;
                    self.address_sync();
                    self.update_title();
                }
                self.refresh();
            }
            Some(ChromePart::NewTab) => {
                self.browser.new_tab();
                self.ui.address.set_exact("");
                self.ui.address.focus();
                self.ui.status = LoadStatus::Ready;
                self.refresh();
            }
            Some(ChromePart::Page) => {
                let Some((page_x, page_y)) = self.chrome.page_local(x, y) else {
                    return;
                };
                self.browser.dispatch_pointer_event(page_x, page_y, "click");

                // Submit buttons are real controls: activate their form.
                let submitter = self.browser.page().hit_test_node(page_x, page_y);
                let form = submitter.and_then(|node| {
                    let doc = self.browser.page().document.borrow();
                    let is_button = doc.element_name(node).as_deref() == Some("input")
                        && matches!(
                            doc.attr(node, "type")
                                .unwrap_or("")
                                .to_ascii_lowercase()
                                .as_str(),
                            "" | "submit" | "button"
                        );
                    is_button
                        .then(|| browser_core::form_for_input(&doc, node))
                        .flatten()
                });
                if let Some(form) = form {
                    match self.browser.submit_form_with(form, submitter) {
                        Ok(_) => {
                            self.ui.status = LoadStatus::Ready;
                            self.address_sync();
                            self.update_title();
                            self.record_navigation(&self.current_url());
                            self.refresh();
                        }
                        Err(error) => eprintln!("Mag: {error}"),
                    }
                    return;
                }

                let link_url = self
                    .browser
                    .page()
                    .link_at(page_x, page_y)
                    .map(|url| url.as_str().to_string());
                if link_url.is_some() {
                    self.run_navigation(|browser| {
                        browser
                            .click(page_x, page_y)
                            .expect("link was present just before the click")
                    });
                    let url = link_url.unwrap_or_else(|| self.current_url());
                    self.record_navigation(&url);
                } else {
                    self.ui.address.blur();
                    self.refresh();
                }
            }
            Some(ChromePart::Status) | None => {}
        }
    }

    fn scroll_at(&mut self, x: f32, y: f32, delta_y: f32) {
        let Some((page_x, page_y)) = self.chrome.page_local(x, y) else {
            return;
        };
        if self.browser.scroll_at(page_x, page_y, delta_y) {
            self.render_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    fn on_key(&mut self, event: &winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let command = self.modifiers.super_key() || self.modifiers.control_key();
        let alt = self.modifiers.alt_key();
        let key = &event.logical_key;

        if command && let Key::Character(chars) = key {
            match chars.to_lowercase().as_str() {
                "l" => {
                    self.ui.address.focus();
                    self.refresh();
                    return;
                }
                "r" => {
                    self.run_navigation(|browser| browser.reload());
                    let url = self.current_url();
                    self.record_navigation(&url);
                    return;
                }
                "t" => {
                    self.browser.new_tab();
                    self.ui.address.set_exact("");
                    self.ui.address.focus();
                    self.ui.status = LoadStatus::Ready;
                    self.refresh();
                    return;
                }
                _ => {}
            }
        }

        if alt {
            match key {
                Key::Named(NamedKey::ArrowLeft) => {
                    self.run_navigation(|browser| browser.back());
                    let url = self.current_url();
                    self.record_navigation(&url);
                    return;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    self.run_navigation(|browser| browser.forward());
                    let url = self.current_url();
                    self.record_navigation(&url);
                    return;
                }
                _ => {}
            }
        }

        let toggle_devtools = matches!(key, Key::Named(NamedKey::F12))
            || (command
                && self.modifiers.shift_key()
                && matches!(key, Key::Character(chars) if chars.to_lowercase() == "i"));
        if toggle_devtools {
            self.devtools.toggle();
            self.refresh();
            return;
        }

        if self.devtools.open && matches!(key, Key::Named(NamedKey::Escape)) {
            self.devtools.close();
            self.refresh();
            return;
        }

        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.cancel_navigation();
            return;
        }

        // Enter in a focused form control submits the form, like a browser.
        if !self.ui.address.is_focused() && matches!(key, Key::Named(NamedKey::Enter)) {
            match self.browser.submit_focused_input() {
                Ok(Some(_)) => {
                    self.address_sync();
                    self.update_title();
                    self.ui.status = LoadStatus::Ready;
                    self.refresh();
                }
                Ok(None) => {}
                Err(error) => eprintln!("Mag: {error}"),
            }
            return;
        }

        if !self.ui.address.is_focused() {
            return;
        }
        let mut edited = true;
        match key {
            Key::Named(NamedKey::Backspace) => self.ui.address.backspace(),
            Key::Named(NamedKey::Delete) => self.ui.address.delete(),
            Key::Named(NamedKey::ArrowLeft) => self.ui.address.move_caret(-1),
            Key::Named(NamedKey::ArrowRight) => self.ui.address.move_caret(1),
            Key::Named(NamedKey::Home) => self.ui.address.home(),
            Key::Named(NamedKey::End) => self.ui.address.end(),
            Key::Named(NamedKey::Enter) => {
                let text = self.ui.address.text().to_string();
                self.navigate(&text);
                return;
            }
            _ => {
                if command || alt {
                    edited = false;
                } else if let Some(text) = &event.text {
                    self.ui.address.insert(text);
                } else {
                    edited = false;
                }
            }
        }
        if edited {
            self.refresh();
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y * 48.0,
            MouseScrollDelta::PixelDelta(position) => -position.y as f32,
        };
        if pixels != 0.0 {
            let (x, y) = self.cursor;
            if self.pointer_in_devtools(x, y) {
                self.devtools.scroll(pixels);
                self.refresh();
                return;
            }
            self.scroll_at(x, y, pixels);
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(presenter)) = (self.window.as_ref(), self.presenter.as_mut())
        else {
            return;
        };
        let scale = self
            .window
            .as_ref()
            .map_or(1.0, |window| window.scale_factor()) as f32;
        // Rasterize the page off-thread; the UI thread only composes the cached
        // chrome with the newest completed frame. Submitting never blocks and
        // stale jobs are dropped by the pipeline, so scrolling stays fluid.
        if self.render_dirty {
            self.pipeline
                .submit(PageRenderJob::from_browser(&self.browser, scale));
            self.render_dirty = false;
        }
        let completed = self.pipeline.try_take();
        let page = completed.as_ref().or_else(|| self.pipeline.latest());
        let mut image = render_window_with_page(&self.browser, &self.ui, &self.chrome, scale, page);

        // The worker does not drive the event loop: keep requesting redraws
        // until its newest frame is actually presented, otherwise the window
        // would stay on a stale/blank frame after a navigation or reload.
        if self.pipeline.has_pending() {
            window.request_redraw();
        }
        if self.devtools.open {
            let window = Rect::new(0.0, 0.0, self.chrome.width, self.chrome.height);
            let panel = self.devtools.panel_rect(window);
            self.devtools
                .render(&mut image, &self.browser, panel, scale);
        }
        window.pre_present_notify();
        match presenter.present(&image) {
            Ok(_) => self.present_error = false,
            Err(error) => {
                if !self.present_error {
                    eprintln!("Mag: {error}");
                    self.present_error = true;
                }
            }
        }
    }
}

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Mag")
            .with_inner_size(LogicalSize::new(
                f64::from(self.requested.0),
                f64::from(self.requested.1),
            ))
            .with_min_inner_size(LogicalSize::new(360.0, 240.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fatal = Some(format!("could not create the window: {error}"));
                event_loop.exit();
                return;
            }
        };
        let physical = window.inner_size();
        let device = match NativeDevice::new() {
            Ok(device) => device,
            Err(error) => {
                self.fatal = Some(error.to_string());
                event_loop.exit();
                return;
            }
        };
        let presenter =
            match device.create_presenter(window.clone(), physical.width, physical.height) {
                Ok(presenter) => presenter,
                Err(error) => {
                    self.fatal = Some(error.to_string());
                    event_loop.exit();
                    return;
                }
            };
        self._device = Some(device);
        self.presenter = Some(presenter);
        self.window = Some(window);
        self.rebuild_chrome();
        self.resize_page();
        if let Some(target) = self.pending_target.take() {
            // Navigation is synchronous; the loading state is painted first.
            let resolved = resolve_target(&target);
            self.run_navigation(|browser| browser.navigate(&resolved));
            self.record_navigation(&resolved);
            self.ui.address.set_exact(&target);
            self.ui.address.blur();
            self.update_title();
        } else {
            self.address_sync();
        }
        self.rebuild_chrome();
        if let Some(window) = &self.window {
            window.focus_window();
            window.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(presenter) = self.presenter.as_mut() {
                    presenter.resize(size.width, size.height);
                }
                self.rebuild_chrome();
                self.resize_page();
                self.refresh();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.rebuild_chrome();
                self.resize_page();
                self.refresh();
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self
                    .window
                    .as_ref()
                    .map_or(1.0, |window| window.scale_factor()) as f32;
                let x = position.x as f32 / scale;
                let y = position.y as f32 / scale;
                self.cursor = (x, y);
                let hover = if self.pointer_in_devtools(x, y) {
                    None
                } else {
                    self.chrome
                        .hit(x, y)
                        .filter(|part| !matches!(part, ChromePart::Page | ChromePart::Status))
                };
                if hover != self.ui.hover {
                    self.ui.hover = hover;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let (x, y) = self.cursor;
                match state {
                    ElementState::Pressed => {
                        self.ui.pressed = if self.pointer_in_devtools(x, y) {
                            None
                        } else {
                            self.chrome.hit(x, y).filter(|part| {
                                !matches!(part, ChromePart::Page | ChromePart::Status)
                            })
                        };
                        self.click_at(x, y);
                    }
                    ElementState::Released => {
                        if self.ui.pressed.take().is_some()
                            && let Some(window) = &self.window
                        {
                            window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(&event),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Nothing periodic: the window is redrawn only after input or loads.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> WindowApp {
        WindowApp::new(
            Browser::new(browser_core::Settings::headless()),
            None,
            100.0,
            100.0,
        )
    }

    #[test]
    fn requested_size_is_clamped() {
        let app = app();
        assert_eq!(app.requested, (320.0, 240.0));
        assert!(app.chrome.height >= crate::chrome::CHROME_HEIGHT);
    }

    #[test]
    fn beginning_a_navigation_shows_the_loading_state() {
        let mut app = app();
        let ready_width = app.chrome.status.width;
        app.begin_navigation();
        assert_eq!(app.ui.status, LoadStatus::Loading);
        assert!(
            app.chrome.status.width > ready_width,
            "the loading pill must widen for its longer label"
        );
    }

    #[test]
    fn a_failed_navigation_returns_to_a_usable_state() {
        let mut app = app();
        app.navigate("https://example.invalid/");
        assert_eq!(app.ui.status, LoadStatus::Error);
        assert!(app.ui.error.is_some());
        assert!(!app.ui.address.is_focused());
    }

    #[test]
    fn a_panicking_navigation_does_not_leave_the_window_stuck_loading() {
        let mut app = app();
        app.ui.address.focus();
        app.run_navigation(|_| panic!("simulated engine panic"));
        assert_eq!(app.ui.status, LoadStatus::Error);
        assert!(app.ui.error.is_some());
        assert!(!app.ui.address.is_focused());
    }

    #[test]
    fn escape_restores_the_ready_state() {
        let mut app = app();
        app.ui.status = LoadStatus::Loading;
        app.ui.address.focus();
        app.cancel_navigation();
        assert_eq!(app.ui.status, LoadStatus::Ready);
        assert!(app.ui.error.is_none());
        assert!(!app.ui.address.is_focused());
    }
}
