//! Personal context streaming — the domain (PAI-8 P1). Invariants are section 5 of
//! `docs/architecture/pai/08-personal-context-streaming.md`, held by the types: `profile_id` is a
//! non-blank `String`, sensitivity is derived rather than supplied, and [`ContextItem::from_parts`]
//! is the only constructor and takes a [`Redactor`], so nothing reaches storage un-redacted.

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::security::domain::event::{EventCategory, PrivacySensitivity};
use crate::security::domain::redaction::{RedactionKind, RedactionLevel};
use crate::security::ports::redactor::Redactor;

/// How much the ingest chokepoint removes. `Secrets`, not `Full`: a context item is read back
/// into the model's context, and PAI-2 section 3.3's non-goal — the model is not blindfolded —
/// dies if names go too. Must stay equal to `RedactingMemoryRepository::LEVEL`; redacting at one
/// chokepoint and not the other spends the cost and buys nothing.
pub const INGEST_REDACTION_LEVEL: RedactionLevel = RedactionLevel::Secrets;

// ── Source kind ─────────────────────────────────────────────────────────────

/// Where a context source's data comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// A sensor already registered with this pond.
    Sensor,
    /// A camera already attached to this pond.
    Camera,
    /// A voice transcript this pond produced.
    Voice,
    /// The mobile companion (GOTG) pushing calendar, location or contacts.
    Mobile,
    /// An e-mail account.
    Mail,
    /// A calendar account.
    Calendar,
    /// A file or document store.
    Files,
    /// A chat account (Slack, Telegram, a user-run bridge).
    Chat,
}

/// Whether P1's pipeline will accept items from a kind, and if not, what has to land first.
///
/// [`IngestPipeline`](crate::context::ingest::IngestPipeline) refuses every kind that is not
/// [`Landed`](SourceAvailability::Landed), which is how PAI-8 section 3's phasing is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAvailability {
    /// Ingest works today. Data this pond already holds; nothing leaves.
    Landed,
    /// Waiting on `POST /api/v1/context/ingest` (PAI-8 P3). The pond never
    /// reaches out for these — a paired client pushes them — so no egress gate
    /// is involved, only an authenticated route that does not exist yet.
    AwaitingIngestRoute,
    /// Waiting on a connector that signs in to the account and only READS. Not a security gate:
    /// chokepoint 3 is discharged and PAI-8 §0 puts write-back out of scope. Kept because
    /// accepting a kind nothing can produce would let `upsert_source` mint a source that stays
    /// empty forever, and it lifts one kind at a time as each protocol's connector lands.
    AwaitingReadConnector,
}

impl SourceAvailability {
    /// Why a caller was refused, in a sentence naming the thing that is missing.
    pub fn refusal(&self) -> &'static str {
        match self {
            Self::Landed => "",
            Self::AwaitingIngestRoute => {
                "this source kind is pushed to the pond by a paired client, and the ingest \
                 route it would arrive on (PAI-8 P3) does not exist yet"
            }
            Self::AwaitingReadConnector => {
                "this source kind needs a connector that signs in to the account and reads \
                 it, and no connector for this protocol exists yet"
            }
        }
    }
}

impl SourceKind {
    /// Every variant. Adding one breaks this array's length and forces the
    /// availability, sensitivity and retention mappings below to be updated.
    pub const ALL: [SourceKind; 8] = [
        SourceKind::Sensor,
        SourceKind::Camera,
        SourceKind::Voice,
        SourceKind::Mobile,
        SourceKind::Mail,
        SourceKind::Calendar,
        SourceKind::Files,
        SourceKind::Chat,
    ];

