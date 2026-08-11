//! The on-pond producer's one moving part (PAI-8 P3s).
//!
//! [`producer`](crate::context::producer) is a pure function and
//! [`IngestPipeline`] is a pure-ish sink; this joins them, and it exists so that
//! the wiring in `pond-server` is one call rather than four decisions.
//!
//! # Why this is in `pond-core` and not in `main.rs`
//!
//! Invariant 5: policy in `pond-core`, mechanism in adapters. Four things have
//! to be decided to turn a bus event into a stored row, and every one of them is
//! policy:
//!
//! 1. **Which scope enumerates the sources.** [`INGEST_SCOPE`] — and getting it
//!    wrong is not a compile error, it is a silently empty corpus
//!    ([`ProfileScope::Guest`]) or one member's device feeding another member's
//!    row (an `Owner` picked from the wrong place).
//! 2. **When to read the store at all.** A disabled producer must not cost a
//!    query per bus event on a Jetson, so the toggle is answered before the
//!    read, not after it.
//! 3. **What an unreadable store means.** It means refuse: see [`AbsorbError`].
//! 4. **What a refusal is worth saying.** A feature that is switched on and says
//!    nothing is indistinguishable from one that is broken — the lesson PAI-7 P4
//!    paid for when its reviewer ran silently on every pond — so the refusals
//!    are traced with their own sentences rather than dropped.
//!
//! A binary that had to make those four choices would be making them in the
//! crate this workstream's invariants cannot see.
//!
//! # What this is not
//!
//! It is not a subscriber. It holds no task, no channel and no clock: the caller
//! owns the bus subscription and passes each event in with the settings it read
//! for that tick. That is deliberate — a service that cached `Settings` at
//! construction would make `PUT /api/v1/settings` a no-op until restart, which
//! is exactly the shape of the `set_network_mode` defect PAI-2 P6a recorded.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::context::ingest::{IngestOutcome, IngestPipeline};
use crate::context::ports::ContextRepository;
use crate::context::producer::BusProducer;
use crate::shared::ports::event_bus::BusEvent;
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::settings::Settings;

/// The scope the pond enumerates context sources under when it is deciding
/// which of them an event belongs to.
///
/// [`ProfileScope::Household`] because this read has no asker. It is the pond
/// itself walking every source a member created, and each source then supplies
/// its OWN `profile_id` to the item written under it — so the breadth here never
/// becomes breadth in a stored row.
///
/// It is a constant rather than an argument on purpose. A caller that could pass
/// a scope would eventually pass a session's scope, and a session's scope is the
/// answer to "what may this speaker read", which is a different question with a
/// worse failure mode: a `Guest` turn arriving while a sensor fires would make
/// the pond stop recording, and an `Owner` turn would make one member's presence
/// at the keyboard decide whose devices get recorded.
pub const INGEST_SCOPE: ProfileScope = ProfileScope::Household;

/// What one bus event did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AbsorbReport {
    /// One per stored row. Plural because two members may follow one device.
    pub ingested: Vec<IngestOutcome>,
    /// Sources that were offered the event and said no. The overwhelmingly
    /// common answer, since most sources do not follow the device that fired.
    pub refused: usize,
    /// Sources that accepted the event and whose row could not be written.
    /// Counted rather than returned as an error: one failed write must not stop
    /// the other members' sources from getting theirs.
    pub storage_failures: usize,
}

impl AbsorbReport {
    pub fn stored(&self) -> usize {
        self.ingested.len()
    }
}

#[derive(Debug, Error)]
pub enum AbsorbError {
    /// The source list could not be read.
    ///
    /// This is an error and not an empty report, and the difference is the
    /// failure-direction rule. "No sources" and "I cannot tell you what the
    /// sources are" are different answers, and answering the second with the
    /// first would mean an unreadable store looks exactly like a household that
    /// has not configured anything — so a broken store would be reported by this
    /// feature as working correctly and doing nothing, forever.
    #[error(
        "could not read this household's context sources, so the event was offered to none of \
         them: {0}"
    )]
    SourcesUnreadable(#[source] anyhow::Error),
}

/// Turns bus events into stored context items.
pub struct BusIngest {
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
}

impl BusIngest {
    pub fn new(repo: Arc<dyn ContextRepository>, pipeline: Arc<IngestPipeline>) -> Self {
        Self { repo, pipeline }
    }

