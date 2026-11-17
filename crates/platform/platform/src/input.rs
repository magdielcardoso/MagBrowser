//! Keyboard and pointer input state.
//!
//! The platform layer delivers raw [`WindowEvent`]s; [`InputState`] tracks what
//! is currently held down and recognises double/triple clicks. Text translation
//! from a [`KeyEvent`] is deliberately simple: the platform normally sends
//! [`WindowEvent::TextInput`] for real text entry, and [`KeyEvent::text`] covers
//! printable ASCII for synthetic input.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::window::{ButtonState, MouseButton, WindowEvent};

/// Keyboard modifier state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift is held.
    pub shift: bool,
    /// Control is held.
    pub ctrl: bool,
    /// Alt / Option is held.
    pub alt: bool,
    /// Meta / Command / Windows is held.
    pub meta: bool,
}

impl Modifiers {
    /// No modifiers held.
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    /// Builds a modifier state from its four flags.
    pub const fn new(shift: bool, ctrl: bool, alt: bool, meta: bool) -> Self {
        Self {
            shift,
            ctrl,
            alt,
            meta,
        }
    }

    /// Whether any modifier is held.
    pub const fn is_empty(self) -> bool {
        !self.shift && !self.ctrl && !self.alt && !self.meta
    }
}

/// A non-character key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKey {
    /// Enter / Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete (forward delete).
    Delete,
    /// Escape.
    Escape,
    /// Space bar (without a produced character).
    Space,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Insert.
    Insert,
    /// Shift.
    Shift,
    /// Control.
    Control,
    /// Alt / Option.
    Alt,
    /// Meta / Command / Windows.
    Meta,
    /// Function key F1.
    F1,
    /// Function key F2.
    F2,
    /// Function key F3.
    F3,
    /// Function key F4.
    F4,
    /// Function key F5.
    F5,
    /// Function key F6.
    F6,
    /// Function key F7.
    F7,
    /// Function key F8.
    F8,
    /// Function key F9.
    F9,
    /// Function key F10.
    F10,
    /// Function key F11.
    F11,
    /// Function key F12.
    F12,
}

/// A logical key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that produces a character. The character already reflects Shift.
    Character(char),
    /// A key without a printable character.
    Named(NamedKey),
}

/// A key press or release together with the modifiers active at the time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The key.
    pub key: Key,
    /// Modifiers held while the event happened.
    pub modifiers: Modifiers,
    /// Whether this is an auto-repeat rather than the initial press.
    pub repeat: bool,
}

impl KeyEvent {
    /// Creates a non-repeating key event.
    pub const fn new(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key,
            modifiers,
            repeat: false,
        }
    }

    /// Marks this event as an auto-repeat.
    pub const fn with_repeat(mut self, repeat: bool) -> Self {
        self.repeat = repeat;
        self
    }

    /// The text this key event commits, if any.
    ///
    /// Printable ASCII characters map to themselves; [`NamedKey::Space`] maps to
    /// a single space. Control and Meta suppress text (and Alt suppresses it too,
    /// because it is treated as a menu accelerator rather than text entry).
    /// Character keys outside printable ASCII return `None`; real text for those
    /// arrives through [`WindowEvent::TextInput`].
    pub fn text(&self) -> Option<String> {
        if self.modifiers.ctrl || self.modifiers.meta || self.modifiers.alt {
            return None;
        }
        match self.key {
            Key::Character(c) if c.is_ascii_graphic() || c == ' ' => Some(c.to_string()),
            Key::Named(NamedKey::Space) => Some(" ".to_owned()),
            _ => None,
        }
    }
}

/// A recorded click, used for double-click detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickRecord {
    /// Horizontal position of the click.
    pub x: f32,
    /// Vertical position of the click.
    pub y: f32,
    /// Button that was clicked.
    pub button: MouseButton,
    /// Time of the click.
    pub at: Instant,
    /// Consecutive click count (1, 2 or 3).
    pub count: u8,
}

/// Maximum delay between clicks for them to count as a double click.
pub const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

/// Maximum pointer movement between clicks for them to count as a double click.
pub const DOUBLE_CLICK_DISTANCE: f32 = 4.0;

/// Tracks held keys and buttons, pointer position and recent clicks.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    pressed_keys: HashSet<Key>,
    pressed_buttons: HashSet<MouseButton>,
    modifiers: Modifiers,
    pointer: (f32, f32),
    last_click: Option<ClickRecord>,
}

