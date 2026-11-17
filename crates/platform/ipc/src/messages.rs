//! Message types exchanged between engine components.
//!
//! A [`Message`] pairs a typed [`MessageKind`] with opaque bytes. Payload
//! interpretation belongs to the component handling the message; for example a
//! `Navigate` payload is the target URL as UTF-8 (see [`Message::navigate`]).
//! Wire encoding lives in [`crate::serialization`].

/// Identifier assigned by the sender; used to correlate replies and progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub u64);

impl MessageId {
    /// Wraps a raw numeric id.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw numeric id.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for MessageId {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a message is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Request to navigate; payload is the URL bytes.
    Navigate,
    /// Load progress report; payload is component-defined.
    LoadProgress,
    /// A frame was produced; payload is component-defined.
    FrameRendered,
    /// Script for the JavaScript engine; payload is the source bytes.
    Script,
    /// User input forwarded to the UI process; payload is component-defined.
    Input,
    /// Console output; payload is UTF-8 text.
    ConsoleLog,
    /// Shut down.
    Quit,
    /// Component-defined message named by the string.
    Custom(String),
}

impl MessageKind {
    /// Wire tag of [`MessageKind::Navigate`].
    pub const TAG_NAVIGATE: u8 = 0;
    /// Wire tag of [`MessageKind::LoadProgress`].
    pub const TAG_LOAD_PROGRESS: u8 = 1;
    /// Wire tag of [`MessageKind::FrameRendered`].
    pub const TAG_FRAME_RENDERED: u8 = 2;
    /// Wire tag of [`MessageKind::Script`].
    pub const TAG_SCRIPT: u8 = 3;
    /// Wire tag of [`MessageKind::Input`].
    pub const TAG_INPUT: u8 = 4;
    /// Wire tag of [`MessageKind::ConsoleLog`].
    pub const TAG_CONSOLE_LOG: u8 = 5;
    /// Wire tag of [`MessageKind::Quit`].
    pub const TAG_QUIT: u8 = 6;
    /// Wire tag of [`MessageKind::Custom`] (followed by a length-prefixed name).
    pub const TAG_CUSTOM: u8 = 7;

    /// Numeric wire tag for this kind.
    pub const fn tag(&self) -> u8 {
        match self {
            Self::Navigate => Self::TAG_NAVIGATE,
            Self::LoadProgress => Self::TAG_LOAD_PROGRESS,
            Self::FrameRendered => Self::TAG_FRAME_RENDERED,
            Self::Script => Self::TAG_SCRIPT,
            Self::Input => Self::TAG_INPUT,
            Self::ConsoleLog => Self::TAG_CONSOLE_LOG,
            Self::Quit => Self::TAG_QUIT,
            Self::Custom(_) => Self::TAG_CUSTOM,
        }
    }

    /// Rebuilds a non-custom kind from its tag.
    ///
    /// Returns `None` for [`Self::TAG_CUSTOM`] (which needs the name that follows
    /// the tag) and for unknown tags.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            Self::TAG_NAVIGATE => Some(Self::Navigate),
            Self::TAG_LOAD_PROGRESS => Some(Self::LoadProgress),
            Self::TAG_FRAME_RENDERED => Some(Self::FrameRendered),
            Self::TAG_SCRIPT => Some(Self::Script),
            Self::TAG_INPUT => Some(Self::Input),
            Self::TAG_CONSOLE_LOG => Some(Self::ConsoleLog),
            Self::TAG_QUIT => Some(Self::Quit),
            _ => None,
        }
    }

    /// Stable human-readable name (the custom string for [`Self::Custom`]).
    pub fn name(&self) -> &str {
        match self {
            Self::Navigate => "navigate",
            Self::LoadProgress => "load-progress",
            Self::FrameRendered => "frame-rendered",
            Self::Script => "script",
            Self::Input => "input",
            Self::ConsoleLog => "console-log",
            Self::Quit => "quit",
            Self::Custom(name) => name,
        }
    }
}