    /// Stable name. This string is written to SQLite and read back, so it is
    /// part of the schema, not a display detail.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sensor => "sensor",
            Self::Camera => "camera",
            Self::Voice => "voice",
            Self::Mobile => "mobile",
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Files => "files",
            Self::Chat => "chat",
        }
    }

    /// Parse a stored kind. `None` for anything unrecognised — the storage
    /// adapter turns that into "no item", which narrows.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// What must land before P1's pipeline will accept this kind.
    pub fn availability(&self) -> SourceAvailability {
        match self {
            // Data the pond already holds. Ingesting it adds no egress.
            Self::Sensor | Self::Camera | Self::Voice => SourceAvailability::Landed,
            Self::Mobile => SourceAvailability::AwaitingIngestRoute,
            // The CalDAV adapter reads it, the connect route stores its
            // credentials and `calendar_sync` pulls it on a schedule. All three
            // had to exist before this line could move: a `Landed` kind with no
            // sync is a source that looks connected and stays empty.
            Self::Calendar | Self::Mail => SourceAvailability::Landed,
            Self::Files | Self::Chat => SourceAvailability::AwaitingReadConnector,
        }
    }

    /// Whether connecting this kind means signing in to an account.
    ///
    /// Stated on the kind rather than checked at the route, because the connect surface and the
    /// sync sweep both have to agree and a second copy is how they stop agreeing.
    pub fn needs_credentials(&self) -> bool {
        match self {
            Self::Sensor | Self::Camera | Self::Voice | Self::Mobile => false,
            Self::Mail | Self::Calendar | Self::Files | Self::Chat => true,
        }
    }

    /// The least sensitive an item from this kind may be classified.
    ///
    /// A floor, not a value: the redactor's findings can only push it up. Nothing here is
    /// `Public`, which on the events log means safe to surface anywhere.
    pub fn min_sensitivity(&self) -> PrivacySensitivity {
        match self {
            // A reading is a measurement until it is about a person; "the hall
            // sensor saw motion at 03:12" is about a person.
            Self::Sensor => PrivacySensitivity::Internal,
            Self::Camera | Self::Voice | Self::Mobile => PrivacySensitivity::Sensitive,
            Self::Mail | Self::Calendar | Self::Files | Self::Chat => PrivacySensitivity::Sensitive,
        }
    }

    /// Which [`EventCategory`]'s retention setting governs items from this kind.
    ///
    /// Reusing the events-log categories is deliberate: `retention_events_by_category` is the map
    /// the user already edits, so no second retention vocabulary appears.
    pub fn retention_category(&self) -> EventCategory {
        match self {
            Self::Sensor => EventCategory::Sensor,
            Self::Camera => EventCategory::Camera,
            // A transcript is something the agent produced from a turn.
            Self::Voice => EventCategory::Agent,
            Self::Mobile => EventCategory::Device,
            // Everything a connector fetched crossed the network to get here.
            Self::Mail | Self::Calendar | Self::Files | Self::Chat => EventCategory::Network,
        }
    }
}

// ── Item kind ───────────────────────────────────────────────────────────────

/// What sort of thing an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Message,
    Event,
    Document,
    Location,
    Task,
}

impl ItemKind {
    pub const ALL: [ItemKind; 5] = [
        ItemKind::Message,
        ItemKind::Event,
        ItemKind::Document,
        ItemKind::Location,
        ItemKind::Task,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Event => "event",
            Self::Document => "document",
            Self::Location => "location",
            Self::Task => "task",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }
}

// ── Source status ───────────────────────────────────────────────────────────

/// How a source is currently doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    Connected,
    NeedsReauth,
    Error,
    /// Deliberately distinct from `Error`. PAI-8 invariant 5: in
    /// `network_mode = offline` a connector reports `Paused`, because an
    /// operator who turned the network off does not want an error badge for it.
    Paused,
}

impl SourceStatus {
    pub const ALL: [SourceStatus; 4] = [
        SourceStatus::Connected,
        SourceStatus::NeedsReauth,
        SourceStatus::Error,
        SourceStatus::Paused,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::NeedsReauth => "needs_reauth",
            Self::Error => "error",
            Self::Paused => "paused",
        }
    }

    /// Parse a stored status. Anything unrecognised reads as [`Error`](Self::Error)
    /// rather than [`Connected`](Self::Connected): an unreadable status must not
    /// be the one that says "carry on syncing".
    pub fn parse(raw: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == raw)
            .unwrap_or(Self::Error)
    }
}

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    #[error("a context {0} needs an id")]
    BlankId(&'static str),
    #[error(
        "a context {0} must belong to a household member; a blank profile id is the `None` \
         this type exists to make unrepresentable"
    )]
    BlankOwner(&'static str),
    #[error("a context item must name the source it came from")]
    BlankSourceId,
    #[error(
        "a context item needs an external id, which is what makes re-syncing it idempotent \
         rather than duplicating it"
    )]
    BlankExternalId,
    #[error("a context item with neither a title nor a body carries nothing")]
    Empty,
    #[error("a context source needs a provider name")]
    BlankProvider,
}

// ── ContextSource ───────────────────────────────────────────────────────────

