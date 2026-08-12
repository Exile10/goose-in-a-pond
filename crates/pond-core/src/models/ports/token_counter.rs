//! Driven Port: Token Counting
//!
//! How many tokens a piece of text will occupy in the model's context window.
//!
//! Budget arithmetic is only as good as this number. Until PAI-3 P2 the answer
//! was `text.len() / 4` everywhere, corrected one turn late by comparing
//! against the engine's real prompt count — which means it was wrong for one
//! turn, every time the conversation's shape changed.
//!
//! # On exactness
//!
//! [`TokenCounter::is_exact`] is not decoration, and implementations must be
//! honest about it. A counter is exact only when it uses **the active model's
//! own vocabulary**. A different real tokenizer (tiktoken against a Gemma GGUF,
//! say) is much better than dividing by four and still not exact, because the
//! vocabularies differ.
//!
//! Callers use it to decide how much safety margin to keep: an exact count can
//! be budgeted to the token, an inexact one needs the overshoot-feedback
//! correction in `turn_trimmer` to stay behind it.
//!
//! Nothing may report `is_exact() == true` on the strength of being a real
//! tokenizer alone. It has to be the *right* tokenizer.

/// Counts tokens for the active model.
///
/// Implementations must be cheap enough to call once per message per turn —
/// this runs on the hard-real-time trim path, which never blocks and never
/// calls a model.
pub trait TokenCounter: Send + Sync {
    /// Tokens `text` will occupy, excluding any per-message envelope.
    fn count(&self, text: &str) -> usize;

    /// Whether [`count`](Self::count) uses the active model's own vocabulary.
    ///
    /// See the module docs: a real-but-different tokenizer reports `false`.
    fn is_exact(&self) -> bool;

    /// Short label for logs and telemetry, e.g. `"chars/4"`, `"tiktoken"`.
    fn name(&self) -> &'static str;
}
