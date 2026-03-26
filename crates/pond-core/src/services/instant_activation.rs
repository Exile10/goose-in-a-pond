//! InstantActivation — no-op WakeWordDetector for keyboard / stdin mode.
//!
//! Returns immediately from `wait_for_activation()` so the workflow loop
//! transitions straight from Wait to Listen without pausing.
//! Used as the default in `ChatService` and in all tests.

use crate::ports::wake_word::WakeWordDetector;
use anyhow::Result;
use async_trait::async_trait;

/// No-op wake-word detector: activates instantly without waiting.
///
/// Use this when the user drives the loop via keyboard (stdin) or when
/// tests need deterministic, non-blocking behaviour.
pub struct InstantActivation;

#[async_trait]
impl WakeWordDetector for InstantActivation {
    async fn wait_for_activation(&self) -> Result<()> {
        Ok(())
    }

    fn activation_prompt(&self) -> &str {
        "Type your message"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn instant_activation_returns_ok_immediately() {
        let detector = InstantActivation;
        assert!(detector.wait_for_activation().await.is_ok());
    }

    #[tokio::test]
    async fn instant_activation_prompt_is_non_empty() {
        let detector = InstantActivation;
        assert!(!detector.activation_prompt().is_empty());
    }
}
