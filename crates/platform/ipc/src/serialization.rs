//! Deterministic little-endian wire format for [`Message`].
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! magic      4 bytes  b"MIPC"
//! version    1 byte   currently 1
//! id         8 bytes  u64
//! kind       1 byte   tag; for Custom (7) a u32 length + UTF-8 name follows
//! payload    u32 length + that many bytes
//! sender     u32 length + UTF-8 bytes
//! ```
//!
//! Encoding the same message twice always produces the same bytes, which makes
//! transcripts and hashes stable. Decoding is strict: trailing bytes, unknown
//! tags and lengths above the limits below are rejected.

use std::fmt;

use crate::messages::{Message, MessageId, MessageKind};

/// File/stream magic bytes.
pub const MAGIC: [u8; 4] = *b"MIPC";

/// Current wire format version.
pub const VERSION: u8 = 1;

/// Maximum accepted payload length (16 MiB).
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;

/// Maximum accepted sender length (64 KiB).
pub const MAX_SENDER_LEN: usize = 64 * 1024;

/// Maximum accepted custom-kind name length (256 bytes).
pub const MAX_CUSTOM_KIND_LEN: usize = 256;

/// Errors produced while decoding a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The buffer ended in the middle of a field.
    Truncated,
    /// The buffer does not start with [`MAGIC`].
    BadMagic,
    /// The version byte is not supported.
    UnsupportedVersion(u8),
    /// The kind tag is unknown.
    UnknownKind(u8),
    /// A length prefix exceeds the documented limit.
    Oversize {
        /// Field whose length was too large.
        field: &'static str,
        /// Length found in the buffer.
        len: usize,
        /// Maximum accepted length.
        max: usize,
    },
    /// A string field was not valid UTF-8.
    InvalidUtf8(&'static str),
    /// Bytes remained after a complete message.
    TrailingData(usize),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("message is truncated"),
            Self::BadMagic => f.write_str("bad message magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported message version {version}")
            }
            Self::UnknownKind(tag) => write!(f, "unknown message kind tag {tag}"),
            Self::Oversize { field, len, max } => {
                write!(f, "{field} length {len} exceeds the limit of {max}")
            }
            Self::InvalidUtf8(field) => write!(f, "{field} is not valid UTF-8"),
            Self::TrailingData(count) => {
                write!(f, "{count} trailing bytes after the message")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encodes `message` into its canonical wire representation.
///
/// This function does not fail: lengths that do not fit a `u32` are impossible
/// with in-memory `Vec`s in practice, and values above the decoder's limits
/// simply produce a buffer the decoder will reject as oversize.
pub fn encode_message(message: &Message) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        MAGIC.len() + 1 + 8 + 1 + 4 + message.payload.len() + 4 + message.sender.len(),
    );
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&message.id.0.to_le_bytes());
    out.push(message.kind.tag());
    if let MessageKind::Custom(name) = &message.kind {
        put_bytes(&mut out, name.as_bytes());
    }
    put_bytes(&mut out, &message.payload);
    put_bytes(&mut out, message.sender.as_bytes());
    out
}

