//! Model role enum — identifies which LLM role should handle a request.

/// The three model roles GIAP can dispatch to.
///
/// Each role maps to an independently configured provider + model in `Settings`.
/// Roles fall back to `Chat` if not separately configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRole {
    /// Fast conversational model for everyday queries.
    Chat,
    /// Deeper reasoning model for analysis, comparison, explanation.
    Think,
    /// Agentic tool-use model for scheduling, device control, search.
    Task,
}
