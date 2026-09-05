//! The chars-per-token fallback, and the per-message envelope constant.
//!
//! Used when no model-aware tokenizer is reachable, which is every path that is
//! not the live Goose adapter. Deliberately the same arithmetic the trimmer uses.

use crate::models::ports::token_counter::TokenCounter;

/// Divisor for the character heuristic. Roughly right for English prose and
/// roughly wrong for everything else — dense punctuation (JSON, code) packs
/// more tokens per character, and non-Latin scripts far more.
const CHARS_PER_TOKEN: usize = 4;

/// Per-message overhead: role marker and message delimiters the chat template
/// adds around every message. Counted by the trimmer, not by the counter,
/// because it is a property of the envelope rather than of the text.
pub const PER_MESSAGE_TOKEN_OVERHEAD: usize = 4;

/// `text.len() / 4`. Never exact, and says so.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicTokenCounter;

impl TokenCounter for HeuristicTokenCounter {
    fn count(&self, text: &str) -> usize {
        text.len() / CHARS_PER_TOKEN
    }

    fn is_exact(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "chars/4"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_by_dividing_characters() {
        let c = HeuristicTokenCounter;
        assert_eq!(c.count(""), 0);
        assert_eq!(c.count("abcd"), 1);
        assert_eq!(c.count("abcdefgh"), 2);
    }

    #[test]
    fn is_never_exact() {
        assert!(!HeuristicTokenCounter.is_exact());
        assert_eq!(HeuristicTokenCounter.name(), "chars/4");
    }

    /// `len()` is bytes, not characters, so multi-byte text is over-counted.
    /// Over-counting is the safe direction: it shrinks the history budget rather
    /// than overflowing the window. Do not "fix" this to `chars().count()`, which
    /// removes a margin the Jetson relies on.
    #[test]
    fn multibyte_text_is_over_counted_which_is_the_safe_direction() {
        let ascii = HeuristicTokenCounter.count("aaaaaaaa");
        let cyrillic = HeuristicTokenCounter.count("аааааааа");
        assert!(
            cyrillic > ascii,
            "byte-based counting must over-estimate multi-byte text"
        );
    }
}
