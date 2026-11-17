//! Messages and channels between engine components.
//!
//! Engine components talk through two pieces:
//!
//! * [`Message`] — a typed request/notification with an opaque payload and a
//!   stable binary wire format ([`encode_message`] / [`decode_message`]).
//! * [`Channel`] — a bounded, thread-safe queue with blocking and non-blocking
//!   receive operations.
//!
//! ```
//! use browser_ipc::{Channel, Message, MessageKind, decode_message, encode_message};
//!
//! let channel = Channel::bounded(4);
//! channel.send(Message::navigate(1, "ui", "https://example.com/")).unwrap();
//!
//! let message = channel.recv_timeout(std::time::Duration::from_millis(50)).unwrap();
//! assert_eq!(message.kind, MessageKind::Navigate);
//!
//! let bytes = encode_message(&message);
//! assert_eq!(decode_message(&bytes).unwrap(), message);
//! ```

#![forbid(unsafe_code)]

mod channel;
mod messages;
mod serialization;

pub use channel::{Channel, ChannelError};
pub use messages::{Message, MessageId, MessageKind};
pub use serialization::{
    DecodeError, MAGIC, MAX_CUSTOM_KIND_LEN, MAX_PAYLOAD_LEN, MAX_SENDER_LEN, VERSION,
    decode_message, encode_message,
};
