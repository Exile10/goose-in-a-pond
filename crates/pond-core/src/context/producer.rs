//! The on-pond producer (PAI-8 P3s) — the bridge from the event bus to the
//! ingest pipeline.
//!
//! P1 built a pipeline that refuses everything it is not given and P2 built the
//! retrieval side, and between them there was nothing that PRODUCES a
//! [`RawItem`]. This module is that thing, and it is deliberately the smallest
//! producer that can exist: a pure function from one [`BusEvent`] and one
//! [`ContextSource`] to either an item or a named refusal.
//!
//! # The design, and why it is this one
//!
//! A household member creates a [`ContextSource`] whose [`kind`] is
//! [`Sensor`](SourceKind::Sensor) or [`Camera`](SourceKind::Camera) and whose
//! [`provider`] names the device it follows. When a matching
//! [`BusEvent::Sensor`] or [`BusEvent::Camera`] arrives from that device it
//! becomes an item ingested under that source, and therefore under that source's
//! OWNER.
//!
//! [`ContextSource::profile_id`] is not an `Option`, so a source belongs to one
//! member. That is the whole reason the producer keys on a source the member
//! created rather than on the device registry: "the front-door camera is my
//! context" is a thing a person can say, and it is the only honest answer to
//! "whose data is this" for a device a household shares. A device registry entry
//! belongs to the house; a context source belongs to a person.
//!
//! **Voice transcripts were considered and rejected.** [`SourceKind::Voice`] is
//! `Landed`, so nothing in the type system stops it, and this module refuses it
//! anyway: pouring every conversation turn into the corpus is section 3.2's own
//! mistake in mirror image. `ChatService::persist_assistant_turn_with_extraction`
//! already curates facts out of those turns into the memory corpus, and a second,
//! uncurated copy of the same material would cost the retrieval budget twice and
//! recall worse.
//!
//! # What this refuses, and the shape of the refusals
//!
//! Every refusal is a named variant of [`NotIngested`] rather than a `None`,
//! because two of them have to carry a sentence: a source kind that has not
//! landed is refused with [`SourceAvailability::refusal`] verbatim, which is the
//! one place in the tree that says what has to land first. A `None` cannot carry
//! that, which is why the return type is a `Result` and not the `Option` the
//! brief sketched.
//!
//! The match on [`BusEvent`] has **no wildcard arm**. A seventh variant added to
//! the bus does not compile until somebody decides what it means here, which is
//! the only mechanism that survives the person who wrote this leaving.
//!
//! [`kind`]: ContextSource::kind
//! [`provider`]: ContextSource::provider
//! [`ContextSource::profile_id`]: ContextSource::profile_id

use chrono::{DateTime, SecondsFormat, Utc};
use thiserror::Error;

use crate::context::domain::{
    ContextSource, ItemKind, SourceAvailability, SourceKind, SourceStatus,
};
use crate::context::ingest::RawItem;
use crate::shared::ports::event_bus::BusEvent;
use crate::user_data::domain::sensor::{CameraEvent, SensorReading};
use crate::user_data::domain::settings::Settings;

// ── The worth-keeping policy ────────────────────────────────────────────────

/// Sensor signals whose readings are TRANSITIONS rather than samples.
///
/// This is the judgement in this module, and the number that forces it is
/// blunt: a temperature sensor reporting every 30 seconds produces 2 880 rows a
/// day per device, and this runs on a Jetson Orin Nano with an 8 GB budget where
/// the corpus is also read back into a prompt. Ten such sensors would put a
/// million rows a year into a store whose whole purpose is to answer "what
/// happened", and the honest answer for any one of those rows is "nothing".
///
/// So the rule is: **keep transitions, drop samples.** A context item is a
/// durable statement about the household. `occupancy = 1` is a statement.
/// `temperature = 21.4` is one point on a series the pond ALREADY stores, in
/// `sensor_readings` under `retention_sensor_days`, queryable by the
/// `giap-sensor` tools with min/max/avg aggregation. Copying that series into
/// the context corpus buys nothing and costs the budget twice.
///
/// **The rule is about the SIGNAL and never about the VALUE, and that is not
/// laziness.** The obvious refinement — "keep the active edge, drop the resting
/// state" — is wrong on this pond's own hardware. `pond-adapters-matter`'s
/// `protocol.rs` maps Matter's `BooleanState` to `sensor_type = "contact"` with
/// `true = closed`, per the Matter spec. A producer that kept non-zero readings
/// would therefore file "the door is shut" as news and drop "the door opened".
/// Polarity is device-specific, this function cannot know it, and guessing gets
/// it exactly backwards on the one binary sensor family the tree actually
/// implements.
///
/// An unrecognised signal is DROPPED. That is the narrowing direction: a new
/// sensor type produces nothing until somebody adds it here, whereas the
/// opposite default (a deny-list of known-continuous signals) answers an unknown
/// signal with 2 880 rows a day.
///
/// What this rule does NOT bound: a discrete signal from a bridge that POLLS
/// rather than reporting on change. Matter attribute subscriptions are
/// edge-reported, so the three signals grounded in this tree today are safe, but
/// a future adapter that polls a contact sensor every 30 seconds would produce
/// the same 2 880 rows. Retention bounds it; this function does not. Said here
/// rather than implied, because the alternative — quantising the timestamp in
/// the idempotency key — is rejected below for a reason.
pub const DISCRETE_SENSOR_TYPES: &[&str] = &[
    // The three that exist in this tree today: `motion` (the sensor route and
    // the bus tests), `occupancy` and `contact` (the Matter bridge's
    // `OCCUPANCY` and `BOOLEAN_STATE` clusters).
    "motion",
    "occupancy",
    "contact",
    // The same shape, for bridges that name them separately. Every one of these
    // is binary by construction; none of them is a measurement.
    "door",
    "window",
    "smoke",
    "leak",
    "button",
    // The Matter `SmokeCoAlarm` cluster's own name for the same thing (#195).
    // Its state is Normal/Warning/Critical rather than a boolean, but it is an
    // ALARM: it changes rarely, every change is an event a household needs, and
    // "the smoke alarm went to Critical at 03:12" is exactly the sentence this
    // corpus exists to be able to say. Grouped with `smoke` above rather than
    // replacing it, because the two names come from different bridges.
    "smoke_alarm",
];

/// Camera event types that mean "the pixels changed" rather than naming a thing.
///
/// `pond-adapters-vision`'s pipeline emits `"motion"` in three places — no
/// classifier configured, the classifier erroring, and the classifier finding
/// nothing above its own floor — so on a build without `vision-onnx` it is the
/// only label a camera ever produces. Its `confidence` in that case is the
/// fraction of the frame that changed, not a probability that anything is there.
///
/// A camera source on such a pond therefore ingests **nothing**, and that is the
/// correct answer rather than a defect: 8 640 rows a day (the pipeline's
/// 10-second `min_event_interval`) saying "something moved" is not context, it
/// is a smoke detector for a corpus. When the classifier is present the label
/// names a thing — `person`, `package`, `pet` — and that is a household fact
/// worth a durable row.
///
/// A deny-list rather than an allow-list, which is the opposite choice from
/// [`DISCRETE_SENSOR_TYPES`], and the asymmetry is deliberate: the value being
/// excluded here is a KNOWN sentinel this repo emits, while everything else is
/// by construction a positive classification out of a detector with 80 labels.
/// An allow-list would silently drop 75 of them.
/// `crates/pond-core/tests/context_producer_tracks_the_vision_pipeline.rs` ties
/// this constant to the literal `pipeline.rs` actually emits, because the silent
/// failure here is that the fallback label is renamed and this rule quietly
/// starts keeping every frame.
pub const UNCLASSIFIED_CAMERA_EVENT_TYPES: &[&str] = &["motion"];