/// An account, sensor or device that produces context for one household member.
///
/// No token: PAI-8 invariant 4 puts connector credentials in the encrypted secret store, so this
/// type carries at most a `secret_ref` and the table has no column a token could be written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSource {
    id: String,
    kind: SourceKind,
    provider: String,
    profile_id: String,
    scopes: Vec<String>,
    cursor: Option<String>,
    last_sync: Option<DateTime<Utc>>,
    status: SourceStatus,
    secret_ref: Option<String>,
    created_at: DateTime<Utc>,
}

/// Where a source's sign-in details live in the secret store.
///
/// Derived from the source id, not stored beside it, so the two cannot drift. The connect route
/// and the sync sweep must both call this; a second copy of the format string strands a secret.
pub fn secret_key_for(source_id: &str) -> String {
    format!("caldav:{source_id}")
}

/// The parts a [`ContextSource`] is built from, on both the connect path and the
/// storage read path.
#[derive(Debug, Clone)]
pub struct SourceParts {
    pub id: String,
    pub kind: SourceKind,
    pub provider: String,
    pub profile_id: String,
    pub scopes: Vec<String>,
    pub cursor: Option<String>,
    pub last_sync: Option<DateTime<Utc>>,
    pub status: SourceStatus,
    pub secret_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ContextSource {
    /// The only constructor. Refuses a blank id, owner or provider.
    pub fn from_parts(parts: SourceParts) -> Result<Self, ContextError> {
        if parts.id.trim().is_empty() {
            return Err(ContextError::BlankId("source"));
        }
        if parts.profile_id.trim().is_empty() {
            return Err(ContextError::BlankOwner("source"));
        }
        if parts.provider.trim().is_empty() {
            return Err(ContextError::BlankProvider);
        }
        Ok(Self {
            id: parts.id,
            kind: parts.kind,
            provider: parts.provider,
            profile_id: parts.profile_id,
            scopes: parts.scopes,
            cursor: parts.cursor,
            last_sync: parts.last_sync,
            status: parts.status,
            secret_ref: parts.secret_ref,
            created_at: parts.created_at,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn kind(&self) -> SourceKind {
        self.kind
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }
    pub fn last_sync(&self) -> Option<DateTime<Utc>> {
        self.last_sync
    }
    pub fn status(&self) -> SourceStatus {
        self.status
    }
    pub fn secret_ref(&self) -> Option<&str> {
        self.secret_ref.as_deref()
    }
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Advance the incremental sync position. The only mutation a source has,
    /// and it deliberately cannot touch `kind` or `profile_id`: migration 0044
    /// refuses to change either, because a source whose owner moved would
    /// misattribute every item already stored under it.
    pub fn advance(&mut self, cursor: Option<String>, at: DateTime<Utc>, status: SourceStatus) {
        self.cursor = cursor;
        self.last_sync = Some(at);
        self.status = status;
    }
}

// ── ContextItem ─────────────────────────────────────────────────────────────

/// The raw parts of an item, before redaction.
///
/// `title`, `body` and `participants` all go through the redactor: a bridge puts whatever the
/// upstream account called the sender into `participants`, which is where credentials land too.
#[derive(Debug, Clone)]
pub struct ItemParts {
    pub id: String,
    pub source_id: String,
    pub external_id: String,
    pub profile_id: String,
    pub source_kind: SourceKind,
    pub kind: ItemKind,
    pub occurred_at: DateTime<Utc>,
    pub ingested_at: DateTime<Utc>,
    pub title: String,
    pub body: String,
    pub participants: Vec<String>,
    /// What the row already claimed, when this is a storage read. `None` on the
    /// ingest path. See [`ContextItem::from_parts`] for why it can only make the
    /// classification stricter.
    pub stored_sensitivity: Option<PrivacySensitivity>,
    pub embedding: Option<Vec<f32>>,
}

/// One thing a source produced, owned by one household member, redacted.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    id: String,
    source_id: String,
    external_id: String,
    profile_id: String,
    source_kind: SourceKind,
    kind: ItemKind,
    occurred_at: DateTime<Utc>,
    ingested_at: DateTime<Utc>,
    title: String,
    body: String,
    participants: Vec<String>,
    sensitivity: PrivacySensitivity,
    findings: Vec<RedactionKind>,
    embedding: Option<Vec<f32>>,
}

impl ContextItem {
    /// The only constructor, and it takes the redactor: every `ContextItem` has been through
    /// [`INGEST_REDACTION_LEVEL`]. Sensitivity is derived, never a parameter — the strictest of
    /// the source kind's floor, `Sensitive` if the redactor found anything, and any
    /// `stored_sensitivity`. `Secret` is unreachable: this level replaces credentials.
    pub fn from_parts(
        redactor: &dyn Redactor,
        parts: ItemParts,
    ) -> Result<ContextItem, ContextError> {
        if parts.id.trim().is_empty() {
            return Err(ContextError::BlankId("item"));
        }
        if parts.source_id.trim().is_empty() {
            return Err(ContextError::BlankSourceId);
        }
        if parts.external_id.trim().is_empty() {
            return Err(ContextError::BlankExternalId);
        }
        if parts.profile_id.trim().is_empty() {
            return Err(ContextError::BlankOwner("item"));
        }
        if parts.title.trim().is_empty() && parts.body.trim().is_empty() {
            return Err(ContextError::Empty);
        }

        let mut findings: Vec<RedactionKind> = Vec::new();
        let mut clean = |raw: &str| -> String {
            let result = redactor.redact(raw, INGEST_REDACTION_LEVEL);
            for kind in result.findings {
                if !findings.contains(&kind) {
                    findings.push(kind);
                }
            }
            result.text
        };

        let title = clean(&parts.title);
        let body = clean(&parts.body);
        let participants: Vec<String> = parts.participants.iter().map(|p| clean(p)).collect();

        let mut sensitivity = parts.source_kind.min_sensitivity();
        if !findings.is_empty() {
            sensitivity = sensitivity.max(PrivacySensitivity::Sensitive);
        }
        if let Some(stored) = parts.stored_sensitivity {
            sensitivity = sensitivity.max(stored);
        }

        Ok(ContextItem {
            id: parts.id,
            source_id: parts.source_id,
            external_id: parts.external_id,
            profile_id: parts.profile_id,
            source_kind: parts.source_kind,
            kind: parts.kind,
            occurred_at: parts.occurred_at,
            ingested_at: parts.ingested_at,
            title,
            body,
            participants,
            sensitivity,
            findings,
            embedding: parts.embedding,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn source_id(&self) -> &str {
        &self.source_id
    }
    pub fn external_id(&self) -> &str {
        &self.external_id
    }
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
    pub fn source_kind(&self) -> SourceKind {
        self.source_kind
    }
    pub fn kind(&self) -> ItemKind {
        self.kind
    }
    pub fn occurred_at(&self) -> DateTime<Utc> {
        self.occurred_at
    }
    pub fn ingested_at(&self) -> DateTime<Utc> {
        self.ingested_at
    }
    /// The redacted title. There is no accessor for the raw one because the raw
    /// one was never stored.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// The redacted body.
    pub fn body(&self) -> &str {
        &self.body
    }
    pub fn participants(&self) -> &[String] {
        &self.participants
    }
    pub fn sensitivity(&self) -> PrivacySensitivity {
        self.sensitivity
    }
    /// What the redactor found, whether or not it replaced it. For logging and
    /// for the audit trail; never the matched text.
    pub fn findings(&self) -> &[RedactionKind] {
        &self.findings
    }
    pub fn embedding(&self) -> Option<&[f32]> {
        self.embedding.as_deref()
    }

    /// The text an embedding is computed over.
    ///
    /// Built from the REDACTED fields: a vector computed over a secret is a durable derivative of
    /// it, and this type never holds the raw text (compare PAI-2 P3, which had to drop vectors).
    pub fn embedding_text(&self) -> String {
        if self.title.trim().is_empty() {
            self.body.clone()
        } else if self.body.trim().is_empty() {
            self.title.clone()
        } else {
            format!("{}\n{}", self.title, self.body)
        }
    }

    /// Attach a vector computed over [`embedding_text`](Self::embedding_text).
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::mocks::mock_redactor::MockRedactor;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    fn redactor() -> MockRedactor {
        MockRedactor::replacing(KEY, RedactionKind::ApiKey)
    }

    fn parts(title: &str, body: &str) -> ItemParts {
        ItemParts {
            id: "i1".into(),
            source_id: "src-1".into(),
            external_id: "ext-1".into(),
            profile_id: "jerry".into(),
            source_kind: SourceKind::Voice,
            kind: ItemKind::Message,
            occurred_at: Utc::now(),
            ingested_at: Utc::now(),
            title: title.into(),
            body: body.into(),
            participants: vec![],
            stored_sensitivity: None,
            embedding: None,
        }
    }

    #[test]
    fn a_credential_is_gone_before_the_value_exists() {
        let item = ContextItem::from_parts(&redactor(), parts("subject", &format!("key {KEY} x")))
            .expect("valid item");
        assert!(
            !item.body().contains(KEY),
            "the constructor built an item whose body still holds the credential, so a \
             `ContextItem` in hand is no longer evidence that redaction ran: {}",
            item.body()
        );
        assert!(item.body().contains("[redacted:api-key]"));
        assert!(item.body().contains(" x"), "prose was mangled");
        assert!(
            !item.embedding_text().contains(KEY),
            "the embedding would be computed over the credential, making the vector a durable \
             derivative of it"
        );
    }

    #[test]
    fn a_participant_is_redacted_too() {
        let mut p = parts("subject", "body");
        p.participants = vec![format!("bot {KEY}")];
        let item = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert!(
            !item.participants()[0].contains(KEY),
            "a participant reached storage unredacted: {}",
            item.participants()[0]
        );
        assert!(item.findings().contains(&RedactionKind::ApiKey));
    }

    /// The level is pinned here, not passed in: a caller that could choose
    /// `Detect` would get an unredacted item out of the same constructor.
    #[test]
    fn the_level_is_secrets_and_the_caller_cannot_choose_it() {
        let r = redactor();
        let _ = ContextItem::from_parts(&r, parts("t", "b")).expect("valid item");
        let calls = r.calls();
        assert!(!calls.is_empty(), "the redactor was never consulted");
        for (_, level) in calls {
            assert_eq!(
                level,
                RedactionLevel::Secrets,
                "ingest redacts credentials, not contact details -- see INGEST_REDACTION_LEVEL"
            );
        }
    }

    #[test]
    fn every_text_field_goes_through_the_redactor() {
        let r = redactor();
        let mut p = parts("title text", "body text");
        p.participants = vec!["someone".into()];
        let _ = ContextItem::from_parts(&r, p).expect("valid item");
        let texts: Vec<String> = r.calls().into_iter().map(|(t, _)| t).collect();
        for expected in ["title text", "body text", "someone"] {
            assert!(
                texts.iter().any(|t| t == expected),
                "{expected:?} never reached the redactor; seen: {texts:?}"
            );
        }
    }

    #[test]
    fn a_blank_owner_is_refused_and_so_is_a_blank_external_id() {
        let mut p = parts("t", "b");
        p.profile_id = "   ".into();
        assert_eq!(
            ContextItem::from_parts(&redactor(), p).unwrap_err(),
            ContextError::BlankOwner("item")
        );

        let mut p = parts("t", "b");
        p.external_id = String::new();
        assert_eq!(
            ContextItem::from_parts(&redactor(), p).unwrap_err(),
            ContextError::BlankExternalId
        );
    }

    #[test]
    fn an_item_with_no_content_at_all_is_refused() {
        assert_eq!(
            ContextItem::from_parts(&redactor(), parts("  ", "")).unwrap_err(),
            ContextError::Empty
        );
        // One of the two is enough.
        assert!(ContextItem::from_parts(&redactor(), parts("subject only", "")).is_ok());
    }

    #[test]
    fn sensitivity_is_derived_and_a_finding_raises_it() {
        let mut p = parts("t", "b");
        p.source_kind = SourceKind::Sensor;
        let plain = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(plain.sensitivity(), PrivacySensitivity::Internal);

        let mut p = parts("t", &format!("{KEY} was in here"));
        p.source_kind = SourceKind::Sensor;
        let found = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(
            found.sensitivity(),
            PrivacySensitivity::Sensitive,
            "an item the redactor had to clean is not Internal"
        );
        assert_ne!(
            found.sensitivity(),
            PrivacySensitivity::Secret,
            "Secret means credentials, and the credential is what was removed"
        );
    }

    /// A row whose `sensitivity` column was lowered out of band must not come
    /// back at the lowered level. Sensitivity is a restriction; the stricter of
    /// the two wins, in both directions.
    #[test]
    fn a_stored_classification_can_only_make_it_stricter() {
        let mut p = parts("t", "b");
        p.source_kind = SourceKind::Sensor;
        p.stored_sensitivity = Some(PrivacySensitivity::Public);
        let lowered = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(
            lowered.sensitivity(),
            PrivacySensitivity::Internal,
            "a Public in the row must not beat the source kind's floor"
        );

        let mut p = parts("t", "b");
        p.source_kind = SourceKind::Sensor;
        p.stored_sensitivity = Some(PrivacySensitivity::Secret);
        let raised = ContextItem::from_parts(&redactor(), p).expect("valid item");
        assert_eq!(raised.sensitivity(), PrivacySensitivity::Secret);
    }

    /// Reading a stored row re-runs redaction. `Redactor::redact` is
    /// contractually idempotent, so this costs nothing for a row written
    /// properly and repairs one that arrived some other way.
    #[test]
    fn the_read_path_repairs_a_row_written_out_of_band() {
        let smuggled = ItemParts {
            stored_sensitivity: Some(PrivacySensitivity::Internal),
            ..parts("t", &format!("someone pasted {KEY} straight into sqlite"))
        };
        let item = ContextItem::from_parts(&redactor(), smuggled).expect("valid item");
        assert!(!item.body().contains(KEY), "{}", item.body());
    }

    #[test]
    fn every_kind_maps_to_a_stable_name_and_back() {
        for kind in SourceKind::ALL {
            assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
        }
        for kind in ItemKind::ALL {
            assert_eq!(ItemKind::parse(kind.as_str()), Some(kind));
        }
        for status in SourceStatus::ALL {
            assert_eq!(SourceStatus::parse(status.as_str()), status);
        }
        assert_eq!(SourceKind::parse("gmail"), None);
        assert_eq!(ItemKind::parse("email"), None);
    }

    /// An unreadable status must not read as "carry on syncing".
    #[test]
    fn an_unknown_status_is_an_error_not_connected() {
        assert_eq!(SourceStatus::parse("whatever"), SourceStatus::Error);
        assert_ne!(SourceStatus::parse("whatever"), SourceStatus::Connected);
    }

    /// Exactly the three on-pond kinds are landed. The count is pinned because
    /// the failure direction is silent: a kind quietly promoted to `Landed`
    /// would let the pipeline accept a connector's data before the gate that is
    /// supposed to govern it exists.
    #[test]
    fn only_kinds_with_a_working_path_are_landed() {
        let landed: Vec<&str> = SourceKind::ALL
            .into_iter()
            .filter(|k| k.availability() == SourceAvailability::Landed)
            .map(|k| k.as_str())
            .collect();
        // A kind may only be `Landed` when something can actually produce items for it. Adding a
        // name here before the adapter, the credential path and the sync all exist mints sources
        // that look connected and stay empty.
        assert_eq!(
            landed,
            vec!["sensor", "camera", "voice", "mail", "calendar"],
            "a kind is Landed only once an adapter, a credential path and a sync exist for it"
        );
        for kind in SourceKind::ALL {
            if kind.availability() != SourceAvailability::Landed {
                assert!(
                    !kind.availability().refusal().is_empty(),
                    "{} is refused with no reason given",
                    kind.as_str()
                );
            }
        }
    }

    /// Nothing personal is classified `Public`, and `Public` really is a value
    /// the ordering admits — so this is a claim about the mapping, not about the
    /// enum having only one option.
    #[test]
    fn no_source_kind_floors_at_public() {
        assert!(PrivacySensitivity::Public < PrivacySensitivity::Internal);
        for kind in SourceKind::ALL {
            assert!(
                kind.min_sensitivity() >= PrivacySensitivity::Internal,
                "{} floors at {:?}",
                kind.as_str(),
                kind.min_sensitivity()
            );
        }
    }

    #[test]
    fn a_source_refuses_a_blank_owner_and_keeps_its_kind_across_an_advance() {
        let base = SourceParts {
            id: "src-1".into(),
            kind: SourceKind::Voice,
            provider: "pond".into(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: Utc::now(),
        };

        let blank = SourceParts {
            profile_id: " ".into(),
            ..base.clone()
        };
        assert_eq!(
            ContextSource::from_parts(blank).unwrap_err(),
            ContextError::BlankOwner("source")
        );

        let mut source = ContextSource::from_parts(base).expect("valid source");
        source.advance(Some("cursor-2".into()), Utc::now(), SourceStatus::Paused);
        assert_eq!(source.cursor(), Some("cursor-2"));
        assert_eq!(source.status(), SourceStatus::Paused);
        assert_eq!(source.kind(), SourceKind::Voice);
        assert_eq!(source.profile_id(), "jerry");
    }
}
