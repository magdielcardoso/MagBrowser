//! Window lifecycle and event delivery.
//!
//! [`WindowBackend`] is the whole windowing contract the engine needs. A real
//! native backend can implement it later; [`VirtualWindowBackend`] is the
//! headless backend actually used by the headless shell and by tests: it owns
//! real window state, records every request and delivers synthesised events.

use std::collections::{HashMap, VecDeque};

use crate::PlatformError;
use crate::input::KeyEvent;

/// Stable identifier of an open window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

/// Mouse button involved in a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary (usually left) button.
    Left,
    /// Secondary (usually right) button.
    Right,
    /// Middle button / scroll wheel click.
    Middle,
    /// First extra button.
    Back,
    /// Second extra button.
    Forward,
    /// Any other platform-specific button.
    Other(u8),
}

/// Pressed or released state of a mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ButtonState {
    /// The button went down.
    Pressed,
    /// The button went up.
    Released,
}

/// Everything a backend can report to the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowEvent {
    /// The drawable area changed size, in physical pixels.
    Resized {
        /// New width.
        w: u32,
        /// New height.
        h: u32,
    },
    /// The display scale factor changed.
    ScaleFactorChanged {
        /// Ratio of physical to logical pixels.
        scale_factor: f32,
    },
    /// The user asked to close the window (title bar button, OS shortcut).
    CloseRequested,
    /// The window gained or lost focus.
    Focused(bool),
    /// The pointer moved, in logical pixels relative to the window.
    CursorMoved {
        /// Horizontal position.
        x: f32,
        /// Vertical position.
        y: f32,
    },
    /// A mouse button changed state at a position.
    MouseInput {
        /// Button that changed.
        button: MouseButton,
        /// New state.
        state: ButtonState,
        /// Horizontal position.
        x: f32,
        /// Vertical position.
        y: f32,
    },
    /// A wheel or trackpad scroll.
    Wheel {
        /// Horizontal delta (positive scrolls right).
        delta_x: f32,
        /// Vertical delta (positive scrolls down).
        delta_y: f32,
    },
    /// A key was pressed or released.
    Key(KeyEvent),
    /// Committed text input, already translated by the platform IME.
    TextInput(String),
}

/// Parameters for creating a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowOptions {
    /// Window title.
    pub title: String,
    /// Initial width in logical pixels.
    pub width: u32,
    /// Initial height in logical pixels.
    pub height: u32,
    /// Whether the user may resize the window.
    pub resizable: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: String::from("Mag"),
            width: 1024,
            height: 768,
            resizable: true,
        }
    }
}

impl WindowOptions {
    /// Creates options with a title and initial size; the window is resizable.
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            title: title.into(),
            width,
            height,
            resizable: true,
        }
    }

    /// Sets whether the window can be resized.
    pub fn with_resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }
}

/// A request the engine made to a backend, recorded for inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendRequest {
    /// A window was created.
    Create(WindowId),
    /// A redraw was requested.
    Redraw(WindowId),
    /// The window title was changed.
    SetTitle(WindowId, String),
    /// The window was closed.
    Close(WindowId),
}

/// The windowing contract implemented by native and virtual backends.
pub trait WindowBackend {
    /// Creates a window and returns its id.
    fn create(&mut self, options: WindowOptions) -> Result<WindowId, PlatformError>;

    /// Drains and returns all pending events.
    fn poll_events(&mut self) -> Vec<(WindowId, WindowEvent)>;

    /// Asks the backend to schedule a redraw of `id`.
    fn request_redraw(&mut self, id: WindowId);

    /// Changes the window title.
    fn set_title(&mut self, id: WindowId, title: &str) -> Result<(), PlatformError>;

    /// Closes and forgets the window.
    fn close(&mut self, id: WindowId);

    /// Returns the current logical size of `id`, if it is still open.
    fn size(&self, id: WindowId) -> Option<(u32, u32)>;
}

/// State of a window in the headless backend.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtualWindow {
    options: WindowOptions,
}

/// Headless window backend.
///
/// This is not a test mock: it is the backend used when the engine runs without
/// a display. Windows are real entries in the backend's registry with real sizes
/// and titles; events are injected by the embedder (or by tests) through
/// [`VirtualWindowBackend::push_event`], [`VirtualWindowBackend::resize`] and
/// friends. Every request the engine makes is recorded in [`Self::requests`] so
/// a headless harness can assert on it.
#[derive(Debug)]
pub struct VirtualWindowBackend {
    next_id: u64,
    windows: HashMap<WindowId, VirtualWindow>,
    events: VecDeque<(WindowId, WindowEvent)>,
    requests: Vec<BackendRequest>,
}