/// A classified camera event below this confidence is dropped.
///
/// The same 0.5 `pond-adapters-vision`'s `MIN_CLASSIFIER_CONFIDENCE` uses, so
/// the vision path never trips it — this gate is live for
/// `POST /api/v1/camera/events`, where the confidence is whatever a paired
/// client sent. A second floor that agrees with the first is not redundant when
/// one of the two producers is outside this repo.
///
/// A camera event carrying NO confidence is kept: the manual route may legitimately
/// omit it, and the event-type rule above has already done the narrowing.
pub const MIN_CAMERA_CONFIDENCE: f64 = 0.5;

// ── Refusals ────────────────────────────────────────────────────────────────

/// Why the worth-keeping rule dropped an event that was otherwise addressed to
/// this source.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DropReason {
    #[error(
        "`{signal}` is a measurement, not a transition: the reading series already lives in \
         sensor_readings under retention_sensor_days, and copying it into the context corpus \
         costs 2880 rows a day per device and answers no question"
    )]
    ContinuousSample { signal: String },
    #[error(
        "a camera event typed `{event_type}` says the pixels changed and names nothing; without \
         a classifier label there is no household fact to record"
    )]
    NothingNamedIt { event_type: String },
    #[error(
        "a camera event at confidence {confidence} is below the {MIN_CAMERA_CONFIDENCE} floor \
         the on-device classifier itself applies"
    )]
    LowConfidence { confidence: f64 },
}

/// Why a bus event did not become a context item.
///
/// Every variant is a decision somebody made, and the ones that name a missing
/// prerequisite quote [`SourceAvailability::refusal`] rather than restating it —
/// there is exactly one sentence in this tree saying what a connector is waiting
/// for and a second one would go stale.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum NotIngested {
    #[error(
        "the on-pond context producer is off; set `context_ingest_enabled` to turn it on, which \
         nobody has done by upgrading"
    )]
    Disabled,

    #[error("cannot ingest from a {kind} source: {reason}")]
    SourceUnavailable {
        kind: &'static str,
        reason: &'static str,
    },

    #[error(
        "a voice source is not fed from the bus: memory extraction already curates facts out of \
         conversation turns, and a second uncurated copy of every turn would spend the retrieval \
         budget twice to recall worse"
    )]
    VoiceIsCuratedByMemoryExtraction,

    #[error("nothing on this pond's event bus produces data for a {kind} source")]
    NoBusEventFeedsThisKind { kind: &'static str },

    #[error(
        "this source is `{status}` rather than connected, and only a connected source ingests: \
         pausing a source is the one control a member has that stops the copying without \
         deleting what is already there, and a producer that ignored it would make that \
         control a no-op"
    )]
    SourceNotConnected { status: &'static str },

    #[error("a {event} event cannot feed a {source_kind} source")]
    WrongFamilyForSource {
        source_kind: &'static str,
        event: &'static str,
    },

    #[error("this source follows `{follows}`; the event came from `{came_from}`")]
    DifferentDevice { follows: String, came_from: String },

    #[error(
        "a {event} event is the pond noticing itself rather than a household fact, and a corpus \
         of the pond's own heartbeat teaches a model that the passage of time is something to \
         have opinions about"
    )]
    PondNoticingItself { event: &'static str },

    #[error(
        "a presence event names its own household member, and this source has one owner: \
         ingesting it here would file one member's movements in another member's corpus, which \
         is the misattribution `profile_id` is non-optional to prevent"
    )]
    WouldMisattribute,

    #[error(
        "no landed source kind describes a device state change: the only SourceKind whose \
         retention category is `device` is `mobile`, which has not landed, so there is no \
         sensitivity floor or retention window chosen for this data"
    )]
    NoLandedSourceKindDescribesIt,

    #[error(transparent)]
    NotWorthKeeping(#[from] DropReason),
}

// ── The producer ────────────────────────────────────────────────────────────

/// Turns bus events into [`RawItem`]s for the sources that follow them.
///
/// Constructed only from [`Settings`], so there is no way to hold one without
/// having answered `context_ingest_enabled`. That is the same move PAI-6 P1 made
/// with `DelegationDepth`: a toggle a caller can forget to read is a toggle that
/// is on.
#[derive(Debug, Clone)]
pub struct BusProducer {
    enabled: bool,
}

