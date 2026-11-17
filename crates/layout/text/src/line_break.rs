//! Greedy line breaking for the built-in monospace face.
//!
//! Lines are wrapped at spaces and after hyphens, with mandatory breaks on
//! `'\n'`. All positions are byte offsets into the source string and always
//! fall on UTF-8 character boundaries.
//!
//! Only the simple greedy algorithm is implemented (no Knuth-Plass, no CSS
//! `line-break` tailoring, no hyphenation dictionaries).

use crate::glyphs::Font;

/// Result of breaking a string into lines for a given width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineBreakResult {
    /// Byte offset at which each line after the first starts.
    ///
    /// `breaks.len()` is always `lines.len() - 1`: a wrap contributes the offset
    /// after the separating space, and a newline contributes the offset after
    /// the `'\n'` itself.
    pub breaks: Vec<usize>,
    /// `(start, end)` byte ranges, one per line, in order.
    ///
    /// Ranges never contain a `'\n'` and never contain the space a line was
    /// wrapped at. A forced break on a trailing newline produces a final empty
    /// range `(len, len)`.
    pub lines: Vec<(usize, usize)>,
}

impl LineBreakResult {
    /// Number of laid-out lines (at least one, even for empty text).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// Breaks `text` into lines of at most `available_width` pixels.
///
/// `px` is the requested pixel size (16.0 is the design size). If a single word
/// is wider than the available width it is left on its own line and overflows;
/// lines are never broken inside a word, except after a hyphen.
pub fn line_break(text: &str, available_width: f32, px: f32) -> LineBreakResult {
    let font = Font::monospace();
    let mut lines = Vec::new();
    let mut breaks = Vec::new();
    let mut start = 0usize;
    let mut width = 0.0f32;
    // (start of the next line, end of the current line) for the last break
    // opportunity seen on this line.
    let mut opportunity: Option<(usize, usize)> = None;

    for (idx, ch) in text.char_indices() {
        let ch_end = idx + ch.len_utf8();
        if ch == '\n' {
            lines.push((start, idx));
            breaks.push(ch_end);
            start = ch_end;
            width = 0.0;
            opportunity = None;
            continue;
        }

        width += font.advance(ch, px);
        if ch == ' ' {
            // Break after the space, but do not keep the space in the line.
            opportunity = Some((ch_end, idx));
        } else if ch == '-' {
            // Break after the hyphen, keeping it on the line.
            opportunity = Some((ch_end, ch_end));
        }

        if width > available_width
            && let Some((next_start, line_end)) = opportunity
            && next_start > start
            && next_start < text.len()
            && line_end >= start
        {
            lines.push((start, line_end));
            breaks.push(next_start);
            start = next_start;
            width = font.measure_range(text, px, start, ch_end);
            opportunity = None;
        }
    }

    lines.push((start, text.len()));
    LineBreakResult { breaks, lines }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice(text: &str, range: (usize, usize)) -> &str {
        &text[range.0..range.1]
    }

    #[test]
    fn empty_text_has_one_empty_line() {
        let result = line_break("", 100.0, 16.0);
        assert_eq!(result.lines, vec![(0, 0)]);
        assert!(result.breaks.is_empty());
        assert_eq!(result.line_count(), 1);
    }

    #[test]
    fn short_text_does_not_wrap() {
        let result = line_break("hello", 100.0, 16.0);
        assert_eq!(result.lines, vec![(0, 5)]);
        assert!(result.breaks.is_empty());
    }

    #[test]
    fn exact_fit_does_not_wrap() {
        // 5 characters at 8px each fit in 40px exactly.
        let result = line_break("hello", 40.0, 16.0);
        assert_eq!(result.lines, vec![(0, 5)]);
    }

    #[test]
    fn wraps_on_space_and_drops_the_space() {
        let text = "hello world";
        let result = line_break(text, 40.0, 16.0);
        assert_eq!(result.breaks, vec![6]);
        assert_eq!(result.lines, vec![(0, 5), (6, 11)]);
        assert_eq!(slice(text, result.lines[0]), "hello");
        assert_eq!(slice(text, result.lines[1]), "world");
    }

    #[test]
    fn wraps_after_hyphen_keeping_it() {
        let text = "co-operate";
        let result = line_break(text, 24.0, 16.0);
        assert_eq!(result.lines, vec![(0, 3), (3, 10)]);
        assert_eq!(slice(text, result.lines[0]), "co-");
        assert_eq!(slice(text, result.lines[1]), "operate");
    }

    #[test]
    fn newline_is_mandatory() {
        let text = "ab\ncd";
        let result = line_break(text, 1000.0, 16.0);
        assert_eq!(result.lines, vec![(0, 2), (3, 5)]);
        assert_eq!(result.breaks, vec![3]);
    }

    #[test]
    fn consecutive_and_trailing_newlines_make_empty_lines() {
        let text = "a\n\nb\n";
        let result = line_break(text, 1000.0, 16.0);
        assert_eq!(result.lines, vec![(0, 1), (2, 2), (3, 4), (5, 5)]);
        assert_eq!(result.breaks, vec![2, 3, 5]);
        assert_eq!(result.breaks.len() + 1, result.lines.len());
    }

    #[test]
    fn long_word_overflows_on_one_line() {
        let text = "abcdefghij";
        let result = line_break(text, 24.0, 16.0);
        assert_eq!(result.lines, vec![(0, 10)]);
        assert!(result.breaks.is_empty());
    }

    #[test]
    fn long_word_overflow_resumes_wrapping_after_it() {
        let text = "abcdefghij klm";
        let result = line_break(text, 40.0, 16.0);
        assert_eq!(result.lines, vec![(0, 10), (11, 14)]);
    }

    #[test]
    fn wraps_multiple_times() {
        let text = "one two three four";
        let result = line_break(text, 32.0, 16.0);
        let rendered: Vec<&str> = result.lines.iter().map(|r| slice(text, *r)).collect();
        assert_eq!(rendered, vec!["one", "two", "three", "four"]);
        assert_eq!(result.breaks, vec![4, 8, 14]);
        // Every break is the start of the following line.
        for (break_pos, line) in result.breaks.iter().zip(&result.lines[1..]) {
            assert_eq!(*break_pos, line.0);
        }
    }

    #[test]
    fn unicode_ranges_stay_on_boundaries() {
        let text = "héllo wörld ünïcode";
        let result = line_break(text, 40.0, 16.0);
        let mut previous_end = 0;
        for &(start, end) in &result.lines {
            assert!(text.is_char_boundary(start), "start {start} not a boundary");
            assert!(text.is_char_boundary(end), "end {end} not a boundary");
            assert!(start >= previous_end);
            previous_end = end;
        }
        let joined: String = result
            .lines
            .iter()
            .map(|range| slice(text, *range))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, text);
    }

    #[test]
    fn unicode_wrap_uses_character_widths_not_bytes() {
        // Five 2-byte characters occupy 5 cells = 40px, so this fits.
        let text = "äääää";
        let result = line_break(text, 40.0, 16.0);
        assert_eq!(result.lines, vec![(0, text.len())]);
    }

    #[test]
    fn zero_width_wraps_at_opportunities() {
        let text = "aa bb";
        let result = line_break(text, 0.0, 16.0);
        let rendered: Vec<&str> = result.lines.iter().map(|r| slice(text, *r)).collect();
        assert_eq!(rendered, vec!["aa", "bb"]);
    }

    #[test]
    fn scaled_text_wraps_by_pixel_size() {
        // At 32px each character is 16px wide, so three fit in 48px.
        let text = "abc def";
        let result = line_break(text, 48.0, 32.0);
        assert_eq!(result.lines, vec![(0, 3), (4, 7)]);
    }

    #[test]
    fn break_offsets_are_monotonic() {
        let text = "the quick brown fox jumps over the lazy dog";
        let result = line_break(text, 64.0, 16.0);
        assert!(result.breaks.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(result.breaks.len() + 1, result.lines.len());
    }
}
