use crate::models::domain::message::ImageAttachment;
use crate::user_data::domain::session::{
    MessageAttachment, Session, SessionIdentity, SessionMessage,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionStorageError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Message not found: {0}")]
    MessageNotFound(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("General error: {0}")]
    General(String),
}

/// Driven Port: SessionStorage
///
/// This trait defines the interface for persisting conversation sessions and messages.
/// Implementations can range from in-memory storage to database-backed persistence.
#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    /// Create a new session.
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError>;

    /// Get a session by ID.
    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError>;

    /// Add a message to a session.
    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError>;

    /// Get all messages for a session.
    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Update the title of a session.
    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError>;

    /// Delete a session and all its messages.
    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError>;

    /// List all sessions, ordered by most recently updated first.
    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError>;

    /// Get messages for a session with pagination.
    ///
    /// Returns up to `limit` messages starting from `offset`, ordered chronologically.
    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Fetch the most recent `limit` messages, returned in chronological order
    /// (oldest-first). Use this instead of `get_messages()` to cap context load.
    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Increment the cumulative token usage for a session.
    async fn increment_usage(
        &self,
        _session_id: &str,
        _prompt_tokens: u32,
        _completion_tokens: u32,
        _model_name: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Count the messages stored for a session.
    ///
    /// Used to render a per-conversation badge in the history sidebar.
    /// The default returns 0 so in-memory mocks and legacy adapters keep
    /// compiling; real adapters override with an indexed `COUNT(*)`.
    async fn count_messages(&self, _session_id: &str) -> Result<u64, SessionStorageError> {
        Ok(0) // default no-op for backward compat
    }

    /// Return the content of the earliest user message in a session, if any.
    ///
    /// Used as a read-time fallback to derive a human-readable label when a
    /// session has no stored `title`. The default returns `None` so mocks and
    /// legacy adapters keep compiling.
    async fn first_user_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// Read the session's rolling conversation summary and the id of the last
    /// message it covers. `(None, None)` when no summary exists yet.
    async fn get_rolling_summary(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok((None, None)) // default no-op for backward compat
    }

    /// Store a refreshed rolling summary. `through_message_id` is the id of
    /// the newest message the summary covers.
    async fn set_rolling_summary(
        &self,
        _session_id: &str,
        _summary: &str,
        _through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// The agent engine's own session id paired with this GIAP session, if one
    /// has been recorded.
    ///
    /// Engines keep their own conversation store under ids they generate
    /// themselves. Without a persisted pairing, a server restart makes an
    /// existing GIAP chat resolve to a brand-new empty engine session and the
    /// model loses the whole conversation even though every message is still in
    /// `pond_system.db`. The id is engine-generated and opaque here — callers
    /// must re-validate it against the engine before use, because the engine's
    /// store can be wiped independently of ours.
    async fn get_engine_session_id(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// The GIAP session paired to an engine session id, if any.
    ///
    /// The inverse of [`get_engine_session_id`](Self::get_engine_session_id).
    /// Needed because a builtin MCP tool call carries the ENGINE's session id
    /// in its request `_meta`, and a draft decision has to resolve that to a
    /// speaker.
    ///
    /// The default returns `Ok(None)` -- "unresolvable". That is the narrowing
    /// answer, so a mock or legacy adapter that does not override it causes a
    /// refusal, never a permission.
    async fn get_session_id_for_engine(
        &self,
        _engine_session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// Record the engine session paired with this GIAP session (idempotent
    /// upsert). Deliberately not keyed to a `sessions` row: the pairing is also
    /// established on paths (direct tool calls, the voice child) that can run
    /// before a GIAP session row exists.
    async fn set_engine_session_id(
        &self,
        _session_id: &str,
        _engine_session_id: &str,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Who this session is attributed to, and on what evidence.
    ///
    /// Returns [`SessionIdentity::unknown`] for a session that has never been
    /// identified, which today is every session in every existing pond. That
    /// is a real answer, not a missing one: "nobody has been identified" is
    /// exactly what the caller needs to know, and returning it rather than an
    /// `Option` removes the temptation to treat absence as permission.
    ///
    /// An unknown session id also reads as unattributed rather than erroring.
    /// A read asking "whose session is this" has a correct answer for a session
    /// that does not exist, and it is "nobody".
    async fn get_session_identity(
        &self,
        _session_id: &str,
    ) -> Result<SessionIdentity, SessionStorageError> {
        Ok(SessionIdentity::unknown()) // default no-op for backward compat
    }

    /// Record who a session belongs to.
    ///
    /// This does NOT decide whether the new identification should win over
    /// whatever is already stored -- that is a policy question and it lives in
    /// [`SessionIdentity::supersedes`], in the domain. An adapter that made the
    /// choice itself would put the rule beyond the reach of a pond-core test.
    ///
    /// Unlike the tool-group and engine-session pairings, this one is stored on
    /// the `sessions` row itself, so it genuinely requires the row to exist.
    /// Implementations return [`SessionStorageError::SessionNotFound`] rather
    /// than succeeding silently -- an attribution that was accepted and then
    /// discarded is the failure mode this whole phase exists to end.
    async fn set_session_identity(
        &self,
        _session_id: &str,
        _identity: &SessionIdentity,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Write an identity **only if** it is at least as strong as what is stored.
    ///
    /// The read-compare-write in the handlers is not safe on its own. Two
    /// requests can both read `Unknown` and both pass
    /// [`SessionIdentity::supersedes`], after which the later write wins
    /// whatever its rank -- so a face match landing a millisecond after a
    /// member tapped "this is Liz" takes the session, for a different person,
    /// on weaker evidence. That is the exact downgrade `supersedes` exists to
    /// refuse, and it is reachable today.
    ///
    /// Implementations must do the comparison inside the write itself. Returns
    /// `true` when the write happened, `false` when a stronger identification
    /// already held the session -- which is a normal outcome, not an error.
    ///
    /// The default implementation is **not** race-free; it falls back to the
    /// unconditional write so mocks and legacy adapters keep compiling. Real
    /// adapters override it.
    async fn set_session_identity_if_stronger(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<bool, SessionStorageError> {
        let existing = self.get_session_identity(session_id).await?;
        if !identity.supersedes(&existing) {
            return Ok(false);
        }
        self.set_session_identity(session_id, identity).await?;
        Ok(true)
    }

    /// The tool GROUPS (MCP extension names) selected for this session, if any.
    ///
    /// Phase D2 chooses a session's tool surface once, from its opening message,
    /// and keeps it stable so the local engine's KV prompt prefix stays reusable
    /// across turns. Persisting it matters for one specific reason: the model can
    /// widen its own surface mid-session via `enable_tool_group`, and a
    /// process-local record would silently drop that capability on the next
    /// restart, mid-conversation, with no way for the user to tell why.
    ///
    /// `None` means "not chosen yet" — the caller selects and stores.
    async fn get_session_tool_groups(
        &self,
        _session_id: &str,
    ) -> Result<Option<Vec<String>>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// Record this session's tool groups (idempotent upsert, replaces the list).
    ///
    /// Deliberately not keyed to a `sessions` row, for the same reason as
    /// [`set_engine_session_id`]: selection also happens on paths that run before
    /// a GIAP `sessions` row exists.
    ///
    /// [`set_engine_session_id`]: SessionStorage::set_engine_session_id
    async fn set_session_tool_groups(
        &self,
        _session_id: &str,
        _groups: &[String],
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    // ── Image attachments (phase F2) ────────────────────────────────────────
    //
    // Attachments are written implicitly: `add_message` persists whatever is on
    // `SessionMessage.message.images`. They are read back EXPLICITLY, through
    // the three methods below, and never inflated into `get_messages` — a
    // session-history read happens on every turn and on every UI load, and
    // silently base64-inflating every image a conversation ever contained would
    // turn a cheap read into a multi-megabyte one.

    /// Metadata for every attachment in a session, chronological then by
    /// ordinal. Cheap: no bytes are read.
    ///
    /// Callers use this to decide WHICH images are worth loading (see
    /// `models::services::context::image_history::plan_image_replay`) before
    /// paying for any of them.
    async fn list_session_attachments(
        &self,
        _session_id: &str,
    ) -> Result<Vec<MessageAttachment>, SessionStorageError> {
        Ok(Vec::new()) // default no-op for backward compat
    }

    /// Load the base64 image payloads for specific messages, keyed by message
    /// id, with each message's images in ordinal order.
    ///
    /// Batched deliberately: history replay needs a handful of images chosen
    /// across a whole conversation, and one call per image would be one file
    /// read plus one query per image.
    async fn load_message_images(
        &self,
        _message_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<ImageAttachment>>, SessionStorageError> {
        Ok(std::collections::HashMap::new()) // default no-op for backward compat
    }

    /// Read one attachment's raw (decoded) bytes and MIME type.
    ///
    /// Serves the desktop's `<img>` requests, so it returns bytes rather than
    /// base64 — re-encoding only to have the browser decode again is pure waste.
    async fn read_attachment(
        &self,
        _attachment_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }
}
