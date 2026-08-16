//! Idle re-titling — giving a conversation a name worth finding again.
//!
//! A session gets its first title as soon as it has a user message: the first
//! six words of that message, verbatim, from `derive_title_from_text`. That is
//! a reliable label and a poor name. "so i was wondering whether" tells you
//! nothing from the sidebar three weeks later.
//!
//! This service replaces those with something recognisable at a glance, and it
//! only ever runs when the machine is otherwise doing nothing. On an 8GB
//! Jetson there is one inference slot, and a title is never worth taking it
//! from a person: every model call here races a [`CancellationToken`] and
//! persists nothing if it loses.
//!
//! # What it will and will not rename
//!
//! Provenance is recorded per title (`sessions.title_source`) because the three
//! kinds have to be treated differently:
//!
//! - `derived` — the six-word fallback. Always eligible.
//! - `model` — written by this service. Eligible again only once the
//!   conversation has moved substantially past what the title covers, so a chat
//!   that began about DNS and became about the Jetson stops being filed under
//!   DNS.
//! - `user` — someone typed it. Never touched, at any interval, for any reason.
//!
//! A row written before the provenance column existed reads as `None`, and is
//! treated as `user` **unless** its title is exactly what the fallback would
//! have produced. That asymmetry is deliberate: guessing wrong in the lenient
//! direction silently overwrites a name a person chose, and there is no undo
//! for that. Guessing wrong in the strict direction leaves a mediocre title
//! alone, which is the failure everyone can live with.

use std::sync::Arc;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::models::domain::message::{ChatMessage, Role};
use crate::models::ports::provider::LlmProvider;
use crate::user_data::ports::session_storage::SessionStorage;

/// Hard ceiling on a title, as asked for: ten words, never eleven.
pub const MAX_TITLE_WORDS: usize = 10;

/// Character cap, so a title of ten very long words still fits the sidebar.
pub const MAX_TITLE_CHARS: usize = 72;

/// Past this, the model did not answer the question it was asked — it wrote a
/// sentence, or explained itself. Truncating that to ten words produces a
/// confident-looking fragment, which is worse than keeping the dull title we
/// already had, so the response is rejected whole.
const REJECT_OVER_WORDS: usize = 20;

/// A session needs at least this many messages before it is worth naming. One
/// lone user message is exactly what the six-word fallback already handles.
pub const MIN_MESSAGES_TO_TITLE: usize = 3;

/// How far a conversation must move past its current model-written title
/// before that title is rebuilt.
pub const REFRESH_AFTER_MESSAGES: usize = 8;

/// How many recent messages to show the model when there is no rolling summary.
const EVIDENCE_MESSAGES: usize = 10;

/// Per-message truncation for that evidence — enough to carry the subject,
/// bounded so a long paste cannot blow the context on a small board.
const EVIDENCE_CHARS_PER_MESSAGE: usize = 280;

/// Who last wrote `sessions.title`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    /// The deterministic six-word fallback.
    Derived,
    /// This service.
    Model,
    /// A person, through the rename endpoint.
    User,
}

impl TitleSource {
    /// Stable string for the database column.
    pub fn as_str(self) -> &'static str {
        match self {
            TitleSource::Derived => "derived",
            TitleSource::Model => "model",
            TitleSource::User => "user",
        }
    }

    /// Parse the column back. An unrecognised value reads as `None`, which the
    /// gate treats with the same suspicion as a legacy row.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "derived" => Some(TitleSource::Derived),
            "model" => Some(TitleSource::Model),
            "user" => Some(TitleSource::User),
            _ => None,
        }
    }
}

/// Everything the gate needs, as plain values, so the policy is testable
/// without a database or a model.
#[derive(Debug, Clone, Copy)]
pub struct TitleGateInputs<'a> {
    /// The title as stored, if any.
    pub title: Option<&'a str>,
    /// `sessions.title_source`, or `None` for a row that predates the column.
    pub source: Option<TitleSource>,
    /// What `derive_title_from_text` would produce from the first user message
    /// right now. Used only to identify a legacy fallback title.
    pub derived_fallback: Option<&'a str>,
    /// Total messages in the session.
    pub total_messages: usize,
    /// Messages added since the id the current model title covers. Meaningless
    /// unless `source` is [`TitleSource::Model`].
    pub messages_since_covered: usize,
}