/// A message from one engine component to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Sender-assigned identifier.
    pub id: MessageId,
    /// What the message is about.
    pub kind: MessageKind,
    /// Opaque payload bytes.
    pub payload: Vec<u8>,
    /// Component name of the sender.
    pub sender: String,
}

impl Message {
    /// Creates a message with an empty payload.
    pub fn new(id: impl Into<MessageId>, kind: MessageKind, sender: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind,
            payload: Vec::new(),
            sender: sender.into(),
        }
    }

    /// Sets the payload.
    pub fn with_payload(mut self, payload: impl Into<Vec<u8>>) -> Self {
        self.payload = payload.into();
        self
    }

    /// Creates a navigation request whose payload is the URL in UTF-8.
    pub fn navigate(id: impl Into<MessageId>, sender: impl Into<String>, url: &str) -> Self {
        Self::new(id, MessageKind::Navigate, sender).with_payload(url.as_bytes().to_vec())
    }

    /// Creates a console log message whose payload is the text in UTF-8.
    pub fn console_log(
        id: impl Into<MessageId>,
        sender: impl Into<String>,
        text: impl AsRef<str>,
    ) -> Self {
        Self::new(id, MessageKind::ConsoleLog, sender)
            .with_payload(text.as_ref().as_bytes().to_vec())
    }

    /// Creates a shutdown message.
    pub fn quit(id: impl Into<MessageId>, sender: impl Into<String>) -> Self {
        Self::new(id, MessageKind::Quit, sender)
    }

    /// The payload interpreted as UTF-8 text, if it is valid.
    pub fn payload_as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.payload).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_tags_are_stable_and_unique() {
        let kinds = [
            MessageKind::Navigate,
            MessageKind::LoadProgress,
            MessageKind::FrameRendered,
            MessageKind::Script,
            MessageKind::Input,
            MessageKind::ConsoleLog,
            MessageKind::Quit,
            MessageKind::Custom("x".to_owned()),
        ];
        let tags: Vec<u8> = kinds.iter().map(MessageKind::tag).collect();
        assert_eq!(tags, vec![0, 1, 2, 3, 4, 5, 6, 7]);
        let mut sorted = tags.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tags.len());
    }

    #[test]
    fn tags_round_trip_for_builtin_kinds() {
        for tag in 0..=6 {
            let kind = MessageKind::from_tag(tag).expect("known tag");
            assert_eq!(kind.tag(), tag);
        }
        assert_eq!(MessageKind::from_tag(MessageKind::TAG_CUSTOM), None);
        assert_eq!(MessageKind::from_tag(99), None);
    }

    #[test]
    fn constructors_fill_expected_fields() {
        let navigate = Message::navigate(7, "ui", "https://example.com/");
        assert_eq!(navigate.id, MessageId(7));
        assert_eq!(navigate.kind, MessageKind::Navigate);
        assert_eq!(navigate.sender, "ui");
        assert_eq!(navigate.payload_as_str(), Some("https://example.com/"));

        let log = Message::console_log(8, "js", "hello");
        assert_eq!(log.kind, MessageKind::ConsoleLog);
        assert_eq!(log.payload_as_str(), Some("hello"));

        let quit = Message::quit(9, "engine");
        assert_eq!(quit.kind, MessageKind::Quit);
        assert!(quit.payload.is_empty());
    }

    #[test]
    fn custom_kind_reports_its_name() {
        let kind = MessageKind::Custom("devtools".to_owned());
        assert_eq!(kind.name(), "devtools");
        assert_eq!(MessageKind::Quit.name(), "quit");
    }

    #[test]
    fn invalid_utf8_payload_is_not_text() {
        let message = Message::new(1, MessageKind::Script, "js").with_payload(vec![0xff]);
        assert_eq!(message.payload_as_str(), None);
    }
}