impl Default for VirtualWindowBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualWindowBackend {
    /// Creates an empty backend with no windows.
    pub fn new() -> Self {
        Self {
            next_id: 1,
            windows: HashMap::new(),
            events: VecDeque::new(),
            requests: Vec::new(),
        }
    }

    /// Every request recorded since the backend was created.
    pub fn requests(&self) -> &[BackendRequest] {
        &self.requests
    }

    /// Number of redraw requests made for any window.
    pub fn redraw_requests(&self) -> usize {
        self.requests
            .iter()
            .filter(|request| matches!(request, BackendRequest::Redraw(_)))
            .count()
    }

    /// Ids of all currently open windows, ascending.
    pub fn open_windows(&self) -> Vec<WindowId> {
        let mut ids: Vec<WindowId> = self.windows.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Whether `id` is open.
    pub fn is_open(&self, id: WindowId) -> bool {
        self.windows.contains_key(&id)
    }

    /// Current title of `id`.
    pub fn title(&self, id: WindowId) -> Option<&str> {
        self.windows
            .get(&id)
            .map(|window| window.options.title.as_str())
    }

    /// Options `id` was created with, including its current size.
    pub fn options(&self, id: WindowId) -> Option<&WindowOptions> {
        self.windows.get(&id).map(|window| &window.options)
    }

    /// Queues an event for `id`, as the embedder would after reading the OS.
    pub fn push_event(&mut self, id: WindowId, event: WindowEvent) -> Result<(), PlatformError> {
        if !self.is_open(id) {
            return Err(PlatformError::UnknownWindow(id));
        }
        self.events.push_back((id, event));
        Ok(())
    }

    /// Resizes `id` and queues the corresponding [`WindowEvent::Resized`].
    pub fn resize(&mut self, id: WindowId, width: u32, height: u32) -> Result<(), PlatformError> {
        let window = self
            .windows
            .get_mut(&id)
            .ok_or(PlatformError::UnknownWindow(id))?;
        window.options.width = width;
        window.options.height = height;
        self.events.push_back((
            id,
            WindowEvent::Resized {
                w: width,
                h: height,
            },
        ));
        Ok(())
    }

    /// Queues a [`WindowEvent::CloseRequested`] for `id`.
    pub fn request_close(&mut self, id: WindowId) -> Result<(), PlatformError> {
        self.push_event(id, WindowEvent::CloseRequested)
    }

    /// Queues a [`WindowEvent::TextInput`] for `id`.
    pub fn type_text(
        &mut self,
        id: WindowId,
        text: impl Into<String>,
    ) -> Result<(), PlatformError> {
        self.push_event(id, WindowEvent::TextInput(text.into()))
    }

    /// Number of queued events not yet drained by [`WindowBackend::poll_events`].
    pub fn pending_event_count(&self) -> usize {
        self.events.len()
    }
}

impl WindowBackend for VirtualWindowBackend {
    fn create(&mut self, options: WindowOptions) -> Result<WindowId, PlatformError> {
        if options.width == 0 || options.height == 0 {
            return Err(PlatformError::InvalidInput(
                "window width and height must be non-zero".to_owned(),
            ));
        }
        let id = WindowId(self.next_id);
        self.next_id += 1;
        self.windows.insert(id, VirtualWindow { options });
        self.requests.push(BackendRequest::Create(id));
        Ok(id)
    }

    fn poll_events(&mut self) -> Vec<(WindowId, WindowEvent)> {
        self.events.drain(..).collect()
    }

    fn request_redraw(&mut self, id: WindowId) {
        if self.is_open(id) {
            self.requests.push(BackendRequest::Redraw(id));
        }
    }

    fn set_title(&mut self, id: WindowId, title: &str) -> Result<(), PlatformError> {
        let window = self
            .windows
            .get_mut(&id)
            .ok_or(PlatformError::UnknownWindow(id))?;
        window.options.title = title.to_owned();
        self.requests
            .push(BackendRequest::SetTitle(id, title.to_owned()));
        Ok(())
    }

    fn close(&mut self, id: WindowId) {
        if self.windows.remove(&id).is_some() {
            self.requests.push(BackendRequest::Close(id));
            self.events.retain(|(queued, _)| *queued != id);
        }
    }