    /// Offer one bus event to every source that might follow it.
    ///
    /// `settings` is a parameter rather than a field so the toggle is answered
    /// from whatever the caller last read, and `now` is a parameter for the same
    /// reason [`IngestPipeline::ingest`] takes one: the ingest timestamp is the
    /// caller's clock, and a service that read its own could not be tested for
    /// the stability the idempotency claim rests on.
    pub async fn absorb(
        &self,
        settings: &Settings,
        event: &BusEvent,
        now: DateTime<Utc>,
    ) -> Result<AbsorbReport, AbsorbError> {
        let producer = BusProducer::from_settings(settings);
        // Before the read, not after it. On a default pond this is the whole
        // cost of the feature: a bool, once per bus event, and no query.
        if !producer.is_enabled() {
            return Ok(AbsorbReport::default());
        }

        let sources = self
            .repo
            .list_sources(&INGEST_SCOPE)
            .await
            .map_err(AbsorbError::SourcesUnreadable)?;

        let mut report = AbsorbReport::default();
        for source in &sources {
            let raw = match producer.raw_item_for(source, event) {
                Ok(raw) => raw,
                Err(reason) => {
                    report.refused += 1;
                    tracing::trace!(
                        target: "giap::trace",
                        source_id = %source.id(),
                        source_kind = %source.kind().as_str(),
                        reason = %reason,
                        "[context] a bus event was not ingested"
                    );
                    continue;
                }
            };
            match self.pipeline.ingest(source, raw, now).await {
                Ok(outcome) => report.ingested.push(outcome),
                Err(e) => {
                    report.storage_failures += 1;
                    tracing::warn!(
                        source_id = %source.id(),
                        error = %e,
                        "[context] a bus event matched a source and could not be stored"
                    );
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::{ContextSource, SourceKind, SourceParts, SourceStatus};
    use crate::context::mocks::mock_context_repository::MockContextRepository;
    use crate::security::domain::redaction::RedactionKind;
    use crate::security::mocks::mock_redactor::MockRedactor;
    use crate::user_data::domain::sensor::SensorReading;

    const HALL_PIR: &str = "hall-pir";
    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("representable instant")
    }

    fn on() -> Settings {
        Settings {
            context_ingest_enabled: true,
            ..Settings::default()
        }
    }

    fn source(id: &str, owner: &str, provider: &str) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: id.into(),
            kind: SourceKind::Sensor,
            provider: provider.into(),
            profile_id: owner.into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: at(0),
        })
        .expect("valid source")
    }