/// Why a session was left alone this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Not enough conversation to name yet.
    TooShort,
    /// Someone typed this name. Off limits.
    UserNamed,
    /// A row older than the provenance column whose title is not the fallback,
    /// so it is assumed to have been chosen deliberately.
    UnknownProvenance,
    /// Model-written and still current enough.
    StillCurrent,
}

impl SkipReason {
    /// Short, stable label for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::TooShort => "too_short",
            SkipReason::UserNamed => "user_named",
            SkipReason::UnknownProvenance => "unknown_provenance",
            SkipReason::StillCurrent => "still_current",
        }
    }
}

/// The gate's verdict for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleDecision {
    Retitle,
    Skip(SkipReason),
}

impl TitleDecision {
    pub fn is_retitle(self) -> bool {
        matches!(self, TitleDecision::Retitle)
    }
}

/// Decide whether this session should be renamed now.
///
/// Ordered so the protective rules run before the permissive ones: a
/// user-named session is refused before anything else gets a chance to
/// consider it eligible.
pub fn should_retitle(inputs: TitleGateInputs<'_>) -> TitleDecision {
    if inputs.total_messages < MIN_MESSAGES_TO_TITLE {
        return TitleDecision::Skip(SkipReason::TooShort);
    }

    match inputs.source {
        Some(TitleSource::User) => TitleDecision::Skip(SkipReason::UserNamed),

        Some(TitleSource::Derived) => TitleDecision::Retitle,

        Some(TitleSource::Model) => {
            if inputs.messages_since_covered >= REFRESH_AFTER_MESSAGES {
                TitleDecision::Retitle
            } else {
                TitleDecision::Skip(SkipReason::StillCurrent)
            }
        }

        // Legacy row, or an unrecognised column value. An empty title is
        // nobody's deliberate choice, so it is safe to fill. Otherwise the
        // title is only touched when it is byte-identical to what the fallback
        // would have written — that is the one case where we know no person
        // composed it.
        None => match (inputs.title, inputs.derived_fallback) {
            (None, _) => TitleDecision::Retitle,
            (Some(t), _) if t.trim().is_empty() => TitleDecision::Retitle,
            (Some(t), Some(fallback)) if t == fallback => TitleDecision::Retitle,
            _ => TitleDecision::Skip(SkipReason::UnknownProvenance),
        },
    }
}

/// Clean up whatever the model said and decide whether it is usable as a title.
///
/// Models reliably do three things the prompt asked them not to: wrap the
/// answer in quotes, prefix it with "Title:", and add a full stop. Rejecting
/// those would throw away good names over punctuation, so they are stripped.
/// Length is different — a model that returns a paragraph did not understand
/// the task, and truncating that produces a fragment that reads like a real
/// title while meaning something else. Those are refused outright.
pub fn normalise_title(raw: &str) -> Option<String> {
    // Only ever the first line. Anything after it is the model explaining
    // itself, which is not part of the name.
    let first_line = raw.lines().find(|l| !l.trim().is_empty())?;

    let mut cleaned = first_line.trim();

    // "Title: ..." / "title - ..." — a label, not part of the name.
    for prefix in ["chat title:", "title:", "title -", "name:"] {
        if let Some(rest) = strip_prefix_ci(cleaned, prefix) {
            cleaned = rest.trim();
            break;
        }
    }

    let cleaned = cleaned
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('*')
        .trim()
        .trim_end_matches('.')
        .trim();

    if cleaned.is_empty() {
        return None;
    }

    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    // The model wrote prose rather than a name. Keep the old title.
    if words.len() > REJECT_OVER_WORDS {
        return None;
    }

    let mut title = words
        .iter()
        .take(MAX_TITLE_WORDS)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");

    // Char cap, on a word boundary where one is available, so the result never
    // ends mid-word.
    if title.chars().count() > MAX_TITLE_CHARS {
        let truncated: String = title.chars().take(MAX_TITLE_CHARS).collect();
        title = match truncated.rsplit_once(' ') {
            Some((head, _)) if !head.trim().is_empty() => head.trim().to_string(),
            _ => truncated.trim().to_string(),
        };
    }

    // A title made only of punctuation is not a title.
    if !title.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }

    Some(title)
}