impl InputState {
    /// Creates an empty input state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a window event at the given time.
    ///
    /// Key presses update [`Self::is_pressed`] and modifiers; button presses also
    /// update [`Self::last_click`] with the recognised click count. Events that
    /// carry no input state (resize, wheel, …) are ignored.
    pub fn handle_event(&mut self, event: &WindowEvent, now: Instant) {
        match event {
            WindowEvent::Key(key_event) => {
                self.modifiers = key_event.modifiers;
                if key_event.repeat {
                    return;
                }
                match key_event.key {
                    Key::Named(NamedKey::Shift) => self.modifiers.shift = true,
                    Key::Named(NamedKey::Control) => self.modifiers.ctrl = true,
                    Key::Named(NamedKey::Alt) => self.modifiers.alt = true,
                    Key::Named(NamedKey::Meta) => self.modifiers.meta = true,
                    _ => {}
                }
                self.pressed_keys.insert(key_event.key);
            }
            WindowEvent::MouseInput {
                button,
                state,
                x,
                y,
            } => match state {
                ButtonState::Pressed => {
                    self.pressed_buttons.insert(*button);
                    self.pointer = (*x, *y);
                    self.register_click(*x, *y, *button, now);
                }
                ButtonState::Released => {
                    self.pressed_buttons.remove(button);
                    self.pointer = (*x, *y);
                }
            },
            WindowEvent::CursorMoved { x, y } => self.pointer = (*x, *y),
            _ => {}
        }
    }

    /// Records a click and returns its consecutive count (1, 2 or 3).
    ///
    /// A click continues the previous run when it uses the same button, happens
    /// within [`DOUBLE_CLICK_INTERVAL`] and lands within
    /// [`DOUBLE_CLICK_DISTANCE`] logical pixels of the previous click.
    pub fn register_click(&mut self, x: f32, y: f32, button: MouseButton, now: Instant) -> u8 {
        let count = match self.last_click {
            Some(previous)
                if previous.button == button
                    && now
                        .checked_duration_since(previous.at)
                        .is_some_and(|elapsed| elapsed <= DOUBLE_CLICK_INTERVAL)
                    && (previous.x - x).abs() <= DOUBLE_CLICK_DISTANCE
                    && (previous.y - y).abs() <= DOUBLE_CLICK_DISTANCE =>
            {
                previous.count.saturating_add(1).min(3)
            }
            _ => 1,
        };
        self.last_click = Some(ClickRecord {
            x,
            y,
            button,
            at: now,
            count,
        });
        count
    }

    /// Whether `key` is currently held.
    pub fn is_pressed(&self, key: &Key) -> bool {
        self.pressed_keys.contains(key)
    }

    /// Whether `button` is currently held.
    pub fn is_button_pressed(&self, button: MouseButton) -> bool {
        self.pressed_buttons.contains(&button)
    }

    /// All keys currently held, in unspecified order.
    pub fn pressed_keys(&self) -> impl Iterator<Item = &Key> {
        self.pressed_keys.iter()
    }

    /// Current modifier state.
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Last known pointer position.
    pub fn pointer(&self) -> (f32, f32) {
        self.pointer
    }

    /// The most recent click, if any.
    pub fn last_click(&self) -> Option<ClickRecord> {
        self.last_click
    }

    /// Marks `key` as held.
    ///
    /// [`WindowEvent::Key`] only models key presses (presses arrive through
    /// [`Self::handle_event`]); a backend that can report releases calls this
    /// and [`Self::release_key`] directly.
    pub fn press_key(&mut self, key: Key) {
        self.pressed_keys.insert(key);
    }

    /// Marks `key` as no longer held.
    pub fn release_key(&mut self, key: &Key) {
        self.pressed_keys.remove(key);
    }