impl BusProducer {
    /// The only constructor.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            enabled: settings.context_ingest_enabled,
        }
    }

    /// Whether anything will be produced at all.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// One event, one source: an item or a named refusal.
    ///
    /// Pure. It reads no clock, no store and no global, so the same event and
    /// the same source give the same answer forever — which is what makes the
    /// idempotency claim below testable rather than asserted.
    pub fn raw_item_for(
        &self,
        source: &ContextSource,
        event: &BusEvent,
    ) -> Result<RawItem, NotIngested> {
        if !self.enabled {
            return Err(NotIngested::Disabled);
        }

        // 1. Availability, quoting the table's own sentence. A connector source
        //    is refused here whatever the event is, so this cannot be reached by
        //    picking an event that happens to match.
        let availability = source.kind().availability();
        if availability != SourceAvailability::Landed {
            return Err(NotIngested::SourceUnavailable {
                kind: source.kind().as_str(),
                reason: availability.refusal(),
            });
        }

        // 2. Of the kinds that HAVE landed, which does the bus feed? Exhaustive
        //    and wildcard-free: a ninth SourceKind does not compile until it is
        //    answered. The connector arm is unreachable while step 1 stands, and
        //    is written anyway so that promoting a kind to `Landed` does not
        //    silently make it ingestable from the bus.
        match source.kind() {
            SourceKind::Sensor | SourceKind::Camera => {}
            SourceKind::Voice => return Err(NotIngested::VoiceIsCuratedByMemoryExtraction),
            SourceKind::Mobile
            | SourceKind::Mail
            | SourceKind::Calendar
            | SourceKind::Files
            | SourceKind::Chat => {
                return Err(NotIngested::NoBusEventFeedsThisKind {
                    kind: source.kind().as_str(),
                })
            }
        }

        // 3. This source's own state. A kind can be landed and the member can
        //    still have paused THIS source, and pausing is the only control they
        //    have that stops the copying without deleting what is already
        //    stored. Exhaustive and wildcard-free for the same reason as the
        //    match above: a fifth status must be dispositioned, not defaulted.
        //
        //    Everything that is not `Connected` refuses, which is the narrowing
        //    direction and matters more here than it looks: `SourceStatus::parse`
        //    already reads an unrecognised stored string as `Error`, so a corrupt
        //    row arrives here as `Error` and must not be the one that says
        //    "carry on copying".
        match source.status() {
            SourceStatus::Connected => {}
            status @ (SourceStatus::NeedsReauth | SourceStatus::Error | SourceStatus::Paused) => {
                return Err(NotIngested::SourceNotConnected {
                    status: status.as_str(),
                })
            }
        }

        // 4. The event. NO WILDCARD ARM.
        match event {
            BusEvent::Sensor(reading) => self.item_from_sensor(source, reading),
            BusEvent::Camera(camera) => self.item_from_camera(source, camera),
            // A device state change is mostly the pond's own actuation coming
            // back: the assistant turned the lamp on and the lamp reports
            // `power = true`. More decisively, there is no landed `SourceKind`
            // whose sensitivity floor and retention window were chosen for it —
            // `retention_category() == Device` holds only for `Mobile`, which is
            // `AwaitingIngestRoute`. Filing it under `Sensor` would classify a
            // record of what the household switched on and off as a measurement.
            BusEvent::Device(_) => Err(NotIngested::NoLandedSourceKindDescribesIt),
            // The pond noticing itself. `proactive_review::reviewable` refuses
            // exactly this pair for the same reason, and this is the second
            // consumer of the bus to reach it independently.
            BusEvent::Time(_) => Err(NotIngested::PondNoticingItself { event: "time" }),
            BusEvent::Session(_) => Err(NotIngested::PondNoticingItself { event: "session" }),
            // A presence event carries its own `profile_id`, and this source
            // carries a different one. There is no correct owner to store it
            // under.
            BusEvent::Presence(_) => Err(NotIngested::WouldMisattribute),
        }
    }

    /// Every (source, item) pair one event produces across a household's
    /// sources.
    ///
    /// Plural on purpose. Two members may each create a source following the
    /// same front-door camera, and each then gets the item in their own corpus
    /// under their own `profile_id`. The ids do not collide — the stored id is
    /// `{source_id}:{external_id}` — so this is two rows by design rather than a
    /// write race.
    ///
    /// Refusals are dropped here rather than returned: this is the call the
    /// wiring makes for every event on the bus, and the overwhelmingly common
    /// answer is "no source follows this device". Use
    /// [`raw_item_for`](Self::raw_item_for) when the reason matters.
    pub fn items_for<'a>(
        &self,
        sources: &'a [ContextSource],
        event: &BusEvent,
    ) -> Vec<(&'a ContextSource, RawItem)> {
        sources
            .iter()
            .filter_map(|source| {
                self.raw_item_for(source, event)
                    .ok()
                    .map(|item| (source, item))
            })
            .collect()
    }

    fn item_from_sensor(
        &self,
        source: &ContextSource,
        reading: &SensorReading,
    ) -> Result<RawItem, NotIngested> {
        if source.kind() != SourceKind::Sensor {
            return Err(NotIngested::WrongFamilyForSource {
                source_kind: source.kind().as_str(),
                event: "sensor",
            });
        }
        if reading.device_id != source.provider() {
            return Err(NotIngested::DifferentDevice {
                follows: source.provider().to_string(),
                came_from: reading.device_id.clone(),
            });
        }
        sensor_is_worth_keeping(reading)?;

        let unit = reading.unit.trim();
        let body = if unit.is_empty() {
            format!(
                "{} reported {} = {}",
                reading.device_id, reading.sensor_type, reading.value
            )
        } else {
            format!(
                "{} reported {} = {} {}",
                reading.device_id, reading.sensor_type, reading.value, unit
            )
        };

        Ok(RawItem {
            external_id: sensor_external_id(reading),
            kind: ItemKind::Event,
            occurred_at: reading.recorded_at,
            title: format!("{} at {}", reading.sensor_type, reading.device_id),
            body,
            // Empty, and it must stay empty. A reading names no person, and
            // `participants` is the field `ContextItem::from_parts` singles out
            // as the one an upstream pastes arbitrary sender strings into.
            participants: Vec::new(),
        })
    }

    fn item_from_camera(
        &self,
        source: &ContextSource,
        camera: &CameraEvent,
    ) -> Result<RawItem, NotIngested> {
        if source.kind() != SourceKind::Camera {
            return Err(NotIngested::WrongFamilyForSource {
                source_kind: source.kind().as_str(),
                event: "camera",
            });
        }
        if camera.camera_id != source.provider() {
            return Err(NotIngested::DifferentDevice {
                follows: source.provider().to_string(),
                came_from: camera.camera_id.clone(),
            });
        }
        camera_is_worth_keeping(camera)?;

        let body = match camera.confidence {
            Some(c) => format!(
                "{} saw {} (confidence {c})",
                camera.camera_id, camera.event_type
            ),
            None => format!("{} saw {}", camera.camera_id, camera.event_type),
        };

        Ok(RawItem {
            external_id: camera_external_id(camera),
            kind: ItemKind::Event,
            occurred_at: camera.created_at,
            title: format!("{} at {}", camera.event_type, camera.camera_id),
            // `snapshot_path` and `metadata` are deliberately absent. The body
            // is text a model reads back into its window: a filesystem path is
            // not a household fact and is wrong the moment the file is pruned,
            // and `metadata` is adapter-controlled JSON of unbounded size, which
            // is token cost against PAI-3's asymmetry rule with no reading
            // benefit. Neither is lost — both stay on the camera_events row.
            body,
            participants: Vec::new(),
        })
    }
}

// ── Idempotency ─────────────────────────────────────────────────────────────

/// The external id of a sensor reading: `sensor:{device}:{signal}:{instant}`.
///
/// **What it is derived from, and why those three.** The triple identifies the
/// reading uniquely — one device cannot report two values for one signal at one
/// instant — and every part of it travels in the event, so the id is a function
/// of the event and of nothing else. No clock is read and no counter is kept.
///
/// **What re-ingesting the same event does.** `IngestPipeline` stores an item
/// under `{source_id}:{external_id}` and `ContextRepository::save_item` is
/// idempotent on `(source_id, external_id)`, so a second delivery of the same
/// reading UPDATEs the row the first one wrote. Nothing accumulates. That is not
/// a theoretical case: a cursor that slips backwards is normal, the bus is a
/// broadcast channel a restarted consumer re-reads from, and a store that
/// answered a re-delivery with a duplicate would fill the corpus with the same
/// three readings.
///
/// **The value is NOT in the key**, deliberately: two readings at the same
/// instant from the same device for the same signal are the same reading, and
/// putting a `f64` in a primary key means the row identity depends on float
/// formatting.
///
/// **Quantising the instant was considered and rejected.** Bucketing to the
/// minute would collapse a polling bridge's repeats into one row and would look
/// like a free rate limit. It is not free: two genuinely different readings in
/// the same minute would then share a row, so the second silently destroys the
/// first. That is a lossy policy wearing an idempotency key's clothes, and it
/// would make "re-ingesting the same event is idempotent" and "ingesting a
/// different event is not" stop being the same claim.
fn sensor_external_id(reading: &SensorReading) -> String {
    format!(
        "sensor:{}:{}:{}",
        reading.device_id,
        reading.sensor_type,
        instant(reading.recorded_at)
    )
}