/// Case-insensitive prefix strip that cannot panic on a multi-byte character.
///
/// `str::get` returns `None` rather than panicking when the range lands inside
/// a character, which is the whole reason it is used here: slicing
/// `s[..prefix.len()]` directly crashed on any title starting with a
/// multi-byte character close enough to the front — "Déjà vu…" against the
/// five-byte prefix "name:" cuts the "à" in half.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

/// The instruction. Written to be read by a small on-device model, so the
/// rules are short, positive where possible, and each one is demonstrated.
///
/// Public because the chat service names a conversation after its first
/// exchange too, and two prompts for one job is how the two paths drift into
/// disagreeing about what a title is.
pub const TITLE_SYSTEM_PROMPT: &str = "\
You name conversations. Reply with the name and nothing else.

- Ten words at most. Fewer is better.
- Name the real subject, so it is recognisable in a list weeks from now.
- Be concrete. \"Wake word fires twice on the Jetson\" beats \"Technical discussion\".
- Sentence case. No quotes, no full stop, no \"Chat about\".
- If it covers several things, name the one it kept coming back to.";

/// What one attempt at renaming a session did.
#[derive(Debug, PartialEq, Eq)]
pub enum RetitleOutcome {
    /// Renamed and persisted.
    Retitled {
        title: String,
        through_message_id: String,
    },
    /// The gate declined.
    Skipped(SkipReason),
    /// A person came back. Nothing was written.
    Cancelled,
    /// The model answered with something unusable. The old title stands.
    Unusable,
}

/// Renames one session, when the gate allows it.
pub struct SessionTitleService {
    provider: Arc<dyn LlmProvider>,
    storage: Arc<dyn SessionStorage>,
}

impl SessionTitleService {
    pub fn new(provider: Arc<dyn LlmProvider>, storage: Arc<dyn SessionStorage>) -> Self {
        Self { provider, storage }
    }

