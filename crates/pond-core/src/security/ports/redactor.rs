//! Driven Port: removal of personal data before it is stored or sent.
//!
//! Synchronous and not a model. Invariant 3 of PAI-2 applies: never run this over the
//! model's own prompt; it protects the durable stores and anything leaving the pond.

use crate::security::domain::redaction::{Redacted, RedactionLevel};

pub trait Redactor: Send + Sync {
    /// Return `text` with detected personal data replaced, plus what was found.
    /// Must be idempotent: redacting twice equals redacting once.
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted;
}
