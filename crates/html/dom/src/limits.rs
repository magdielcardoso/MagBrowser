//! Configurable resource caps for the DOM arena.
//!
//! A document can be fed unbounded input (for example a hostile HTML page), so
//! every operation that can grow memory is bounded. The defaults are generous
//! enough for real pages but small enough that a runaway parser cannot exhaust
//! the process.

/// Hard caps applied by a [`Document`](crate::Document) when allocating nodes,
/// attributes and text.
///
/// Use [`Document::with_limits`](crate::Document::with_limits) or
/// [`Document::set_limits`](crate::Document::set_limits) to override them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum number of nodes in the arena, including the root and detached
    /// nodes. Ids are never reused, so this also bounds the id space.
    pub max_nodes: usize,
    /// Maximum number of attributes a single element may carry.
    pub max_attributes_per_element: usize,
    /// Maximum number of UTF-8 bytes in a single text node.
    pub max_text_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 200_000,
            max_attributes_per_element: 512,
            max_text_bytes: 4 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Caps that effectively disable the limits (useful for trusted input).
    pub fn unlimited() -> Self {
        Self {
            max_nodes: usize::MAX,
            max_attributes_per_element: usize::MAX,
            max_text_bytes: usize::MAX,
        }
    }

    /// Returns at most `max_bytes` bytes of `text`, never splitting a UTF-8
    /// code point.
    pub fn clamp_text<'a>(&self, text: &'a str) -> &'a str {
        clamp_str(text, self.max_text_bytes)
    }
}

/// Returns at most `max_bytes` bytes of `text`, never splitting a UTF-8 code
/// point. Used both when creating text nodes and when appending to one.
pub fn clamp_str(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_generous_but_bounded() {
        let limits = Limits::default();
        assert_eq!(limits.max_nodes, 200_000);
        assert_eq!(limits.max_attributes_per_element, 512);
        assert_eq!(limits.max_text_bytes, 4 * 1024 * 1024);
    }

    #[test]
    fn clamp_preserves_utf8_boundaries() {
        assert_eq!(clamp_str("abcdef", 3), "abc");
        assert_eq!(clamp_str("aé€", 2), "a");
        assert_eq!(clamp_str("aé€", 3), "aé");
        assert_eq!(clamp_str("aé€", 4), "aé");
        assert_eq!(clamp_str("aé€", 100), "aé€");
    }
}
