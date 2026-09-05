//! Driven Port: Token Counting — how many tokens text occupies in the model's context window,
//! and the basis of all budget arithmetic. [`TokenCounter::is_exact`] is true ONLY for the active
//! model's own vocabulary: a real-but-different tokenizer is still inexact. Callers size the
//! safety margin from it — an inexact count needs `turn_trimmer`'s overshoot-feedback correction.

/// Counts tokens for the active model. Implementations must be cheap enough to call once per
/// message per turn: this runs on the hard-real-time trim path, which never blocks and never
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
