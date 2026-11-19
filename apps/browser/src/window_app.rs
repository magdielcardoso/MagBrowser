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

use crate::chrome::render::{
    FramePipeline, PageRenderJob, render_context_menu, render_window_with_page_scaled,
};
use crate::chrome::{ChromeLayout, ChromePart, LoadStatus, UiState};
use crate::devtools::{DevTools, LogLevel, RequestStatus, ResourceType};
use crate::target::resolve_target;
use crate::ui_extra::find_overlay;
use crate::ui_extra::{
    ContextAction, ContextMenu, Point as MenuPoint, TabDrag, ZoomController, push_tab, remove_tab,
    reorder_tabs,
};

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
    /// Logical zoom level applied only to the page render scale.
    zoom: ZoomController,
    /// Right-click menu, open until an action or an outside click dismisses it.
    context_menu: Option<ContextMenu>,
    /// Link under the pointer when the context menu opened, if any.
    context_link: Option<String>,
    /// Drag-to-reorder state machine for the tab strip.
    tab_drag: TabDrag,
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
            zoom: ZoomController::new(),
            context_menu: None,
            context_link: None,
            tab_drag: TabDrag::new(),
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
        self.sync_tab_order();
        let size = self.logical_size();
        self.chrome = ChromeLayout::new(size, self.browser.tabs().len(), self.ui.status);
    }

    /// Keeps the display-slot → browser-tab mapping sized to the tab set.
    ///
    /// Close/new paths update the mapping explicitly; this is the safety net
    /// for any tab change made through another entry point.
    fn sync_tab_order(&mut self) {
        let count = self.browser.tabs().len();
        if self.ui.tab_order.len() != count {
            self.ui.tab_order = (0..count).collect();
        }
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
        // Match rectangles are tied to the laid-out text of the old page.
        self.find_page_text_changed();
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

    // -- Find in page -------------------------------------------------------

    /// Re-runs the current query against the active page and recomputes rects.
    fn find_page_text_changed(&mut self) {
        let text = self.browser.page().text();
        self.ui.find.set_page_text(&text);
        self.ui.find_highlights = find_overlay::highlights(self.browser.page(), &self.ui.find);
    }

    /// Opens (or refocuses) the find bar and selects its query field.
    fn open_find(&mut self) {
        self.find_page_text_changed();
        self.ui.address.blur();
        self.ui.find_open = true;
        self.ui.find_focused = true;
        self.refresh();
    }

    /// Hides the find bar and its highlight layer.
    fn close_find(&mut self) {
        self.ui.find_open = false;
        self.ui.find_focused = false;
        self.ui.find_highlights.clear();
        self.refresh();
    }

    fn find_insert(&mut self, text: &str) {
        let mut query = self.ui.find.query().to_string();
        query.push_str(text);
        self.ui.find.set_query(&query, &self.browser.page().text());
        self.ui.find_highlights = find_overlay::highlights(self.browser.page(), &self.ui.find);
        self.refresh();
    }

    fn find_backspace(&mut self) {
        let mut query = self.ui.find.query().to_string();
        query.pop();
        self.ui.find.set_query(&query, &self.browser.page().text());
        self.ui.find_highlights = find_overlay::highlights(self.browser.page(), &self.ui.find);
        self.refresh();
    }

    fn find_navigate(&mut self, forward: bool) {
        if forward {
            self.ui.find.next();
        } else {
            self.ui.find.previous();
        }
        self.ui.find_highlights = find_overlay::highlights(self.browser.page(), &self.ui.find);
        self.refresh();
    }

    // -- Zoom ---------------------------------------------------------------

    /// Applies a zoom step and marks the page image for re-rasterization.
    ///
    /// Only the page render scale changes; the chrome stays at the device pixel
    /// ratio so the output image still matches the window and UI text stays
    /// readable. See [`render_window_with_page_scaled`].
    fn apply_zoom(&mut self, step: i8) {
        let changed = match step {
            1 => self.zoom.zoom_in(),
            -1 => self.zoom.zoom_out(),
            0 => {
                self.zoom.reset();
                true
            }
            _ => false,
        };
        if changed {
            self.render_dirty = true;
            self.refresh();
        }
    }

    // -- Tab strip ----------------------------------------------------------

    /// Display slot under a tab-strip x coordinate.
    fn tab_slot_at(&self, x: f32) -> Option<usize> {
        self.chrome
            .tabs
            .iter()
            .position(|rect| x >= rect.x && x < rect.right())
    }

    /// Activates the browser tab shown in a display slot.
    fn select_tab_slot(&mut self, display_slot: usize) {
        let index = self.ui.tab_index(display_slot);
        let id = self.browser.tabs().ids().get(index).copied();
        if let Some(id) = id
            && self.browser.select_tab(id).is_ok()
        {
            self.ui.status = LoadStatus::Ready;
            self.address_sync();
            self.update_title();
            self.find_page_text_changed();
        }
        self.refresh();
    }

    /// Opens a tab and extends the display mapping.
    fn open_new_tab(&mut self) {
        let index = self.browser.tabs().len();
        self.browser.new_tab();
        self.ui.tab_order = push_tab(&self.ui.tab_order, index);
        self.sync_tab_order();
        self.ui.address.set_exact("");
        self.ui.address.focus();
        self.ui.status = LoadStatus::Ready;
        self.refresh();
    }

    /// Closes a browser tab index and removes it from the display mapping.
    fn close_tab_index(&mut self, index: usize) {
        let Some(id) = self.browser.tabs().ids().get(index).copied() else {
            return;
        };
        if self.browser.tabs_mut().close(id) {
            self.ui.tab_order = remove_tab(&self.ui.tab_order, index);
            self.sync_tab_order();
            self.ui.status = LoadStatus::Ready;
            self.address_sync();
            self.update_title();
            self.find_page_text_changed();
        }
        self.refresh();
    }

    /// Commits the pending tab drag to the display mapping.
    ///
    /// `browser_core::TabSet` has no reorder method, so the committed order is
    /// applied to `UiState::tab_order` and translated at every tab hit test.
    fn commit_tab_drag(&mut self) {
        let Some(source) = self.tab_drag.source() else {
            return;
        };
        let target = self.tab_drag.target().unwrap_or(source);
        self.sync_tab_order();
        if source != target
            && let Some(order) = self.tab_drag.commit(self.browser.tabs().len())
            && let Some(next) = reorder_tabs(&self.ui.tab_order, &order)
        {
            self.ui.tab_order = next;
            self.devtools.push_diagnostic(
                LogLevel::Info,
                format!("tab {source} moved to slot {target}"),
            );
        }
        self.tab_drag.cancel();
        self.ui.tab_drag_source = None;
        self.ui.tab_drag_target = None;
        self.refresh();
    }

    // -- Context menu -------------------------------------------------------

    fn close_context_menu(&mut self) {
        self.context_menu = None;
        self.context_link = None;
        self.ui.context_hover = None;
        self.refresh();
    }

    fn open_context_menu(&mut self, x: f32, y: f32) {
        let link = self
            .chrome
            .page_local(x, y)
            .and_then(|(page_x, page_y)| self.browser.page().link_at(page_x, page_y))
            .map(|url| url.as_str().to_string());
        let width = ContextMenu::DEFAULT_ITEM_WIDTH;
        let rows = ContextAction::ALL.len() as f32;
        let height = ContextMenu::DEFAULT_ITEM_HEIGHT * rows;
        let anchor_x = x.clamp(
            self.chrome.page.x,
            (self.chrome.width - width - 4.0).max(self.chrome.page.x),
        );
        let anchor_y = y.clamp(
            self.chrome.page.y,
            (self.chrome.height - height - 4.0).max(self.chrome.page.y),
        );
        let mut menu = ContextMenu::new(MenuPoint::new(anchor_x, anchor_y));
        if link.is_none() || !self.browser.tab().history.can_go_back() {
            for item in &mut menu.items {
                let unavailable = match item.action {
                    ContextAction::OpenInNewTab | ContextAction::CopyLink => link.is_none(),
                    ContextAction::Back => !self.browser.tab().history.can_go_back(),
                    ContextAction::Reload | ContextAction::Inspect => false,
                };
                item.enabled = !unavailable;
            }
        }
        self.context_link = link;
        self.context_menu = Some(menu);
        self.refresh();
    }

    fn run_context_action(&mut self, action: ContextAction) {
        let link = self.context_link.take();
        self.close_context_menu();
        match action {
            ContextAction::OpenInNewTab => {
                self.open_new_tab();
                if let Some(url) = link {
                    self.navigate(&url);
                }
            }
            ContextAction::CopyLink => {
                // No clipboard backend is linked into this binary; the menu row
                // stays available but only reports the link to the console.
                if let Some(url) = link {
                    self.devtools
                        .push_diagnostic(LogLevel::Info, format!("link: {url}"));
                }
            }
            ContextAction::Back => {
                self.run_navigation(|browser| browser.back());
                let url = self.current_url();
                self.record_navigation(&url);
            }
            ContextAction::Reload => {
                self.run_navigation(|browser| browser.reload());
                let url = self.current_url();
                self.record_navigation(&url);
            }
            ContextAction::Inspect => {
                self.devtools.toggle();
                self.refresh();
            }
        }
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
        if self.ui.find_open {
            match self.chrome.hit(x, y) {
                Some(ChromePart::FindInput) => {
                    self.ui.find_focused = true;
                    self.refresh();
                    return;
                }
                Some(ChromePart::FindClose) => {
                    self.close_find();
                    return;
                }
                Some(ChromePart::FindPrev) => {
                    self.find_navigate(false);
                    return;
                }
                Some(ChromePart::FindNext) => {
                    self.find_navigate(true);
                    return;
                }
                _ => {}
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
            Some(ChromePart::Tab(display_slot)) => self.select_tab_slot(display_slot),
            Some(ChromePart::CloseTab(display_slot)) => {
                self.close_tab_index(self.ui.tab_index(display_slot));
            }
            Some(ChromePart::NewTab) => self.open_new_tab(),
            Some(ChromePart::Page) => {
                let Some((page_x, page_y)) = self.chrome.page_local(x, y) else {
                    return;
                };
                self.browser.dispatch_pointer_event(page_x, page_y, "click");

                // Submit buttons are real controls: activate their form.
                let submitter = self.browser.page().hit_test_node(page_x, page_y);
                let form = submitter.and_then(|node| {
                    let doc = self.browser.page().document.borrow();
                    let is_button = doc.element_name(node) == Some("input")
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
            Some(ChromePart::Status)
            | Some(ChromePart::FindInput)
            | Some(ChromePart::FindClose)
            | Some(ChromePart::FindPrev)
            | Some(ChromePart::FindNext)
            | None => {}
        }
    }

    fn scroll_at(&mut self, x: f32, y: f32, delta_y: f32) {
        let Some((page_x, page_y)) = self.chrome.page_local(x, y) else {
            return;
        };
        if self.browser.scroll_at(page_x, page_y, delta_y) {
            self.render_dirty = true;
            if self.ui.find_open {
                self.ui.find_highlights =
                    find_overlay::highlights(self.browser.page(), &self.ui.find);
            }
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
                    self.open_new_tab();
                    return;
                }
                "f" => {
                    self.open_find();
                    return;
                }
                "+" | "=" => {
                    self.apply_zoom(1);
                    return;
                }
                "-" | "_" => {
                    self.apply_zoom(-1);
                    return;
                }
                "0" => {
                    self.apply_zoom(0);
                    return;
                }
                _ => {}
            }
        }

        if matches!(key, Key::Named(NamedKey::Escape)) && self.context_menu.is_some() {
            self.close_context_menu();
            return;
        }
        if matches!(key, Key::Named(NamedKey::Escape)) && self.ui.find_open {
            self.close_find();
            return;
        }
        if self.ui.find_open && self.ui.find_focused {
            match key {
                Key::Named(NamedKey::Enter) => {
                    self.find_navigate(!self.modifiers.shift_key());
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.find_navigate(true);
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.find_navigate(false);
                    return;
                }
                Key::Named(NamedKey::Backspace) => {
                    self.find_backspace();
                    return;
                }
                _ => {
                    if !command
                        && !alt
                        && let Some(text) = &event.text
                    {
                        self.find_insert(text);
                        return;
                    }
                    if command || alt {
                        return;
                    }
                }
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

        let toggle_devtools = is_devtools_toggle(key, command, self.modifiers.shift_key());
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
        // Zoom multiplies only the page raster scale; the chrome and the output
        // image stay at the device scale so UI text remains readable and the
        // presenter still sees a window-sized texture.
        let page_scale = self.zoom.scale(scale);
        // Rasterize the page off-thread; the UI thread only composes the cached
        // chrome with the newest completed frame. Submitting never blocks and
        // stale jobs are dropped by the pipeline, so scrolling stays fluid.
        if self.render_dirty {
            self.pipeline
                .submit(PageRenderJob::from_browser(&self.browser, page_scale));
            self.render_dirty = false;
        }
        let completed = self.pipeline.try_take();
        let page = completed.as_ref().or_else(|| self.pipeline.latest());
        let mut image = render_window_with_page_scaled(
            &self.browser,
            &self.ui,
            &self.chrome,
            scale,
            page,
            page_scale,
        );

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
        // The context menu is the topmost layer so it stays above devtools.
        if let Some(menu) = &self.context_menu {
            render_context_menu(&mut image, menu, self.ui.context_hover, scale);
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

/// The DevTools shortcut is unchanged by the find bar: `F12` or Ctrl+Shift+I.
fn is_devtools_toggle(key: &Key, command: bool, shift: bool) -> bool {
    matches!(key, Key::Named(NamedKey::F12))
        || (command && shift && matches!(key, Key::Character(chars) if chars.to_lowercase() == "i"))
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
                if self.tab_drag.is_dragging()
                    && y >= self.chrome.tab_strip.y
                    && y < self.chrome.tab_strip.bottom()
                    && let Some(slot) = self.tab_slot_at(x)
                    && self.tab_drag.move_to(slot)
                {
                    self.ui.tab_drag_target = Some(slot);
                    self.refresh();
                }
                let menu_hover = self
                    .context_menu
                    .as_ref()
                    .and_then(|menu| menu.hit_test(MenuPoint::new(x, y)));
                if menu_hover != self.ui.context_hover {
                    self.ui.context_hover = menu_hover;
                    self.refresh();
                }
                let hover = if self.pointer_in_devtools(x, y) || menu_hover.is_some() {
                    None
                } else {
                    self.chrome
                        .hit(x, y)
                        .filter(|part| !matches!(part, ChromePart::Page | ChromePart::Status))
                        .filter(|part| {
                            self.ui.find_open
                                || !matches!(
                                    part,
                                    ChromePart::FindInput
                                        | ChromePart::FindClose
                                        | ChromePart::FindPrev
                                        | ChromePart::FindNext
                                )
                        })
                };
                if hover != self.ui.hover {
                    self.ui.hover = hover;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.cursor;
                match button {
                    MouseButton::Right => {
                        if state == ElementState::Pressed {
                            if self.context_menu.is_some() {
                                self.close_context_menu();
                            } else {
                                self.open_context_menu(x, y);
                            }
                        }
                    }
                    MouseButton::Left => match state {
                        ElementState::Pressed => {
                            let menu_action = self
                                .context_menu
                                .as_ref()
                                .and_then(|menu| menu.hit_test(MenuPoint::new(x, y)));
                            if self.context_menu.is_some() {
                                self.close_context_menu();
                                if let Some(action) = menu_action {
                                    self.run_context_action(action);
                                    return;
                                }
                                // A click outside the menu dismisses it and then
                                // continues to the control underneath.
                            }
                            if self.ui.find_focused
                                && !matches!(
                                    self.chrome.hit(x, y),
                                    Some(
                                        ChromePart::FindInput
                                            | ChromePart::FindClose
                                            | ChromePart::FindPrev
                                            | ChromePart::FindNext
                                    )
                                )
                            {
                                self.ui.find_focused = false;
                            }
                            self.ui.pressed = if self.pointer_in_devtools(x, y) {
                                None
                            } else {
                                self.chrome.hit(x, y).filter(|part| {
                                    !matches!(part, ChromePart::Page | ChromePart::Status)
                                })
                            };
                            // A press on a tab starts a drag; selection happens
                            // on release if the pointer never left the slot.
                            if !self.pointer_in_devtools(x, y)
                                && self.context_menu.is_none()
                                && let Some(ChromePart::Tab(slot)) = self.chrome.hit(x, y)
                                && self.tab_drag.start(slot)
                            {
                                self.ui.tab_drag_source = Some(slot);
                                self.ui.tab_drag_target = Some(slot);
                                self.refresh();
                                return;
                            }
                            self.click_at(x, y);
                        }
                        ElementState::Released => {
                            if self.tab_drag.is_dragging() {
                                let source = self.tab_drag.source();
                                let was_click = source == self.tab_drag.target();
                                self.ui.pressed = None;
                                self.commit_tab_drag();
                                if was_click && let Some(slot) = source {
                                    self.select_tab_slot(slot);
                                }
                            } else if self.ui.pressed.take().is_some()
                                && let Some(window) = &self.window
                            {
                                window.request_redraw();
                            }
                        }
                    },
                    _ => {}
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

    fn app_with_page() -> WindowApp {
        let mut app = app();
        app.browser
            .load_html("<html><body><p>alpha beta alpha</p></body></html>", None)
            .unwrap();
        app.find_page_text_changed();
        app
    }

    #[test]
    fn find_bar_filters_and_navigates_matches() {
        let mut app = app_with_page();
        app.open_find();
        assert!(app.ui.find_open && app.ui.find_focused);

        app.find_insert("alpha");
        assert_eq!(app.ui.find.query(), "alpha");
        assert_eq!(app.ui.find.match_count(), 2);
        assert_eq!(app.ui.find.current_index(), Some(0));
        assert_eq!(app.ui.find_highlights.len(), 2);

        app.find_navigate(true);
        assert_eq!(app.ui.find.current_index(), Some(1));
        assert!(
            app.ui
                .find_highlights
                .iter()
                .any(|highlight| highlight.current)
        );
        app.find_navigate(true);
        assert_eq!(app.ui.find.current_index(), Some(0), "navigation wraps");

        app.close_find();
        assert!(!app.ui.find_open && !app.ui.find_focused);
        assert!(app.ui.find_highlights.is_empty());
    }

    #[test]
    fn zoom_only_changes_the_page_render_scale() {
        let mut app = app();
        assert_eq!(app.zoom.level(), 1.0);
        app.apply_zoom(1);
        assert!(app.render_dirty);
        assert_eq!(app.zoom.level(), 1.1);
        assert!((app.zoom.scale(2.0) - 2.2).abs() < 1e-6);
        app.apply_zoom(0);
        assert_eq!(app.zoom.level(), 1.0);
        assert_eq!(app.zoom.scale(2.0), 2.0);
    }

    #[test]
    fn tab_drag_commits_a_display_order() {
        let mut app = app();
        app.browser.new_tab();
        app.browser.new_tab();
        app.sync_tab_order();
        assert_eq!(app.ui.tab_order, vec![0, 1, 2]);

        assert!(app.tab_drag.start(0));
        assert!(app.tab_drag.move_to(2));
        app.ui.tab_drag_source = Some(0);
        app.ui.tab_drag_target = Some(2);
        app.commit_tab_drag();
        assert_eq!(app.ui.tab_order, vec![1, 2, 0]);
        assert!(app.tab_drag.is_idle());
        assert_eq!(app.ui.tab_index(0), 1);

        let id = app.browser.tabs().ids()[1];
        app.select_tab_slot(0);
        assert_eq!(app.browser.tab().id, id);
    }

    #[test]
    fn context_menu_opens_and_actions_run() {
        let mut app = app_with_page();
        app.open_context_menu(500.0, 300.0);
        let menu = app.context_menu.as_ref().unwrap();
        assert_eq!(menu.item_count(), 5);
        assert!(app.context_link.is_none());

        app.run_context_action(ContextAction::Inspect);
        assert!(app.context_menu.is_none());
        assert!(app.devtools.open);
    }

    #[test]
    fn f12_remains_the_devtools_shortcut() {
        // The find bar handles text keys only; F12 still reaches the DevTools
        // branch of `on_key`.
        assert!(is_devtools_toggle(&Key::Named(NamedKey::F12), false, false));
        assert!(is_devtools_toggle(&Key::Character("I".into()), true, true));
        assert!(!is_devtools_toggle(
            &Key::Character("f".into()),
            true,
            false
        ));
    }
}
