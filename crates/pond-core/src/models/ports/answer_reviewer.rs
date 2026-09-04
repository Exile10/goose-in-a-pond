//! AnswerReviewer port — driven port for post-inference adversarial review. A critic evaluates
//! the generated answer; below threshold its critique goes back to the main LLM for revision.
//! The reviewer uses the SAME model: the difference is the system prompt, not the model.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The outcome of a single review evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewVerdict {
    /// Whether the answer passes the quality bar.
    pub pass: bool,
    /// Quality score 1-5 (1 = unusable, 5 = excellent).
    pub score: u8,
    /// What the reviewer expected the answer to contain.
    #[serde(default)]
    pub expectations: Vec<String>,
    /// Specific critique of what is missing, wrong, or weak.
    /// Empty when `pass` is true.
    #[serde(default)]
    pub critique: String,
}

/// The result of the full review process (review + optional revision).
#[derive(Debug, Clone)]
pub struct ReviewResult {
    /// The final answer text (original if passed, revised if not).
    pub final_answer: String,
    /// Whether the answer was revised (critique triggered a rewrite).
    pub was_revised: bool,
    /// The verdict from the last review round.
    pub verdict: ReviewVerdict,
    /// Number of review rounds executed.
    pub rounds: u32,
}

/// Driven Port: post-inference adversarial answer reviewer. Reviews a completed answer against
/// the original question using the same LlmProvider with a critic system prompt, and may trigger
/// a revision. Callers use `ReviewResult::final_answer` for persistence and delivery.
#[async_trait]
pub trait AnswerReviewer: Send + Sync {
    /// Review a completed answer against the original question. `question` is the user's message,
    /// `answer` the main LLM's response, `tool_context` any tool-retrieved data that was injected.
    async fn review(
        &self,
        question: &str,
        answer: &str,
        tool_context: Option<&str>,
    ) -> Result<ReviewResult>;
}