/// The external id of a camera event: `camera:{camera}:{type}:{instant}`.
///
/// [`CameraEvent::id`] is the upstream's own primary key and is deliberately NOT
/// used. It is `None` until the row is persisted, and both publishers in this
/// tree (`pond-adapters-vision`'s pipeline and the `POST /camera/events` route)
/// persist first and stamp it before publishing — so it is populated today, and
/// keying on it would mean any publisher that ever published before persisting
/// mints a SECOND row for an event already stored. Deriving from the event's own
/// content cannot develop that failure mode.
fn camera_external_id(camera: &CameraEvent) -> String {
    format!(
        "camera:{}:{}:{}",
        camera.camera_id,
        camera.event_type,
        instant(camera.created_at)
    )
}

/// Milliseconds, UTC, `Z`-suffixed. A fixed rendering, so the key does not move
/// with the host's locale or offset.
fn instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

// ── Worth keeping ───────────────────────────────────────────────────────────

/// See [`DISCRETE_SENSOR_TYPES`].
fn sensor_is_worth_keeping(reading: &SensorReading) -> Result<(), DropReason> {
    let signal = reading.sensor_type.trim().to_ascii_lowercase();
    if DISCRETE_SENSOR_TYPES.contains(&signal.as_str()) {
        Ok(())
    } else {
        Err(DropReason::ContinuousSample {
            signal: reading.sensor_type.clone(),
        })
    }
}