    fn motion(device: &str, secs: i64) -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: device.into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "bool".into(),
            recorded_at: at(secs),
        })
    }

    fn wire(repo: Arc<MockContextRepository>) -> BusIngest {
        let redactor = Arc::new(MockRedactor::replacing(KEY, RedactionKind::ApiKey));
        BusIngest::new(repo.clone(), Arc::new(IngestPipeline::new(repo, redactor)))
    }

    /// The happy path, and the two halves of it that matter: the row lands, and
    /// it lands under the SOURCE's owner rather than under anything the event
    /// said.
    #[tokio::test]
    async fn an_event_from_a_followed_device_is_stored_under_the_sources_owner() {
        let repo = Arc::new(MockContextRepository::new());
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        let report = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect("the source list is readable");
        assert_eq!(report.stored(), 1, "nothing was stored: {report:?}");
        assert_eq!(report.refused, 0);

        let stored = repo.all_items();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].profile_id(), "jerry");
        assert_eq!(stored[0].source_id(), "src-jerry");
    }

    /// The toggle, both directions, through THIS entry point — a producer that
    /// honoured it and a service that read the store anyway would still cost a
    /// query per event on every default pond.
    #[tokio::test]
    async fn a_default_pond_stores_nothing_and_does_not_even_read_the_store() {
        // The store is armed to fail on every read. With the toggle off that is
        // not an error, because the read never happens.
        let repo = Arc::new(
            MockContextRepository::new().with_unreadable_sources("the store must not be read"),
        );
        let ingest = wire(repo);
        let report = ingest
            .absorb(&Settings::default(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect("a disabled producer must not touch the store");
        assert_eq!(report, AbsorbReport::default());

        // Vacuity control: the same call with the toggle ON reaches the store,
        // so the line above is a fact about the toggle and not about the mock.
        let armed = Arc::new(
            MockContextRepository::new().with_unreadable_sources("the store must not be read"),
        );
        let ingest = wire(armed);
        assert!(ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .is_err());
    }

    /// An unreadable source list refuses. It does NOT report an empty pond,
    /// which is the permissive answer and the one that makes a broken store look
    /// like a working feature.
    #[tokio::test]
    async fn an_unreadable_source_list_refuses_rather_than_reporting_no_sources() {
        let repo = Arc::new(MockContextRepository::new().with_unreadable_sources("disk is gone"));
        let ingest = wire(repo);
        let err = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect_err("an unreadable store must not be reported as an empty one");
        assert!(
            matches!(err, AbsorbError::SourcesUnreadable(_)),
            "wrong refusal: {err}"
        );
        assert!(
            err.to_string().contains("disk is gone"),
            "the refusal drops the cause, so an operator cannot act on it: {err}"
        );
    }

    /// One event, three sources: two follow the device and each gets its own
    /// row under its own owner, and the third is refused. Both halves matter —
    /// a service that offered the event to only the first matching source would
    /// pass a one-follower test, and one that offered it to every source would
    /// pass a count-only test while filing the porch sensor's motion under the
    /// hall sensor's followers.
    #[tokio::test]
    async fn one_event_reaches_every_source_that_follows_it_and_no_others() {
        let repo = Arc::new(MockContextRepository::new());
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        repo.upsert_source(&source("src-liz", "liz", HALL_PIR))
            .await
            .unwrap();
        repo.upsert_source(&source("src-porch", "jerry", "porch-pir"))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        let report = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .unwrap();
        assert_eq!(report.stored(), 2, "both followers must get a row");
        assert_eq!(report.refused, 1, "the porch source must be refused");

        let owners: Vec<String> = repo
            .all_items()
            .iter()
            .map(|i| i.profile_id().to_string())
            .collect();
        assert_eq!(owners, vec!["jerry".to_string(), "liz".to_string()]);
    }

    /// A storage failure is counted and does not become an `Err`, because the
    /// caller is a loop over a broadcast channel: an event that returned `Err`
    /// for one source's write would report the whole tick as failed.
    #[tokio::test]
    async fn a_failed_write_is_counted_rather_than_abandoning_the_event() {
        let repo =
            Arc::new(MockContextRepository::new().with_unwritable_items("no space left on device"));
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        let report = ingest
            .absorb(&on(), &motion(HALL_PIR, 10), at(11))
            .await
            .expect("a write failure is not a read failure");
        assert_eq!(report.storage_failures, 1);
        assert_eq!(report.stored(), 0);
        assert!(repo.all_items().is_empty());
    }

    /// The same event twice writes one row. The producer's key makes this true
    /// and the store enforces it; this asserts the two agree end to end, which
    /// neither can do alone.
    #[tokio::test]
    async fn re_delivering_one_event_through_the_service_writes_one_row() {
        let repo = Arc::new(MockContextRepository::new());
        repo.upsert_source(&source("src-jerry", "jerry", HALL_PIR))
            .await
            .unwrap();
        let ingest = wire(repo.clone());

        for tick in 0..3 {
            ingest
                .absorb(&on(), &motion(HALL_PIR, 10), at(100 + tick))
                .await
                .unwrap();
        }
        assert_eq!(
            repo.all_items().len(),
            1,
            "three deliveries of one reading wrote {} rows",
            repo.all_items().len()
        );

        // Vacuity control: a DIFFERENT reading does write a second row, so the
        // line above is idempotency and not a store that drops writes.
        ingest
            .absorb(&on(), &motion(HALL_PIR, 20), at(200))
            .await
            .unwrap();
        assert_eq!(repo.all_items().len(), 2);
    }

    /// The scope this service enumerates under is the household's, and it is not
    /// reachable from a session. Asserted rather than commented, because a
    /// narrower value here is a silently empty corpus and a wrong `Owner` is a
    /// misattribution.
    #[test]
    fn the_enumeration_scope_is_the_households() {
        assert_eq!(INGEST_SCOPE, ProfileScope::Household);
        assert_ne!(INGEST_SCOPE, ProfileScope::Guest);
    }
}