    /// Consider one session, and rename it if it qualifies.
    ///
    /// Every rule applies: a name typed by hand is refused, a model-written
    /// name that still fits is left alone, a conversation too short to describe
    /// is left to the fallback. This is what both sweeps use, because a sweep
    /// acts on conversations nobody is looking at.
    ///
    /// Cancellation is checked before the model call, raced against it, and
    /// checked again after — a cancelled attempt writes nothing at all, so
    /// interrupting can never leave a half-considered name behind.
    pub async fn retitle(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RetitleOutcome> {
        self.run(session_id, false, cancel).await
    }

    /// Rename one named conversation because somebody asked for that one.
    ///
    /// Skips "already current" and replaces a name typed by hand. The
    /// protections exist because a sweep touches conversations out of sight; a
    /// click on the conversation in front of you is consent about that
    /// conversation, and a button that silently declined would be the worse
    /// behaviour. Still refuses a conversation too short to describe, because
    /// there the problem is that there is nothing to say, not permission.
    pub async fn retitle_now(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RetitleOutcome> {
        self.run(session_id, true, cancel).await
    }

    async fn run(
        &self,
        session_id: &str,
        force: bool,
        cancel: &CancellationToken,
    ) -> Result<RetitleOutcome> {
        if cancel.is_cancelled() {
            return Ok(RetitleOutcome::Cancelled);
        }

        let session = self.storage.get_session(session_id).await?;
        let (source_raw, covered_through) = self.storage.get_title_provenance(session_id).await?;
        let source = source_raw.as_deref().and_then(TitleSource::parse);

        // Everything the gate needs, by cheap queries only.
        //
        // This runs over every conversation on the pond every five minutes, and
        // its steady state is "already named, nothing new since" — so the
        // history is deliberately NOT loaded until the gate has said yes.
        // Loading it first, which is the obvious way to write this, means
        // re-reading every message of every conversation forever to be told
        // nothing needs doing.
        //
        // Note `count_messages` defaults to 0 on an adapter that does not
        // override it, which reads as `TooShort` and skips. That is the
        // narrowing direction — a storage backend that cannot count cheaply
        // declines to be re-titled rather than being re-titled wrongly.
        let total_messages = self.storage.count_messages(session_id).await? as usize;

        let messages_since_covered = match (source, covered_through.as_deref()) {
            (Some(TitleSource::Model), Some(through)) => self
                .storage
                .messages_after(session_id, through)
                .await?
                // The covered message is gone (deleted, or a rebuilt history).
                // Treat the title as covering nothing rather than as current.
                .map_or(total_messages, |n| n as usize),
            _ => total_messages,
        };

        // Only a row with no recorded provenance needs this, and only so the
        // gate can recognise the six-word fallback and rule out a human name.
        let derived_fallback = match source {
            None => self
                .storage
                .first_user_message(session_id)
                .await?
                .map(|text| derive_fallback_title(&text)),
            _ => None,
        };

        let decision = should_retitle(TitleGateInputs {
            title: session.title.as_deref(),
            source,
            derived_fallback: derived_fallback.as_deref(),
            total_messages,
            messages_since_covered,
        });

        if let TitleDecision::Skip(reason) = decision {
            // A forced run overrides permission, never possibility. `TooShort`
            // is the one refusal that is about there being nothing to describe
            // rather than about who is allowed to describe it.
            let overridable = force && reason != SkipReason::TooShort;
            if !overridable {
                return Ok(RetitleOutcome::Skipped(reason));
            }
        }

        // The gate said yes, so the history is now worth its read.
        let messages = self.storage.get_messages(session_id).await?;
        let Some(newest) = messages.last() else {
            return Ok(RetitleOutcome::Skipped(SkipReason::TooShort));
        };
        let through_message_id = newest.id.clone();

        // Prefer the rolling summary: it is already a condensed account of this
        // conversation, it costs nothing extra to reuse, and on a small board
        // that is the difference between a cheap call and an expensive one.
        let (summary, _) = self.storage.get_rolling_summary(session_id).await?;
        let evidence = match summary {
            Some(s) if !s.trim().is_empty() => {
                format!("Summary of the conversation:\n{}", s.trim())
            }
            _ => transcript_evidence(&messages),
        };

        if cancel.is_cancelled() {
            return Ok(RetitleOutcome::Cancelled);
        }

        let prompt = vec![ChatMessage::user(format!(
            "{evidence}\n\nName this conversation."
        ))];

        // The model call is the long pole. Race it, so a returning user
        // reclaims the serial on-device engine immediately.
        let response = tokio::select! {
            r = self.provider.complete(TITLE_SYSTEM_PROMPT, prompt) => r?,
            _ = cancel.cancelled() => return Ok(RetitleOutcome::Cancelled),
        };

        if cancel.is_cancelled() {
            return Ok(RetitleOutcome::Cancelled);
        }

        let Some(title) = normalise_title(&response.content) else {
            return Ok(RetitleOutcome::Unusable);
        };

        self.storage
            .set_generated_title(session_id, &title, &through_message_id)
            .await?;

        Ok(RetitleOutcome::Retitled {
            title,
            through_message_id,
        })
    }
}

/// The six-word fallback, reproduced here so the gate can recognise one.
///
/// Deliberately a copy of `ChatService::derive_title_from_text`'s rule rather
/// than a call into it: that function is private to the chat service, and the
/// two have different jobs. If the fallback rule ever changes, the only cost
/// here is that a legacy title stops being recognised as a fallback and is
/// therefore left alone — the safe direction.
fn derive_fallback_title(text: &str) -> String {
    const MAX_WORDS: usize = 6;
    const MAX_CHARS: usize = 60;

    let cleaned = text.trim().trim_matches('"').trim_matches('\'');
    let title = cleaned
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim().trim_matches('"').trim_matches('\'').trim();

    title.chars().take(MAX_CHARS).collect()
}

/// Build the transcript excerpt shown to the model when no summary exists.
///
/// The first user message plus the tail, because the opening says what the
/// conversation was for and the tail says what it became — and those are the
/// two things a good name has to reconcile.
fn transcript_evidence(messages: &[crate::user_data::domain::session::SessionMessage]) -> String {
    let mut lines: Vec<String> = Vec::new();

    if let Some(first_user) = messages.iter().find(|m| m.message.role == Role::User) {
        lines.push(format!(
            "Opened with: {}",
            truncate(&first_user.message.content, EVIDENCE_CHARS_PER_MESSAGE)
        ));
    }

    let tail_start = messages.len().saturating_sub(EVIDENCE_MESSAGES);
    for m in &messages[tail_start..] {
        let who = match m.message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            _ => continue,
        };
        let body = truncate(&m.message.content, EVIDENCE_CHARS_PER_MESSAGE);
        if !body.trim().is_empty() {
            lines.push(format!("{who}: {body}"));
        }
    }

    lines.join("\n")
}

/// Truncate on a char boundary, marking that something was cut so the model
/// does not read a severed sentence as a finished thought.
fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    format!("{}…", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TitleGateInputs<'static> {
        TitleGateInputs {
            title: Some("so i was wondering whether"),
            source: Some(TitleSource::Derived),
            derived_fallback: Some("so i was wondering whether"),
            total_messages: 6,
            messages_since_covered: 0,
        }
    }