/// Decodes a message previously produced by [`encode_message`].
pub fn decode_message(bytes: &[u8]) -> Result<Message, DecodeError> {
    let mut reader = Reader::new(bytes);
    let magic = reader.take(MAGIC.len())?;
    if magic != MAGIC.as_slice() {
        return Err(DecodeError::BadMagic);
    }
    let version = reader.u8()?;
    if version != VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let id = MessageId(reader.u64()?);
    let tag = reader.u8()?;
    let kind = if tag == MessageKind::TAG_CUSTOM {
        let name = reader.string(MAX_CUSTOM_KIND_LEN, "custom kind")?;
        MessageKind::Custom(name)
    } else {
        MessageKind::from_tag(tag).ok_or(DecodeError::UnknownKind(tag))?
    };
    let payload = reader.length_prefixed(MAX_PAYLOAD_LEN, "payload")?.to_vec();
    let sender = reader.string(MAX_SENDER_LEN, "sender")?;
    if !reader.is_empty() {
        return Err(DecodeError::TrailingData(reader.remaining()));
    }
    Ok(Message {
        id,
        kind,
        payload,
        sender,
    })
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).expect("field length fits in u32");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(DecodeError::Truncated)?;
        if end > self.bytes.len() {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn length_prefixed(
        &mut self,
        max: usize,
        field: &'static str,
    ) -> Result<&'a [u8], DecodeError> {
        let len = self.u32()? as usize;
        if len > max {
            return Err(DecodeError::Oversize { field, len, max });
        }
        self.take(len)
    }

    fn string(&mut self, max: usize, field: &'static str) -> Result<String, DecodeError> {
        let bytes = self.length_prefixed(max, field)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::InvalidUtf8(field))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Message {
        Message::new(0x0102_0304_0506_0708, MessageKind::LoadProgress, "network")
            .with_payload(vec![0, 1, 2, 0xfe, 0xff])
    }

    fn encoded_len(message: &Message) -> usize {
        let custom = match &message.kind {
            MessageKind::Custom(name) => 4 + name.len(),
            _ => 0,
        };
        MAGIC.len() + 1 + 8 + 1 + custom + 4 + message.payload.len() + 4 + message.sender.len()
    }

    #[test]
    fn round_trips_every_kind() {
        let kinds = [
            MessageKind::Navigate,
            MessageKind::LoadProgress,
            MessageKind::FrameRendered,
            MessageKind::Script,
            MessageKind::Input,
            MessageKind::ConsoleLog,
            MessageKind::Quit,
            MessageKind::Custom("devtools.inspect".to_owned()),
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            let message = Message::new(index as u64, kind, "component").with_payload(vec![
                index as u8,
                0x80,
                0xff,
            ]);
            let bytes = encode_message(&message);
            assert_eq!(bytes.len(), encoded_len(&message));
            assert_eq!(decode_message(&bytes).unwrap(), message);
        }
    }

    #[test]
    fn empty_fields_round_trip() {
        let message = Message::new(0, MessageKind::Quit, "");
        let bytes = encode_message(&message);
        assert_eq!(bytes.len(), 4 + 1 + 8 + 1 + 4 + 4);
        assert_eq!(decode_message(&bytes).unwrap(), message);
    }

    #[test]
    fn encoding_is_deterministic() {
        let message = sample();
        assert_eq!(encode_message(&message), encode_message(&message));
    }

    #[test]
    fn header_layout_is_little_endian_and_stable() {
        let message = Message::new(0x0102_0304_0506_0708, MessageKind::Quit, "");
        let bytes = encode_message(&message);
        assert_eq!(&bytes[0..4], b"MIPC");
        assert_eq!(bytes[4], VERSION);
        assert_eq!(
            &bytes[5..13],
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(bytes[13], MessageKind::TAG_QUIT);
        assert_eq!(&bytes[14..18], &[0, 0, 0, 0]);
        assert_eq!(&bytes[18..22], &[0, 0, 0, 0]);
        assert_eq!(bytes.len(), 22);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = encode_message(&sample());
        bytes[0] = b'X';
        assert_eq!(decode_message(&bytes), Err(DecodeError::BadMagic));
        assert_eq!(decode_message(b""), Err(DecodeError::Truncated));
        assert_eq!(decode_message(b"MIP"), Err(DecodeError::Truncated));
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut bytes = encode_message(&sample());
        bytes[4] = 2;
        assert_eq!(
            decode_message(&bytes),
            Err(DecodeError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn rejects_unknown_kind() {
        let mut bytes = encode_message(&Message::new(1, MessageKind::Quit, "x"));
        bytes[13] = 42;
        assert_eq!(decode_message(&bytes), Err(DecodeError::UnknownKind(42)));
    }

    #[test]
    fn rejects_every_truncation() {
        let bytes = encode_message(&sample());
        for len in 0..bytes.len() {
            let result = decode_message(&bytes[..len]);
            assert!(result.is_err(), "prefix of length {len} decoded");
        }
        assert!(decode_message(&bytes).is_ok());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encode_message(&sample());
        bytes.push(0);
        assert_eq!(decode_message(&bytes), Err(DecodeError::TrailingData(1)));
    }

    #[test]
    fn rejects_oversize_payload() {
        let mut bytes = encode_message(&sample());
        // Payload length sits right before the payload itself.
        let payload_offset = 4 + 1 + 8 + 1;
        bytes[payload_offset..payload_offset + 4]
            .copy_from_slice(&(MAX_PAYLOAD_LEN as u32 + 1).to_le_bytes());
        assert_eq!(
            decode_message(&bytes),
            Err(DecodeError::Oversize {
                field: "payload",
                len: MAX_PAYLOAD_LEN + 1,
                max: MAX_PAYLOAD_LEN,
            })
        );
    }

    #[test]
    fn rejects_oversize_sender() {
        let mut bytes = encode_message(&Message::new(1, MessageKind::Quit, "x"));
        let sender_offset = bytes.len() - 5;
        bytes[sender_offset..sender_offset + 4]
            .copy_from_slice(&(MAX_SENDER_LEN as u32 + 1).to_le_bytes());
        assert_eq!(
            decode_message(&bytes),
            Err(DecodeError::Oversize {
                field: "sender",
                len: MAX_SENDER_LEN + 1,
                max: MAX_SENDER_LEN,
            })
        );
    }

    #[test]
    fn rejects_oversize_custom_kind() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.push(MessageKind::TAG_CUSTOM);
        bytes.extend_from_slice(&(MAX_CUSTOM_KIND_LEN as u32 + 1).to_le_bytes());
        assert!(matches!(
            decode_message(&bytes),
            Err(DecodeError::Oversize {
                field: "custom kind",
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_utf8_fields() {
        // Payload can be arbitrary bytes; sender and custom kind cannot.
        let mut message = Message::new(1, MessageKind::Quit, "x");
        message.payload = vec![0xff];
        let bytes = encode_message(&message);
        assert_eq!(decode_message(&bytes).unwrap().payload, vec![0xff]);

        let mut bad_sender = Vec::new();
        bad_sender.extend_from_slice(&MAGIC);
        bad_sender.push(VERSION);
        bad_sender.extend_from_slice(&1u64.to_le_bytes());
        bad_sender.push(MessageKind::TAG_QUIT);
        bad_sender.extend_from_slice(&0u32.to_le_bytes());
        bad_sender.extend_from_slice(&2u32.to_le_bytes());
        bad_sender.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(
            decode_message(&bad_sender),
            Err(DecodeError::InvalidUtf8("sender"))
        );

        let mut bad_kind = Vec::new();
        bad_kind.extend_from_slice(&MAGIC);
        bad_kind.push(VERSION);
        bad_kind.extend_from_slice(&1u64.to_le_bytes());
        bad_kind.push(MessageKind::TAG_CUSTOM);
        bad_kind.extend_from_slice(&2u32.to_le_bytes());
        bad_kind.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(
            decode_message(&bad_kind),
            Err(DecodeError::InvalidUtf8("custom kind"))
        );
    }

    #[test]
    fn unicode_strings_round_trip() {
        let message = Message::new(42, MessageKind::Custom("événement".to_owned()), "réseau")
            .with_payload("héllo".as_bytes().to_vec());
        let decoded = decode_message(&encode_message(&message)).unwrap();
        assert_eq!(decoded, message);
        assert_eq!(decoded.sender, "réseau");
    }

    #[test]
    fn length_prefixes_are_u32_not_u64() {
        let message = Message::new(1, MessageKind::Quit, "ab");
        let bytes = encode_message(&message);
        let sender_len_at = bytes.len() - 2 - 4;
        assert_eq!(&bytes[sender_len_at..sender_len_at + 4], &[2, 0, 0, 0]);
    }

    #[test]
    fn errors_have_messages() {
        assert!(!DecodeError::Truncated.to_string().is_empty());
        assert!(!DecodeError::BadMagic.to_string().is_empty());
        assert!(DecodeError::UnsupportedVersion(9).to_string().contains('9'));
        assert!(DecodeError::UnknownKind(9).to_string().contains('9'));
        assert!(
            DecodeError::Oversize {
                field: "payload",
                len: 10,
                max: 5
            }
            .to_string()
            .contains("payload")
        );
        assert!(
            DecodeError::InvalidUtf8("sender")
                .to_string()
                .contains("sender")
        );
        assert!(DecodeError::TrailingData(3).to_string().contains('3'));
    }
}