    fn size(&self, id: WindowId) -> Option<(u32, u32)> {
        self.windows
            .get(&id)
            .map(|window| (window.options.width, window.options.height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{KeyEvent, Modifiers, NamedKey};

    fn open(backend: &mut VirtualWindowBackend) -> WindowId {
        backend
            .create(WindowOptions::new("test", 800, 600))
            .expect("window created")
    }

    #[test]
    fn create_returns_unique_ids_and_size() {
        let mut backend = VirtualWindowBackend::new();
        let first = open(&mut backend);
        let second = open(&mut backend);
        assert_ne!(first, second);
        assert_eq!(backend.size(first), Some((800, 600)));
        assert_eq!(backend.open_windows(), vec![first, second]);
        assert_eq!(backend.requests()[0], BackendRequest::Create(first));
    }

    #[test]
    fn zero_sized_window_is_rejected() {
        let mut backend = VirtualWindowBackend::new();
        let result = backend.create(WindowOptions::new("bad", 0, 600));
        assert!(matches!(result, Err(PlatformError::InvalidInput(_))));
        assert!(backend.open_windows().is_empty());
    }

    #[test]
    fn poll_events_returns_injected_events_once() {
        let mut backend = VirtualWindowBackend::new();
        let id = open(&mut backend);
        backend.push_event(id, WindowEvent::Focused(true)).unwrap();
        backend
            .push_event(id, WindowEvent::CursorMoved { x: 3.0, y: 4.0 })
            .unwrap();
        let events = backend.poll_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], (id, WindowEvent::Focused(true)));
        assert_eq!(events[1].1, WindowEvent::CursorMoved { x: 3.0, y: 4.0 });
        assert!(backend.poll_events().is_empty());
    }

    #[test]
    fn resize_updates_size_and_queues_event() {
        let mut backend = VirtualWindowBackend::new();
        let id = open(&mut backend);
        backend.resize(id, 640, 480).unwrap();
        assert_eq!(backend.size(id), Some((640, 480)));
        assert_eq!(
            backend.poll_events(),
            vec![(id, WindowEvent::Resized { w: 640, h: 480 })]
        );
    }

    #[test]
    fn title_changes_are_recorded() {
        let mut backend = VirtualWindowBackend::new();
        let id = open(&mut backend);
        backend.set_title(id, "Example").unwrap();
        assert_eq!(backend.title(id), Some("Example"));
        assert_eq!(
            backend.requests().last(),
            Some(&BackendRequest::SetTitle(id, "Example".to_owned()))
        );
        assert_eq!(
            backend.set_title(WindowId(999), "x"),
            Err(PlatformError::UnknownWindow(WindowId(999)))
        );
    }

    #[test]
    fn redraw_requests_are_recorded_only_for_open_windows() {
        let mut backend = VirtualWindowBackend::new();
        let id = open(&mut backend);
        backend.request_redraw(id);
        backend.request_redraw(WindowId(42));
        assert_eq!(backend.redraw_requests(), 1);
        assert_eq!(backend.requests().last(), Some(&BackendRequest::Redraw(id)));
    }

    #[test]
    fn closing_removes_window_and_queued_events() {
        let mut backend = VirtualWindowBackend::new();
        let id = open(&mut backend);
        let other = open(&mut backend);
        backend.push_event(id, WindowEvent::CloseRequested).unwrap();
        backend
            .push_event(other, WindowEvent::Focused(false))
            .unwrap();
        backend.close(id);
        assert!(!backend.is_open(id));
        assert_eq!(backend.size(id), None);
        assert_eq!(
            backend.poll_events(),
            vec![(other, WindowEvent::Focused(false))]
        );
        assert!(backend.requests().contains(&BackendRequest::Close(id)));
        backend
            .push_event(id, WindowEvent::Focused(true))
            .expect_err("closed windows reject events");
    }

    #[test]
    fn key_and_text_events_round_trip() {
        let mut backend = VirtualWindowBackend::new();
        let id = open(&mut backend);
        let key = KeyEvent {
            key: crate::input::Key::Named(NamedKey::Enter),
            modifiers: Modifiers::NONE,
            repeat: false,
        };
        backend.push_event(id, WindowEvent::Key(key)).unwrap();
        backend.type_text(id, "hi").unwrap();
        let events = backend.poll_events();
        assert_eq!(events[0].1, WindowEvent::Key(key));
        assert_eq!(events[1].1, WindowEvent::TextInput("hi".to_owned()));
    }

    #[test]
    fn options_defaults_are_sensible() {
        let options = WindowOptions::default();
        assert_eq!(options.width, 1024);
        assert_eq!(options.height, 768);
        assert!(options.resizable);
        assert!(
            !WindowOptions::new("x", 10, 20)
                .with_resizable(false)
                .resizable
        );
    }

    #[test]
    fn backend_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<VirtualWindowBackend>();
        assert_send::<WindowEvent>();
        assert_send::<WindowId>();
    }
}
