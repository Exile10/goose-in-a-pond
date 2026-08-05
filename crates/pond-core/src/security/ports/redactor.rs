//! Driven Port: removal of personal data before it is stored or sent.
//!
//! Deliberately synchronous and deliberately not a model. Invariant 3 of PAI-2
//! also applies: this is never run over the model's own prompt. What it
//! protects is the durable stores and anything that leaves the pond.

use crate::security::domain::redaction::{Redacted, RedactionLevel};

pub trait Redactor: Send + Sync {
    /// Return `text` with detected personal data replaced, plus what was found.
    /// Must be idempotent: redacting twice equals redacting once.
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted;
}