/// See [`UNCLASSIFIED_CAMERA_EVENT_TYPES`] and [`MIN_CAMERA_CONFIDENCE`].
fn camera_is_worth_keeping(camera: &CameraEvent) -> Result<(), DropReason> {
    let label = camera.event_type.trim().to_ascii_lowercase();
    // A blank label names nothing just as surely as "motion" does, and it is the
    // shape an adapter that forgot to set the field produces.
    if label.is_empty() || UNCLASSIFIED_CAMERA_EVENT_TYPES.contains(&label.as_str()) {
        return Err(DropReason::NothingNamedIt {
            event_type: camera.event_type.clone(),
        });
    }
    // Written as "at or above the floor is kept" rather than "below the floor is
    // dropped". Those are the same statement for every real number and NOT the
    // same for `NaN`: `NaN < 0.5` is false, so the negative form keeps a
    // confidence that is not a number. That value arrives from outside this
    // repo — `POST /api/v1/camera/events` hands over whatever a paired client
    // sent — and an unreadable confidence must narrow to a refusal rather than
    // widen to an accepted row.
    match camera.confidence {
        None => Ok(()),
        Some(c) if c >= MIN_CAMERA_CONFIDENCE => Ok(()),
        Some(c) => Err(DropReason::LowConfidence { confidence: c }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::domain::{SourceParts, SourceStatus};
    use crate::security::domain::event::EventCategory;
    use crate::shared::domain::session_activity::{
        PresenceTransition, ProfilePresence, SessionLifecycle, SessionPhase,
    };
    use crate::shared::domain::time_tick::{TimeBoundary, TimeTick};
    use crate::user_data::domain::device::{DeviceStateChanged, DeviceStateValue};
    use crate::user_data::domain::session::IdentificationSource;

    const HALL_PIR: &str = "hall-pir";
    const DOOR_CAM: &str = "front-door-cam";

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("representable instant")
    }

    fn producer() -> BusProducer {
        BusProducer::from_settings(&Settings {
            context_ingest_enabled: true,
            ..Settings::default()
        })
    }

    fn source(kind: SourceKind, provider: &str) -> ContextSource {
        source_in(kind, provider, SourceStatus::Connected)
    }

    fn source_in(kind: SourceKind, provider: &str, status: SourceStatus) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: format!("src-{}-{provider}", kind.as_str()),
            kind,
            provider: provider.to_string(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status,
            secret_ref: None,
            created_at: at(0),
        })
        .expect("valid source")
    }

    fn reading(device: &str, signal: &str, value: f64, secs: i64) -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: device.into(),
            sensor_type: signal.into(),
            value,
            unit: "bool".into(),
            recorded_at: at(secs),
        })
    }

    fn camera(camera_id: &str, event_type: &str, confidence: Option<f64>, secs: i64) -> BusEvent {
        BusEvent::Camera(CameraEvent {
            id: Some(7),
            camera_id: camera_id.into(),
            event_type: event_type.into(),
            confidence,
            snapshot_path: Some("/var/pond/snap.jpg".into()),
            metadata: Some(r#"{"changed_fraction":0.31}"#.into()),
            acknowledged: false,
            created_at: at(secs),
        })
    }

    /// The name of a variant, by an exhaustive match with NO wildcard arm.
    ///
    /// This exists so the fixture below is quantified over the enum rather than
    /// over a list somebody kept up to date. A seventh `BusEvent` variant is a
    /// compile error HERE as well as in `raw_item_for`, and the two are
    /// different claims: the first says the producer decided, this says the
    /// sweep still covers what it decided about. A plain `assert_eq!(len, 6)`
    /// would have said neither — six entries still number six after a seventh
    /// variant is added, so a count alone is satisfied by a fixture that has
    /// gone stale, which is the failure it reads as guarding against.
    fn variant_name(event: &BusEvent) -> &'static str {
        match event {
            BusEvent::Sensor(_) => "sensor",
            BusEvent::Camera(_) => "camera",
            BusEvent::Device(_) => "device",
            BusEvent::Time(_) => "time",
            BusEvent::Presence(_) => "presence",
            BusEvent::Session(_) => "session",
        }
    }

    /// One of every [`BusEvent`] variant, each labelled by [`variant_name`] so
    /// the label cannot drift from the value it is attached to.
    fn one_of_each_variant() -> Vec<(&'static str, BusEvent)> {
        let every = vec![
            ("sensor", reading(HALL_PIR, "motion", 1.0, 10)),
            ("camera", camera(DOOR_CAM, "person", Some(0.91), 10)),
            (
                "device",
                BusEvent::Device(DeviceStateChanged {
                    device_id: "kitchen-lamp".into(),
                    key: "power".into(),
                    value: DeviceStateValue::Bool(true),
                    changed_at: at(10),
                }),
            ),
            (
                "time",
                BusEvent::Time(TimeTick {
                    boundary: TimeBoundary::Hour,
                    at: at(10),
                    local_hour: 12,
                }),
            ),
            (
                "presence",
                BusEvent::Presence(ProfilePresence {
                    profile_id: "liz".into(),
                    transition: PresenceTransition::Arrived,
                    source: IdentificationSource::Face,
                    confidence: Some(0.71),
                    session_id: "sess-1".into(),
                    at: at(10),
                }),
            ),
            (
                "session",
                BusEvent::Session(SessionLifecycle {
                    phase: SessionPhase::Idle,
                    session_id: None,
                    at: at(10),
                    idle_secs: 900,
                }),
            ),
        ];
        // The label on each entry is the one `variant_name` gives it, so a
        // fixture that says "camera" beside a session event cannot exist. Every
        // sweep below branches on that label.
        for (label, event) in &every {
            assert_eq!(
                *label,
                variant_name(event),
                "the fixture labels a {} event as `{label}`, and every sweep below branches on \
                 that label -- so the assertions would be made about the wrong variant",
                variant_name(event)
            );
        }
        // And every variant appears exactly once. `variant_name` is exhaustive
        // and wildcard-free, so a seventh variant fails to compile above; this
        // is the other half -- that a variant which compiles is also present.
        let mut labels: Vec<&str> = every.iter().map(|(l, _)| *l).collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            vec!["camera", "device", "presence", "sensor", "session", "time"],
            "BusEvent gained, lost or renamed a variant. Every consumer of this fixture asserts a \
             disposition per variant, so extend the fixture before deciding what the new one means"
        );
        every
    }

    // ── 1. Every variant is dispositioned ───────────────────────────────────

    /// The event match has no wildcard arm.
    ///
    /// The module doc claims a seventh `BusEvent` variant cannot silently join,
    /// and that claim rests entirely on rustc's exhaustiveness check — which a
    /// single `_ =>` added by a later refactor switches off, with no compile
    /// error, no failing test, and a new kind of household event answering
    /// whatever the wildcard says. That is precisely the silent-and-expensive
    /// shape this programme keeps rediscovering, so the absence is asserted
    /// rather than commented. `ingest.rs` guards `RawItem`'s fields the same
    /// way, and for the same reason: some properties are about the source text.
    #[test]
    fn the_event_match_has_no_wildcard_arm() {
        const SRC: &str = include_str!("producer.rs");
        let block = SRC
            .split("// 4. The event. NO WILDCARD ARM.")
            .nth(1)
            .and_then(|s| s.split("\n    }\n").next())
            .expect(
                "the marker comment above the event match is gone, so this guard reads nothing",
            );

        // Vacuity controls first: the extraction found the match and not the
        // whole file. Without them a renamed marker makes every assertion below
        // pass against an empty string.
        assert!(
            block.contains("match event {"),
            "the extracted fragment is not the event match: {block}"
        );
        assert!(
            block.len() < SRC.len() / 2,
            "the extraction swallowed most of the file, so `_ =>` anywhere else would fail this \
             test and a wildcard in the match would not be what it is reporting"
        );

        assert!(
            !block.contains("_ =>"),
            "the event match grew a wildcard arm. A seventh BusEvent variant now compiles \
             without anybody deciding what it means for the context corpus, which is the one \
             mechanism that survives whoever wrote this module leaving:\n{block}"
        );
        for variant in [
            "BusEvent::Sensor",
            "BusEvent::Camera",
            "BusEvent::Device",
            "BusEvent::Time",
            "BusEvent::Presence",
            "BusEvent::Session",
        ] {
            assert!(
                block.contains(variant),
                "`{variant}` has no arm of its own in the event match"
            );
        }
    }

    /// Every `BusEvent` variant has a stated answer for a source that follows
    /// the device it came from, and the four refusals name four DIFFERENT
    /// reasons rather than one shrug.
    #[test]
    fn every_bus_event_variant_is_dispositioned() {
        let p = producer();
        let sensor_src = source(SourceKind::Sensor, HALL_PIR);
        let camera_src = source(SourceKind::Camera, DOOR_CAM);

        let mut produced = 0usize;
        let mut refusals: Vec<NotIngested> = Vec::new();

        for (name, event) in one_of_each_variant() {
            // Offer each event to BOTH landed source kinds: a variant is only
            // "dispositioned" if neither of them ingests it by accident.
            let answers = [
                p.raw_item_for(&sensor_src, &event),
                p.raw_item_for(&camera_src, &event),
            ];
            let ok = answers.iter().filter(|a| a.is_ok()).count();
            match name {
                "sensor" | "camera" => {
                    assert_eq!(
                        ok, 1,
                        "a {name} event must be ingested by exactly its own source kind, got {ok} \
                         acceptances: {answers:?}"
                    );
                    produced += 1;
                }
                _ => {
                    assert_eq!(ok, 0, "a {name} event became a context item: {answers:?}");
                    for answer in answers {
                        refusals.push(answer.expect_err("just asserted no acceptance"));
                    }
                }
            }
        }

        assert_eq!(produced, 2, "neither device-shaped event produced an item");

        // The four refused families each name their own reason. Asserting the
        // VARIANT, not merely that something was refused: a producer that
        // answered every non-device event with `Disabled` would pass a
        // count-only check while saying nothing true.
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::NoLandedSourceKindDescribesIt)),
            "a device state change was refused without saying no source kind describes it: \
             {refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::PondNoticingItself { event: "time" })),
            "a time tick was not refused as the pond noticing itself: {refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::PondNoticingItself { event: "session" })),
            "a session transition was not refused as the pond noticing itself: {refusals:?}"
        );
        assert!(
            refusals
                .iter()
                .any(|r| matches!(r, NotIngested::WouldMisattribute)),
            "a presence event was refused without naming the misattribution: {refusals:?}"
        );
    }

    /// The premise of the `Device` refusal, asserted against the availability
    /// table rather than against my opinion. If a source kind whose retention
    /// category is `Device` ever lands, this fails and the arm above has to be
    /// revisited instead of quietly staying wrong.
    #[test]
    fn no_landed_source_kind_describes_a_device_state_change() {
        let landed_device_kinds: Vec<&str> = SourceKind::ALL
            .into_iter()
            .filter(|k| {
                k.retention_category() == EventCategory::Device
                    && k.availability() == SourceAvailability::Landed
            })
            .map(|k| k.as_str())
            .collect();
        assert!(
            landed_device_kinds.is_empty(),
            "{landed_device_kinds:?} now describes device state and HAS landed, so \
             `NoLandedSourceKindDescribesIt` is no longer true. Decide what a device state change \
             becomes before this ships."
        );
        // Vacuity control: the filter can find rows at all, so "empty" is a
        // fact about the availability of these kinds and not about the query.
        assert!(
            SourceKind::ALL
                .into_iter()
                .any(|k| k.retention_category() == EventCategory::Device),
            "no SourceKind maps to the Device retention category at all, so the assertion above \
             is vacuous"
        );
    }

    /// Voice is `Landed`, so nothing structural refuses it. This is the
    /// decision, and it is asserted rather than left to the doc comment.
    #[test]
    fn a_voice_source_is_refused_even_though_its_kind_has_landed() {
        assert_eq!(
            SourceKind::Voice.availability(),
            SourceAvailability::Landed,
            "if voice ever stops being Landed this test is asserting the wrong thing -- it exists \
             precisely because the availability table does NOT refuse voice"
        );
        let p = producer();
        for (_, event) in one_of_each_variant() {
            assert_eq!(
                p.raw_item_for(&source(SourceKind::Voice, HALL_PIR), &event),
                Err(NotIngested::VoiceIsCuratedByMemoryExtraction),
                "a voice source ingested {event:?}"
            );
        }
    }

    // ── 2. A kind that has not landed is refused, with the sentence ─────────

    /// Every source kind that is not `Landed` is refused, and the refusal
    /// carries `SourceAvailability::refusal()` VERBATIM rather than a second
    /// sentence that could drift from it.
    #[test]
    fn a_source_whose_kind_is_not_landed_is_refused_with_the_sentence_naming_what_is_missing() {
        let p = producer();
        let event = reading(HALL_PIR, "motion", 1.0, 10);

        let mut refused = 0usize;
        for kind in SourceKind::ALL {
            let availability = kind.availability();
            if availability == SourceAvailability::Landed {
                continue;
            }
            refused += 1;
            let err = p
                .raw_item_for(&source(kind, HALL_PIR), &event)
                .expect_err("an unlanded kind must be refused");
            assert_eq!(
                err,
                NotIngested::SourceUnavailable {
                    kind: kind.as_str(),
                    reason: availability.refusal(),
                },
                "{} was refused for the wrong reason",
                kind.as_str()
            );
            let rendered = err.to_string();
            assert!(
                rendered.contains(kind.as_str()),
                "the refusal does not say which kind: {rendered}"
            );
            assert!(
                !availability.refusal().is_empty() && rendered.contains(availability.refusal()),
                "the refusal does not carry the availability table's own sentence: {rendered}"
            );
            // Names the MISSING THING, not a phase number. The reason for the
            // connector kinds used to cite PAI-2 P6b and that citation expired:
            // chokepoint 3 is discharged and the draft gate only ever applied to
            // write-back, which v1 does not do. A guard that pins a phase id
            // outlives the phase; one that pins the mechanism does not.
            assert!(
                rendered.contains("ingest route") || rendered.contains("connector"),
                "the refusal does not name what has to land first: {rendered}"
            );
        }

        // Vacuity controls, both directions. Without the first, a table that
        // said `Landed` for everything would make the loop body unreachable and
        // this test pass having asserted nothing.
        assert_eq!(
            refused,
            SourceKind::ALL.len() - 3,
            "exactly the five connector kinds are unlanded; if that changed, this sweep is no \
             longer testing what it claims"
        );
        assert!(
            p.raw_item_for(&source(SourceKind::Sensor, HALL_PIR), &event)
                .is_ok(),
            "no source kind produces an item, so the refusals above are not a boundary"
        );
    }

    /// Pausing a source stops the copying. Both directions, because a gate that
    /// refused every status would pass the refusal half while making the whole
    /// producer inert, and a gate that refused none would make the one control a
    /// member has over an on-pond source a no-op.
    #[test]
    fn only_a_connected_source_ingests() {
        let p = producer();
        let event = reading(HALL_PIR, "motion", 1.0, 10);

        let mut refused = 0usize;
        for status in SourceStatus::ALL {
            let src = source_in(SourceKind::Sensor, HALL_PIR, status);
            let answer = p.raw_item_for(&src, &event);
            if status == SourceStatus::Connected {
                assert!(
                    answer.is_ok(),
                    "a connected source refused its own device's event: {answer:?}"
                );
                continue;
            }
            refused += 1;
            assert_eq!(
                answer,
                Err(NotIngested::SourceNotConnected {
                    status: status.as_str()
                }),
                "a {} source was not refused, or was refused for the wrong reason",
                status.as_str()
            );
        }
        assert_eq!(
            refused,
            SourceStatus::ALL.len() - 1,
            "exactly one status may ingest; if a second one is now acceptable this sweep is no \
             longer testing what it claims"
        );

        // The narrowing direction, stated as its own claim: an unrecognised
        // stored status parses as `Error`, so a corrupt row refuses rather than
        // carrying on copying.
        assert_eq!(SourceStatus::parse("who-knows"), SourceStatus::Error);
        assert_eq!(
            p.raw_item_for(
                &source_in(
                    SourceKind::Sensor,
                    HALL_PIR,
                    SourceStatus::parse("who-knows")
                ),
                &event
            ),
            Err(NotIngested::SourceNotConnected { status: "error" })
        );
    }

    // ── 3. Idempotency ──────────────────────────────────────────────────────

    /// The same event, delivered twice, addresses the same row. And a DIFFERENT
    /// event does not — without which "the id is stable" is also satisfied by
    /// returning a constant, which would collapse every reading a device ever
    /// made into one row.
    #[test]
    fn re_delivering_the_same_event_addresses_the_same_row() {
        let p = producer();
        let sensor_src = source(SourceKind::Sensor, HALL_PIR);
        let camera_src = source(SourceKind::Camera, DOOR_CAM);

        for (src, first, second) in [
            (
                &sensor_src,
                reading(HALL_PIR, "motion", 1.0, 10),
                reading(HALL_PIR, "motion", 1.0, 10),
            ),
            (
                &camera_src,
                camera(DOOR_CAM, "person", Some(0.91), 10),
                camera(DOOR_CAM, "person", Some(0.91), 10),
            ),
        ] {
            let a = p.raw_item_for(src, &first).expect("produced");
            let b = p.raw_item_for(src, &second).expect("produced");
            assert_eq!(
                a.external_id, b.external_id,
                "re-delivering the same event minted a second external id, so a bus replay would \
                 fill the corpus with duplicates of one reading"
            );
        }

        // Each of the three parts of the key changes the id on its own. A key
        // that ignored one of them would silently merge distinct events.
        let base = p
            .raw_item_for(&sensor_src, &reading(HALL_PIR, "motion", 1.0, 10))
            .expect("produced")
            .external_id;
        let later = p
            .raw_item_for(&sensor_src, &reading(HALL_PIR, "motion", 1.0, 11))
            .expect("produced")
            .external_id;
        assert_ne!(
            base, later,
            "two readings a second apart share a row, so the second destroys the first"
        );
        let other_signal = p
            .raw_item_for(&sensor_src, &reading(HALL_PIR, "contact", 1.0, 10))
            .expect("produced")
            .external_id;
        assert_ne!(
            base, other_signal,
            "two signals from one device share a row"
        );

        let other_device_src = source(SourceKind::Sensor, "porch-pir");
        let other_device = p
            .raw_item_for(&other_device_src, &reading("porch-pir", "motion", 1.0, 10))
            .expect("produced")
            .external_id;
        assert_ne!(base, other_device, "two devices share a row");
    }

    /// The producer reads no clock.
    ///
    /// **Repetition alone does not prove this, and finding that out is why the
    /// assertion below leads.** The obvious form — mint the id twenty-five times
    /// and demand one distinct answer — was the only form this test had, and a
    /// mutation replacing `reading.recorded_at` with `Utc::now()` PASSED it:
    /// [`instant`] renders milliseconds, and twenty-five calls to `format!` fall
    /// inside one of those. A guard whose subject is "does this read a clock"
    /// cannot itself be decided by how fast the clock ticks. So the claim is
    /// made positively instead — the key contains the EVENT's own instant — and
    /// the repetition is kept underneath it as the cheaper check on everything
    /// else in the item.
    #[test]
    fn the_external_id_is_a_function_of_the_event_and_nothing_else() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);
        let event = reading(HALL_PIR, "occupancy", 1.0, 42);

        let item = p.raw_item_for(&src, &event).expect("produced");
        assert!(
            item.external_id.contains(&instant(at(42))),
            "the key `{}` does not contain the reading's own instant `{}`, so it was derived from \
             something other than the event -- a clock, a counter or a random. A bus replay would \
             then mint a new row for a reading already stored",
            item.external_id,
            instant(at(42))
        );
        // Vacuity control: `instant` renders a DIFFERENT string for a different
        // moment, so "contains" above is a real constraint rather than a
        // substring that matches anything.
        assert_ne!(instant(at(42)), instant(at(43)));

        let ids: std::collections::BTreeSet<String> = (0..25)
            .map(|_| p.raw_item_for(&src, &event).expect("produced").external_id)
            .collect();
        assert_eq!(
            ids.len(),
            1,
            "the same event produced {} different external ids, so the key depends on something \
             outside the event: {ids:?}",
            ids.len()
        );
        // The whole item, not only the key: a body rebuilt from a clock would
        // rewrite the row's text on every replay.
        let items: std::collections::BTreeSet<String> = (0..5)
            .map(|_| {
                let item = p.raw_item_for(&src, &event).expect("produced");
                format!("{}|{}|{}", item.title, item.body, item.occurred_at)
            })
            .collect();
        assert_eq!(
            items.len(),
            1,
            "the item's content is not stable: {items:?}"
        );

        // The camera key is a second derivation and needs the same claim made
        // about it; asserting only the sensor one leaves half the producer able
        // to read a clock.
        let cam_src = source(SourceKind::Camera, DOOR_CAM);
        let cam = p
            .raw_item_for(&cam_src, &camera(DOOR_CAM, "person", Some(0.9), 42))
            .expect("produced");
        assert!(
            cam.external_id.contains(&instant(at(42))),
            "the camera key `{}` does not contain the event's own instant",
            cam.external_id
        );
    }

    // ── 4. Worth keeping, in both directions ────────────────────────────────

    /// The sensor rule keeps what it claims to keep AND drops what it claims to
    /// drop. Both halves, because a rule that drops everything passes a
    /// drop-only test and makes the feature inert, and a rule that keeps
    /// everything passes a keep-only test and puts 2 880 rows a day on a Jetson.
    #[test]
    fn the_sensor_rule_keeps_transitions_and_drops_samples() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);

        // Kept: discrete signals, at BOTH polarities. The value is deliberately
        // not consulted -- Matter's `contact` reports `true = closed`, so a
        // producer that kept only non-zero readings would file "the door is
        // shut" and drop "the door opened".
        for signal in ["motion", "occupancy", "contact", "door"] {
            for value in [0.0, 1.0] {
                assert!(
                    p.raw_item_for(&src, &reading(HALL_PIR, signal, value, 10))
                        .is_ok(),
                    "a {signal} reading of {value} was dropped; polarity is device-specific and \
                     this rule must not consult the value"
                );
            }
        }

        // Dropped: measurements. Each is a series `sensor_readings` already
        // holds under `retention_sensor_days`.
        for signal in ["temperature", "humidity", "co2", "pressure", "battery"] {
            let err = p
                .raw_item_for(&src, &reading(HALL_PIR, signal, 21.4, 10))
                .expect_err("a measurement must be dropped");
            assert_eq!(
                err,
                NotIngested::NotWorthKeeping(DropReason::ContinuousSample {
                    signal: signal.to_string()
                }),
                "a {signal} reading was dropped for the wrong reason"
            );
        }

        // An unrecognised signal drops. That is the narrowing direction: the
        // opposite default answers an unknown sensor with 2 880 rows a day.
        assert!(
            p.raw_item_for(&src, &reading(HALL_PIR, "flux-capacitance", 1.0, 10))
                .is_err(),
            "an unrecognised signal was kept, so a new sensor type defaults to being ingested"
        );

        // Case and padding must not decide it, or one adapter's "Motion" is a
        // measurement and another's "motion" is not.
        assert!(p
            .raw_item_for(&src, &reading(HALL_PIR, " Motion ", 1.0, 10))
            .is_ok());
    }

    /// The camera rule, both directions. The `motion` half is the load-bearing
    /// one: it is the label the vision pipeline emits when nothing classified
    /// the frame, and at a 10-second minimum interval that is 8 640 rows a day
    /// that say only "the pixels changed".
    #[test]
    fn the_camera_rule_keeps_classifications_and_drops_bare_motion() {
        let p = producer();
        let src = source(SourceKind::Camera, DOOR_CAM);

        for label in ["person", "vehicle", "package", "pet"] {
            assert!(
                p.raw_item_for(&src, &camera(DOOR_CAM, label, Some(0.91), 10))
                    .is_ok(),
                "a classified `{label}` event was dropped, so a pond with a classifier ingests \
                 nothing either"
            );
        }
        // No confidence at all is kept: the manual POST route may omit it, and
        // the label has already done the narrowing.
        assert!(p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", None, 10))
            .is_ok());

        for (label, confidence) in [("motion", Some(0.99)), ("MOTION", None), ("  ", None)] {
            let err = p
                .raw_item_for(&src, &camera(DOOR_CAM, label, confidence, 10))
                .expect_err("an unclassified camera event must be dropped");
            assert!(
                matches!(
                    err,
                    NotIngested::NotWorthKeeping(DropReason::NothingNamedIt { .. })
                ),
                "`{label}` was dropped for the wrong reason: {err}"
            );
        }

        // A classified event the producer was unsure of is dropped, and says so
        // -- a different refusal from "nothing named it".
        let err = p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", Some(0.2), 10))
            .expect_err("a low-confidence classification must be dropped");
        assert_eq!(
            err,
            NotIngested::NotWorthKeeping(DropReason::LowConfidence { confidence: 0.2 })
        );
        // The floor is a floor, not a ceiling: exactly at it, the event is kept.
        assert!(p
            .raw_item_for(
                &src,
                &camera(DOOR_CAM, "person", Some(MIN_CAMERA_CONFIDENCE), 10)
            )
            .is_ok());
    }

    /// A confidence that is not a number is refused.
    ///
    /// The obvious spelling of this rule — `Some(c) if c < FLOOR => drop` —
    /// KEEPS `NaN`, because every comparison with `NaN` is false. That is the
    /// widening direction on a value this repo does not produce: the vision
    /// pipeline's confidence is computed, but `POST /api/v1/camera/events`
    /// carries whatever a paired client sent. An unreadable confidence has to
    /// narrow to a refusal, which is invariant 2 applied to a float.
    #[test]
    fn a_confidence_that_is_not_a_number_is_refused_rather_than_kept() {
        let p = producer();
        let src = source(SourceKind::Camera, DOOR_CAM);
        let err = p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", Some(f64::NAN), 10))
            .expect_err("a NaN confidence must not be treated as clearing the floor");
        assert!(
            matches!(
                err,
                NotIngested::NotWorthKeeping(DropReason::LowConfidence { .. })
            ),
            "a NaN confidence was refused for the wrong reason: {err}"
        );

        // Infinities go the other way and must not be caught by the same net:
        // +inf clears any floor and is kept, -inf clears none and is dropped.
        // Without this the test above is also satisfied by a rule that refuses
        // every confidence it cannot compare, which would drop real events.
        assert!(p
            .raw_item_for(&src, &camera(DOOR_CAM, "person", Some(f64::INFINITY), 10))
            .is_ok());
        assert!(p
            .raw_item_for(
                &src,
                &camera(DOOR_CAM, "person", Some(f64::NEG_INFINITY), 10)
            )
            .is_err());
    }

    /// Vacuity control for both rules above: neither constant is empty and
    /// neither rule is a constant function. Without this, emptying
    /// `DISCRETE_SENSOR_TYPES` would make the drop half of the sensor test pass
    /// while the keep half is the only thing that fails, and the failure would
    /// read as a fixture problem.
    #[test]
    fn the_worth_keeping_rules_are_not_constant_functions() {
        assert!(!DISCRETE_SENSOR_TYPES.is_empty());
        assert!(!UNCLASSIFIED_CAMERA_EVENT_TYPES.is_empty());
        assert!(
            (0.0..=1.0).contains(&MIN_CAMERA_CONFIDENCE),
            "a confidence floor outside [0,1] either keeps everything or drops everything"
        );
        assert!(!DISCRETE_SENSOR_TYPES.contains(&"temperature"));
        assert!(DISCRETE_SENSOR_TYPES.contains(&"motion"));
    }

    // ── The toggle ──────────────────────────────────────────────────────────

    /// Off by default, and the same event that is refused when off is produced
    /// when on -- so this is a claim about the toggle rather than about the
    /// event being unproducible.
    #[test]
    fn the_producer_is_off_on_a_default_pond() {
        let event = reading(HALL_PIR, "motion", 1.0, 10);
        let src = source(SourceKind::Sensor, HALL_PIR);

        let off = BusProducer::from_settings(&Settings::default());
        assert!(!off.is_enabled());
        assert_eq!(off.raw_item_for(&src, &event), Err(NotIngested::Disabled));
        assert!(
            off.items_for(std::slice::from_ref(&src), &event).is_empty(),
            "the batch entry point ignored the toggle the single one honours"
        );

        assert!(producer().raw_item_for(&src, &event).is_ok());
        assert_eq!(
            producer()
                .items_for(std::slice::from_ref(&src), &event)
                .len(),
            1
        );
    }

    // ── Addressing ──────────────────────────────────────────────────────────

    #[test]
    fn an_event_from_another_device_belongs_to_no_source() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);
        assert_eq!(
            p.raw_item_for(&src, &reading("porch-pir", "motion", 1.0, 10)),
            Err(NotIngested::DifferentDevice {
                follows: HALL_PIR.to_string(),
                came_from: "porch-pir".to_string(),
            })
        );
    }

    #[test]
    fn a_camera_event_cannot_feed_a_sensor_source() {
        let p = producer();
        // Same provider string on purpose: only the FAMILY differs, so this
        // cannot pass by accident of the device check.
        let src = source(SourceKind::Sensor, DOOR_CAM);
        assert_eq!(
            p.raw_item_for(&src, &camera(DOOR_CAM, "person", Some(0.9), 10)),
            Err(NotIngested::WrongFamilyForSource {
                source_kind: "sensor",
                event: "camera",
            })
        );
        let src = source(SourceKind::Camera, HALL_PIR);
        assert_eq!(
            p.raw_item_for(&src, &reading(HALL_PIR, "motion", 1.0, 10)),
            Err(NotIngested::WrongFamilyForSource {
                source_kind: "camera",
                event: "sensor",
            })
        );
    }

    /// Two members each following the same camera each get the event, in their
    /// own corpus. Deliberate: a shared device can be more than one person's
    /// context, and `profile_id` is not an `Option`, so the only way to say that
    /// is two sources.
    #[test]
    fn two_members_following_one_camera_each_get_an_item() {
        let p = producer();
        let following = |id: &str, owner: &str| {
            ContextSource::from_parts(SourceParts {
                id: id.into(),
                kind: SourceKind::Camera,
                provider: DOOR_CAM.into(),
                profile_id: owner.into(),
                scopes: vec![],
                cursor: None,
                last_sync: None,
                status: SourceStatus::Connected,
                secret_ref: None,
                created_at: at(0),
            })
            .expect("valid source")
        };
        let jerry = following("src-jerry-cam", "jerry");
        let liz = following("src-liz-cam", "liz");

        let unrelated = source(SourceKind::Camera, "garage-cam");
        let sources = vec![jerry, liz, unrelated];
        let produced = p.items_for(&sources, &camera(DOOR_CAM, "person", Some(0.9), 10));

        assert_eq!(
            produced.len(),
            2,
            "one camera event reached {} of three sources; two follow it and one does not",
            produced.len()
        );
        let owners: Vec<&str> = produced.iter().map(|(s, _)| s.profile_id()).collect();
        assert_eq!(owners, vec!["jerry", "liz"]);
        assert_eq!(
            produced[0].1.external_id, produced[1].1.external_id,
            "the external id is the EVENT's identity; the owner is the source's, and the stored \
             id combines both"
        );
    }

    // ── The item itself ─────────────────────────────────────────────────────

    /// The body is prose about the reading, and the fields that are deliberately
    /// left out stay out. `snapshot_path` in particular is a filesystem path
    /// that is wrong the moment the file is pruned.
    #[test]
    fn a_camera_item_carries_the_event_and_not_the_adapters_payload() {
        let p = producer();
        let src = source(SourceKind::Camera, DOOR_CAM);
        let item = p
            .raw_item_for(&src, &camera(DOOR_CAM, "package", Some(0.83), 10))
            .expect("produced");

        assert_eq!(item.kind, ItemKind::Event);
        assert_eq!(item.occurred_at, at(10));
        assert!(item.title.contains("package") && item.title.contains(DOOR_CAM));
        assert!(item.body.contains("package") && item.body.contains("0.83"));
        assert!(
            !item.body.contains("snap.jpg"),
            "a filesystem path reached the corpus: {}",
            item.body
        );
        assert!(
            !item.body.contains("changed_fraction"),
            "adapter metadata reached the corpus: {}",
            item.body
        );
        assert!(
            item.participants.is_empty(),
            "a camera event named a participant; nothing here identifies a person"
        );
    }

    #[test]
    fn a_sensor_item_reads_as_a_sentence_and_keeps_its_unit() {
        let p = producer();
        let src = source(SourceKind::Sensor, HALL_PIR);
        let item = p
            .raw_item_for(&src, &reading(HALL_PIR, "occupancy", 1.0, 10))
            .expect("produced");
        assert_eq!(item.kind, ItemKind::Event);
        assert_eq!(item.occurred_at, at(10));
        assert_eq!(item.body, format!("{HALL_PIR} reported occupancy = 1 bool"));
        assert!(item.participants.is_empty());

        // A blank unit must not leave a trailing space in the prose.
        let blank_unit = BusEvent::Sensor(SensorReading {
            device_id: HALL_PIR.into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "  ".into(),
            recorded_at: at(10),
        });
        let item = p.raw_item_for(&src, &blank_unit).expect("produced");
        assert_eq!(item.body, format!("{HALL_PIR} reported motion = 1"));
    }
}
