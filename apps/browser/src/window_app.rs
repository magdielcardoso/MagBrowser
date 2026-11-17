//! Native window application (`winit` + `wgpu`).
//!
//! The event loop owns the browser, the chrome state and the GPU presenter.
//! Redraws are requested only when something changed (input, resize, load),
//! and every frame is composed by [`crate::chrome::render`] before being
//! uploaded as a single texture. Platform failures are reported and close the
//! window instead of panicking.

use std::sync::Arc;

use browser_core::{Browser, BrowserError, Snapshot};
use browser_gpu::{NativeDevice, NativePresenter};
use browser_layout::Size;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::chrome::render::render_window;
use crate::chrome::{ChromeLayout, ChromePart, LoadStatus, UiState};
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
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn resize_page(&mut self) {
        let page = self.chrome.page_size();
        self.browser
            .resize(page.width.max(1.0), page.height.max(1.0));
    }

    fn navigate(&mut self, input: &str) {
        let input = input.trim();
        if input.is_empty() {
            return;
        }
        self.ui.status = LoadStatus::Loading;
        self.ui.error = None;
        let resolved = resolve_target(input);
        match self.browser.navigate(&resolved) {
            Ok(snapshot) => self.navigation_ok(snapshot),
            Err(error) => self.navigation_error(error),
        }
    }

    fn navigation_ok(&mut self, _snapshot: Snapshot) {
        self.ui.status = LoadStatus::Ready;
        self.ui.error = None;
        self.ui.address.blur();
        self.address_sync();
        self.update_title();
        self.refresh();
    }

    fn navigation_error(&mut self, error: BrowserError) {
        self.ui.status = LoadStatus::Error;
        self.ui.error = Some(error.to_string());
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
        match self.chrome.hit(x, y) {
            Some(ChromePart::Back) => {
                if self.browser.back().is_ok() {
                    self.ui.status = LoadStatus::Ready;
                    self.address_sync();
                    self.update_title();
                }
                self.refresh();
            }
            Some(ChromePart::Forward) => {
                if self.browser.forward().is_ok() {
                    self.ui.status = LoadStatus::Ready;
                    self.address_sync();
                    self.update_title();
                }
                self.refresh();
            }
            Some(ChromePart::Reload) => match self.browser.reload() {
                Ok(snapshot) => self.navigation_ok(snapshot),
                Err(error) => self.navigation_error(error),
            },
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
                match self.browser.click(page_x, page_y) {
                    Some(Ok(snapshot)) => self.navigation_ok(snapshot),
                    Some(Err(error)) => self.navigation_error(error),
                    None => {
                        self.ui.address.blur();
                        self.refresh();
                    }
                }
            }
            Some(ChromePart::Status) | None => {}
        }
    }

    fn scroll_at(&mut self, x: f32, y: f32, delta_y: f32) {
        let Some((page_x, page_y)) = self.chrome.page_local(x, y) else {
            return;
        };
        if self.browser.scroll_at(page_x, page_y, delta_y)
            && let Some(window) = &self.window
        {
            window.request_redraw();
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
                    match self.browser.reload() {
                        Ok(snapshot) => self.navigation_ok(snapshot),
                        Err(error) => self.navigation_error(error),
                    }
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
                    if self.browser.back().is_ok() {
                        self.address_sync();
                        self.update_title();
                        self.refresh();
                    }
                    return;
                }
                Key::Named(NamedKey::ArrowRight) => {
                    if self.browser.forward().is_ok() {
                        self.address_sync();
                        self.update_title();
                        self.refresh();
                    }
                    return;
                }
                _ => {}
            }
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
            Key::Named(NamedKey::Escape) => {
                self.ui.address.blur();
            }
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
            self.scroll_at(x, y, pixels);
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(presenter)) = (self.window.as_ref(), self.presenter.as_mut())
        else {
            return;
        };
        let image = render_window(&self.browser, &self.ui, &self.chrome);
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
            // Navigation is synchronous; the first frame appears once loaded.
            self.ui.status = LoadStatus::Loading;
            let resolved = resolve_target(&target);
            match self.browser.navigate(&resolved) {
                Ok(_) => {
                    self.ui.status = LoadStatus::Ready;
                    self.ui.address.set_exact(&target);
                    self.update_title();
                }
                Err(error) => {
                    self.ui.status = LoadStatus::Error;
                    self.ui.error = Some(error.to_string());
                    self.ui.address.set_exact(&target);
                    eprintln!("Mag: {error}");
                }
            }
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
                let hover = self
                    .chrome
                    .hit(x, y)
                    .filter(|part| !matches!(part, ChromePart::Page | ChromePart::Status));
                if hover != self.ui.hover {
                    self.ui.hover = hover;
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (x, y) = self.cursor;
                self.click_at(x, y);
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

    #[test]
    fn requested_size_is_clamped() {
        let app = WindowApp::new(
            Browser::new(browser_core::Settings::headless()),
            None,
            100.0,
            100.0,
        );
        assert_eq!(app.requested, (320.0, 240.0));
        assert!(app.chrome.height >= crate::chrome::CHROME_HEIGHT);
    }
}
