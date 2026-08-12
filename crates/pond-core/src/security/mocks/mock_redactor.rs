//! Test double for [`Redactor`]. Replaces one configured needle, records
//! every call, and honours [`RedactionLevel`] exactly as a real adapter must.

use std::sync::Mutex;

use crate::security::domain::redaction::{Redacted, RedactionKind, RedactionLevel};
use crate::security::ports::redactor::Redactor;

pub struct MockRedactor {
    needle: String,
    kind: RedactionKind,
    calls: Mutex<Vec<(String, RedactionLevel)>>,
}

impl MockRedactor {
    pub fn replacing(needle: &str, kind: RedactionKind) -> Self {
        Self {
            needle: needle.to_string(),
            kind,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Every `(text, level)` this was asked to redact, in order.
    pub fn calls(&self) -> Vec<(String, RedactionLevel)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Redactor for MockRedactor {
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted {
        self.calls.lock().unwrap().push((text.to_string(), level));
        if !text.contains(&self.needle) {
            return Redacted::unchanged(text);
        }
        let findings = vec![self.kind];
        if !level.redacts(self.kind) {
            return Redacted {
                text: text.to_string(),
                findings,
            };
        }
        Redacted {
            text: text.replace(&self.needle, self.kind.placeholder()),
            findings,
        }
    }
}