    // ── the gate ────────────────────────────────────────────────────────────

    #[test]
    fn a_fallback_title_is_replaced() {
        assert_eq!(should_retitle(base()), TitleDecision::Retitle);
    }

    /// The rule the whole provenance column exists to enforce.
    #[test]
    fn a_name_someone_typed_is_never_touched() {
        let inputs = TitleGateInputs {
            title: Some("Jetson deploy notes"),
            source: Some(TitleSource::User),
            // Even when the conversation has moved on enormously.
            messages_since_covered: 500,
            total_messages: 500,
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::UserNamed)
        );
    }

    #[test]
    fn a_short_session_is_left_to_the_fallback() {
        let inputs = TitleGateInputs {
            total_messages: MIN_MESSAGES_TO_TITLE - 1,
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::TooShort)
        );
    }

    #[test]
    fn a_model_title_holds_until_the_conversation_moves_on() {
        let inputs = TitleGateInputs {
            title: Some("Wake word fires twice on the Jetson"),
            source: Some(TitleSource::Model),
            messages_since_covered: REFRESH_AFTER_MESSAGES - 1,
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::StillCurrent)
        );
    }

    #[test]
    fn a_model_title_is_rebuilt_once_the_conversation_moves_on() {
        let inputs = TitleGateInputs {
            title: Some("Wake word fires twice on the Jetson"),
            source: Some(TitleSource::Model),
            messages_since_covered: REFRESH_AFTER_MESSAGES,
            ..base()
        };
        assert_eq!(should_retitle(inputs), TitleDecision::Retitle);
    }

    // ── legacy rows, where the asymmetry lives ──────────────────────────────

    /// A pre-migration row whose title is exactly the fallback: we know no
    /// person wrote that, so it is safe to improve.
    #[test]
    fn a_legacy_fallback_title_is_recognised_and_replaced() {
        let inputs = TitleGateInputs {
            title: Some("so i was wondering whether"),
            source: None,
            derived_fallback: Some("so i was wondering whether"),
            ..base()
        };
        assert_eq!(should_retitle(inputs), TitleDecision::Retitle);
    }

    /// The one that protects data: a pre-migration row with a title that is
    /// NOT the fallback was probably typed by someone, and is left alone.
    #[test]
    fn a_legacy_title_that_is_not_the_fallback_is_assumed_deliberate() {
        let inputs = TitleGateInputs {
            title: Some("Jetson deploy notes"),
            source: None,
            derived_fallback: Some("so i was wondering whether"),
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::UnknownProvenance)
        );
    }

    #[test]
    fn an_absent_or_blank_title_is_always_fillable() {
        for title in [None, Some(""), Some("   ")] {
            let inputs = TitleGateInputs {
                title,
                source: None,
                derived_fallback: Some("anything at all"),
                ..base()
            };
            assert_eq!(should_retitle(inputs), TitleDecision::Retitle);
        }
    }

    #[test]
    fn an_unrecognised_column_value_is_treated_as_legacy() {
        assert_eq!(TitleSource::parse("something-else"), None);
    }

    #[test]
    fn source_strings_round_trip() {
        for s in [TitleSource::Derived, TitleSource::Model, TitleSource::User] {
            assert_eq!(TitleSource::parse(s.as_str()), Some(s));
        }
    }

    // ── normalisation ───────────────────────────────────────────────────────

    #[test]
    fn the_ten_word_ceiling_is_absolute() {
        let raw = "one two three four five six seven eight nine ten eleven twelve";
        let title = normalise_title(raw).expect("usable");
        assert_eq!(title.split_whitespace().count(), MAX_TITLE_WORDS);
        assert!(title.starts_with("one two"));
    }

    #[test]
    fn a_short_answer_is_left_exactly_as_it_is() {
        assert_eq!(
            normalise_title("Wake word fires twice on the Jetson").as_deref(),
            Some("Wake word fires twice on the Jetson")
        );
    }

    #[test]
    fn the_decorations_models_add_are_stripped() {
        for raw in [
            "\"Wake word fires twice\"",
            "Title: Wake word fires twice",
            "title: \"Wake word fires twice\"",
            "**Wake word fires twice**",
            "Wake word fires twice.",
            "  Wake word fires twice  ",
        ] {
            assert_eq!(
                normalise_title(raw).as_deref(),
                Some("Wake word fires twice"),
                "failed to clean {raw:?}"
            );
        }
    }

    #[test]
    fn only_the_first_line_is_used() {
        let raw = "Wake word fires twice\n\nI chose this because the conversation was about…";
        assert_eq!(
            normalise_title(raw).as_deref(),
            Some("Wake word fires twice")
        );
    }

    /// A model that writes prose did not do the task. Truncating it to ten
    /// words would read like a real title while meaning something else, so the
    /// old title is kept instead.
    #[test]
    fn prose_is_refused_rather_than_truncated() {
        let raw = "Certainly, here is a suitable name for this particular conversation \
                   which covered a number of different topics over its course including \
                   several that were only briefly mentioned";
        assert_eq!(normalise_title(raw), None);
    }

    #[test]
    fn an_empty_or_punctuation_only_answer_is_refused() {
        for raw in ["", "   ", "\n\n", "\"\"", "...", "-- --"] {
            assert_eq!(normalise_title(raw), None, "should refuse {raw:?}");
        }
    }

    #[test]
    fn the_character_cap_never_ends_mid_word() {
        let raw = "Supercalifragilistic expialidocious antidisestablishmentarianism \
                   pneumonoultramicroscopicsilicovolcanoconiosis";
        let title = normalise_title(raw).expect("usable");
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        // Whatever survived, it is whole words from the original.
        for word in title.split_whitespace() {
            assert!(raw.contains(word), "{word:?} is not a word from the input");
        }
    }

    #[test]
    fn a_title_of_exactly_ten_words_survives_intact() {
        let raw = "one two three four five six seven eight nine ten";
        assert_eq!(normalise_title(raw).as_deref(), Some(raw));
    }

    /// The prefix strip used to slice by byte offset, so any title with a
    /// multi-byte character straddling one of the prefix lengths panicked and
    /// took the whole background job down with it.
    #[test]
    fn a_multi_byte_character_at_a_prefix_boundary_does_not_panic() {
        for raw in [
            "Déjà vu on the Jetson",
            "café",
            "日本語のタイトル",
            "Ω",
            "naïve retry logic",
            "—",
        ] {
            let _ = normalise_title(raw); // must not panic
        }
    }

    #[test]
    fn unicode_is_counted_by_character_not_byte() {
        let title = normalise_title("Déjà vu on the Jetson café build").expect("usable");
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        assert!(title.starts_with("Déjà vu"));
    }

    // ── evidence ────────────────────────────────────────────────────────────

    #[test]
    fn truncation_marks_that_it_cut() {
        assert_eq!(truncate("short", 40), "short");
        let long = "x".repeat(100);
        let cut = truncate(&long, 10);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), 11);
    }

    // ── the service, end to end ─────────────────────────────────────────────

    mod service {
        use super::*;
        use crate::user_data::domain::session::SessionMessage;
        use crate::user_data::mocks::mock_session::InMemorySessionStorage;
        use crate::user_data::ports::session_storage::SessionStorage;
        use async_trait::async_trait;
        use std::sync::Mutex;

        struct StubProvider {
            reply: String,
            calls: Mutex<usize>,
        }

        impl StubProvider {
            fn new(reply: &str) -> Arc<Self> {
                Arc::new(Self {
                    reply: reply.to_string(),
                    calls: Mutex::new(0),
                })
            }
            fn calls(&self) -> usize {
                *self.calls.lock().unwrap()
            }
        }

        #[async_trait]
        impl LlmProvider for StubProvider {
            async fn complete(
                &self,
                _system: &str,
                _messages: Vec<ChatMessage>,
            ) -> Result<ChatMessage> {
                *self.calls.lock().unwrap() += 1;
                Ok(ChatMessage::assistant(self.reply.clone()))
            }
            fn model_name(&self) -> String {
                "stub".to_string()
            }
        }

        async fn seed(storage: &InMemorySessionStorage, session: &str, n: usize) {
            storage.create_session(session.to_string()).await.unwrap();
            for i in 0..n {
                let msg = if i % 2 == 0 {
                    ChatMessage::user(format!("question {i}"))
                } else {
                    ChatMessage::assistant(format!("answer {i}"))
                };
                storage
                    .add_message(
                        session.to_string(),
                        SessionMessage::new(format!("m{i}"), session.to_string(), msg),
                    )
                    .await
                    .unwrap();
            }
        }

        #[tokio::test]
        async fn a_derived_title_is_replaced_and_its_reach_recorded() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 8).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            let provider = StubProvider::new("Wake word fires twice on the Jetson");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert_eq!(
                outcome,
                RetitleOutcome::Retitled {
                    title: "Wake word fires twice on the Jetson".to_string(),
                    through_message_id: "m7".to_string(),
                }
            );

            let session = storage.get_session("s").await.unwrap();
            assert_eq!(
                session.title.as_deref(),
                Some("Wake word fires twice on the Jetson")
            );
            assert_eq!(
                storage.get_title_provenance("s").await.unwrap(),
                (Some("model".to_string()), Some("m7".to_string()))
            );
        }

        /// The rule that matters most, and the cheapest way to prove it: a
        /// user-named session must not even reach the model.
        #[tokio::test]
        async fn a_user_named_session_costs_no_inference_at_all() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 40).await;
            storage
                .update_title("s", "Jetson deploy notes".to_string())
                .await
                .unwrap();

            let provider = StubProvider::new("Something else entirely");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert_eq!(outcome, RetitleOutcome::Skipped(SkipReason::UserNamed));
            assert_eq!(provider.calls(), 0, "the model must never have been asked");
            assert_eq!(
                storage.get_session("s").await.unwrap().title.as_deref(),
                Some("Jetson deploy notes"),
                "the name someone typed must survive untouched"
            );
        }

        #[tokio::test]
        async fn a_model_title_holds_then_is_rebuilt_as_the_conversation_moves_on() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 10).await;
            // Covers through m9 — the newest message. Nothing has moved on.
            storage
                .set_generated_title("s", "An early name", "m9")
                .await
                .unwrap();

            let provider = StubProvider::new("A later and better name");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            assert_eq!(
                svc.retitle("s", &CancellationToken::new()).await.unwrap(),
                RetitleOutcome::Skipped(SkipReason::StillCurrent)
            );
            assert_eq!(provider.calls(), 0);

            // The conversation carries on well past what the name covers.
            seed_more(&storage, "s", 10, REFRESH_AFTER_MESSAGES).await;

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert!(
                matches!(outcome, RetitleOutcome::Retitled { ref title, .. } if title == "A later and better name"),
                "got {outcome:?}"
            );
            assert_eq!(provider.calls(), 1);
        }

        async fn seed_more(storage: &InMemorySessionStorage, session: &str, from: usize, n: usize) {
            for i in from..from + n {
                storage
                    .add_message(
                        session.to_string(),
                        SessionMessage::new(
                            format!("m{i}"),
                            session.to_string(),
                            ChatMessage::user(format!("more {i}")),
                        ),
                    )
                    .await
                    .unwrap();
            }
        }

        #[tokio::test]
        async fn a_cancelled_pass_writes_nothing() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 8).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            let provider = StubProvider::new("A name that must never land");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let cancel = CancellationToken::new();
            cancel.cancel();

            assert_eq!(
                svc.retitle("s", &cancel).await.unwrap(),
                RetitleOutcome::Cancelled
            );
            assert_eq!(provider.calls(), 0);
            assert_eq!(
                storage.get_session("s").await.unwrap().title.as_deref(),
                Some("question 0"),
                "a cancelled pass must leave the old title exactly as it was"
            );
        }

        #[tokio::test]
        async fn an_unusable_answer_leaves_the_old_title_standing() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 8).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            // Prose, not a name — the model did not do the task.
            let provider = StubProvider::new(
                "Certainly, here is a suitable name for this particular conversation \
                 which ranged over a number of quite different topics during its course",
            );
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            assert_eq!(
                svc.retitle("s", &CancellationToken::new()).await.unwrap(),
                RetitleOutcome::Unusable
            );
            assert_eq!(
                storage.get_session("s").await.unwrap().title.as_deref(),
                Some("question 0")
            );
            assert_eq!(
                storage.get_title_provenance("s").await.unwrap(),
                (Some("derived".to_string()), None),
                "a refused answer must not claim the title as the model's"
            );
        }

        #[tokio::test]
        async fn too_short_a_conversation_is_left_to_the_fallback() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", MIN_MESSAGES_TO_TITLE - 1).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            let provider = StubProvider::new("A name");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            assert_eq!(
                svc.retitle("s", &CancellationToken::new()).await.unwrap(),
                RetitleOutcome::Skipped(SkipReason::TooShort)
            );
            assert_eq!(provider.calls(), 0);
        }

        /// A title whose covered message has since been deleted must read as
        /// covering nothing, not as still current — otherwise a compacted or
        /// rebuilt history would freeze its name forever.
        #[tokio::test]
        async fn a_title_pointing_at_a_vanished_message_is_rebuilt() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 12).await;
            storage
                .set_generated_title("s", "An early name", "no-such-message")
                .await
                .unwrap();

            let provider = StubProvider::new("A name built from what is actually there");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert!(
                matches!(outcome, RetitleOutcome::Retitled { .. }),
                "got {outcome:?}"
            );
        }
    }

    #[test]
    fn the_fallback_rule_matches_the_chat_services_shape() {
        assert_eq!(
            derive_fallback_title("  so I was wondering whether we could ship it "),
            "so I was wondering whether we"
        );
        assert_eq!(derive_fallback_title("   "), "");
    }
}