    /// Releases every held key and button, for example on focus loss.
    pub fn release_all(&mut self) {
        self.pressed_keys.clear();
        self.pressed_buttons.clear();
        self.modifiers = Modifiers::NONE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::WindowEvent;

    fn key_event(key: Key, modifiers: Modifiers) -> WindowEvent {
        WindowEvent::Key(KeyEvent::new(key, modifiers))
    }

    fn click(button: MouseButton, x: f32, y: f32) -> WindowEvent {
        WindowEvent::MouseInput {
            button,
            state: ButtonState::Pressed,
            x,
            y,
        }
    }

    #[test]
    fn printable_ascii_maps_to_text() {
        let event = KeyEvent::new(Key::Character('a'), Modifiers::NONE);
        assert_eq!(event.text().as_deref(), Some("a"));
        let shifted = KeyEvent::new(
            Key::Character('A'),
            Modifiers::new(true, false, false, false),
        );
        assert_eq!(shifted.text().as_deref(), Some("A"));
        let space = KeyEvent::new(Key::Character(' '), Modifiers::NONE);
        assert_eq!(space.text().as_deref(), Some(" "));
    }

    #[test]
    fn modifiers_suppress_text() {
        for modifiers in [
            Modifiers::new(false, true, false, false),
            Modifiers::new(false, false, true, false),
            Modifiers::new(false, false, false, true),
        ] {
            let event = KeyEvent::new(Key::Character('x'), modifiers);
            assert_eq!(event.text(), None);
        }
        // Shift alone does not suppress text.
        let shifted = KeyEvent::new(
            Key::Character('x'),
            Modifiers::new(true, false, false, false),
        );
        assert_eq!(shifted.text().as_deref(), Some("x"));
    }

    #[test]
    fn named_keys_do_not_produce_text_except_space() {
        let enter = KeyEvent::new(Key::Named(NamedKey::Enter), Modifiers::NONE);
        assert_eq!(enter.text(), None);
        let space = KeyEvent::new(Key::Named(NamedKey::Space), Modifiers::NONE);
        assert_eq!(space.text().as_deref(), Some(" "));
        let f1 = KeyEvent::new(Key::Named(NamedKey::F1), Modifiers::NONE);
        assert_eq!(f1.text(), None);
    }

    #[test]
    fn non_ascii_character_has_no_key_text() {
        let event = KeyEvent::new(Key::Character('é'), Modifiers::NONE);
        assert_eq!(event.text(), None);
    }

    #[test]
    fn tracks_pressed_keys() {
        let mut state = InputState::new();
        let now = Instant::now();
        let shift = Key::Named(NamedKey::Shift);
        let a = Key::Character('a');
        state.handle_event(
            &key_event(shift, Modifiers::new(true, false, false, false)),
            now,
        );
        state.handle_event(
            &key_event(a, Modifiers::new(true, false, false, false)),
            now,
        );
        assert!(state.is_pressed(&shift));
        assert!(state.is_pressed(&a));
        assert!(state.modifiers().shift);
        assert_eq!(state.pressed_keys().count(), 2);
        state.release_key(&shift);
        assert!(!state.is_pressed(&shift));
        state.press_key(Key::Named(NamedKey::Escape));
        assert!(state.is_pressed(&Key::Named(NamedKey::Escape)));
        state.release_all();
        assert!(!state.is_pressed(&a));
        assert!(state.modifiers().is_empty());
        assert_eq!(state.pressed_keys().count(), 0);
    }

    #[test]
    fn repeat_events_do_not_duplicate_presses() {
        let mut state = InputState::new();
        let now = Instant::now();
        let event = KeyEvent::new(Key::Character('a'), Modifiers::NONE).with_repeat(true);
        state.handle_event(&WindowEvent::Key(event), now);
        assert!(!state.is_pressed(&Key::Character('a')));
        assert_eq!(state.pressed_keys().count(), 0);
    }

    #[test]
    fn tracks_pointer_and_buttons() {
        let mut state = InputState::new();
        let now = Instant::now();
        state.handle_event(&WindowEvent::CursorMoved { x: 12.0, y: 34.0 }, now);
        assert_eq!(state.pointer(), (12.0, 34.0));
        state.handle_event(&click(MouseButton::Left, 12.0, 34.0), now);
        assert!(state.is_button_pressed(MouseButton::Left));
        state.handle_event(
            &WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: ButtonState::Released,
                x: 13.0,
                y: 35.0,
            },
            now,
        );
        assert!(!state.is_button_pressed(MouseButton::Left));
        assert_eq!(state.pointer(), (13.0, 35.0));
    }

    #[test]
    fn detects_double_and_triple_click() {
        let mut state = InputState::new();
        let start = Instant::now();
        state.handle_event(&click(MouseButton::Left, 10.0, 10.0), start);
        assert_eq!(state.last_click().map(|c| c.count), Some(1));
        state.handle_event(
            &click(MouseButton::Left, 11.0, 11.0),
            start + Duration::from_millis(120),
        );
        assert_eq!(state.last_click().map(|c| c.count), Some(2));
        state.handle_event(
            &click(MouseButton::Left, 11.0, 11.0),
            start + Duration::from_millis(240),
        );
        assert_eq!(state.last_click().map(|c| c.count), Some(3));
        state.handle_event(
            &click(MouseButton::Left, 11.0, 11.0),
            start + Duration::from_millis(360),
        );
        assert_eq!(state.last_click().map(|c| c.count), Some(3));
    }

    #[test]
    fn click_run_resets_on_time_distance_or_button() {
        let mut state = InputState::new();
        let start = Instant::now();
        assert_eq!(
            state.register_click(10.0, 10.0, MouseButton::Left, start),
            1
        );
        // Too slow.
        assert_eq!(
            state.register_click(
                10.0,
                10.0,
                MouseButton::Left,
                start + Duration::from_millis(900)
            ),
            1
        );
        // Too far.
        assert_eq!(
            state.register_click(40.0, 10.0, MouseButton::Left, start),
            1
        );
        // Different button.
        assert_eq!(
            state.register_click(10.0, 10.0, MouseButton::Right, start),
            1
        );
        assert_eq!(
            state.register_click(
                10.0,
                10.0,
                MouseButton::Right,
                start + Duration::from_millis(50)
            ),
            2
        );
    }

    #[test]
    fn unrelated_events_are_ignored() {
        let mut state = InputState::new();
        let now = Instant::now();
        state.handle_event(&WindowEvent::Resized { w: 1, h: 2 }, now);
        state.handle_event(
            &WindowEvent::Wheel {
                delta_x: 1.0,
                delta_y: 2.0,
            },
            now,
        );
        assert_eq!(state.pointer(), (0.0, 0.0));
        assert_eq!(state.pressed_keys().count(), 0);
        assert!(state.last_click().is_none());
    }
}
