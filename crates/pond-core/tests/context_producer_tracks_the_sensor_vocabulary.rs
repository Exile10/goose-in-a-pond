//! The on-pond producer's sensor rule is an ALLOW-list, so a signal it has
//! never heard of produces nothing. This file ties that list to the only bridge
//! in the tree that mints signal names of its own.
//!
//! # Why an allow-list needs a tripwire and a deny-list does not
//!
//! `DISCRETE_SENSOR_TYPES` fails SAFE: an unrecognised signal is dropped, which
//! is the narrowing direction and deliberately so — the other default answers an
//! unknown sensor with 2 880 rows a day on a Jetson. But "fails safe" and "fails
//! visibly" are different properties, and this one fails silently: add a Matter
//! cluster mapping for a new binary sensor family and every one of those
//! readings is discarded forever, with no compile error, no failing test, and a
//! household wondering why their new door sensor is the one device the assistant
//! never mentions.
//!
//! That is the same shape as `context_producer_tracks_the_vision_pipeline.rs`
//! seen from the other side, and it is on this programme's own list of recorded
//! failure shapes: a feature that is switched on and says nothing is
//! indistinguishable from one that is broken.
//!
//! # What this does NOT claim
//!
//! `POST /api/v1/sensors/readings` publishes a `SensorReading` whose
//! `sensor_type` is whatever a paired client sent — an unbounded vocabulary this
//! file cannot enumerate and the producer must not accept wholesale. So this is
//! a **tripwire, not coverage**: it watches the one vocabulary the pond itself
//! generates, and `pond-core`'s unit tests carry the behavioural claims.

use std::path::{Path, PathBuf};

use pond_core::context::producer::DISCRETE_SENSOR_TYPES;

/// Signals the Matter bridge emits that are MEASUREMENTS, and must stay
/// dropped.
///
/// Named here rather than inferred, so that a signal moving between this list
/// and `DISCRETE_SENSOR_TYPES` is an edit somebody made on purpose. A new Matter
/// mapping belongs in exactly one of the two, and belonging to neither is what
/// fails below.
const KNOWN_CONTINUOUS: &[&str] = &[
    "temperature",
    "humidity",
    // The environmental clusters (#195 follow-ups): each reports a
    // `MeasuredValue` that moves continuously, so each is a sampled series and
    // belongs in the reading history rather than as a durable context row. A
    // pond that ingested these would be writing thousands of "the CO2 is now
    // 812 ppm" rows a day, which is the exact failure `DISCRETE_SENSOR_TYPES`
    // exists to prevent.
    "illuminance",
    "pressure",
    "flow",
    // `air_quality` is the AirQuality cluster's 0-6 enum, not a concentration
    // — the borderline case in this list. It is continuous because it is a
    // GRADE derived from the concentrations below it: it moves whenever they
    // do, several times an hour on a busy day, so treating it as a transition
    // would fill the corpus with "the air quality is now Fair". The event a
    // household actually wants from this family is the alarm, and that is
    // `smoke_alarm`, which is discrete.
    "air_quality",
    "carbon_monoxide",
    "carbon_dioxide",
    "nitrogen_dioxide",
    "ozone",
    "formaldehyde",
    "pm1",
    "pm2_5",
    "pm10",
    "radon",
    "total_volatile_organic_compounds",
    // Remaining filter life as a percentage. It falls continuously with use, so
    // it is a sampled series; the event a household wants out of this family is
    // the change indication, and that is discrete and kept.
    "hepa_filter_condition",
    "carbon_filter_condition",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/pond-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pond-core must live two directories below the workspace root")
        .to_path_buf()
}

fn matter_sensor_source() -> String {
    // The cluster map lives in the Matter controller now: it is TypeScript, next
    // to matter.js's typed cluster models, rather than a hand-maintained table of
    // decimal cluster ids in Rust. The vocabulary it mints is unchanged, so this
    // tripwire follows it rather than being retired — a rule that fails silently
    // needs its guard wherever the thing it guards has moved to.
    let path = workspace_root().join("matter-server/src/mapping/sensors.ts");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. If the Matter sensor map moved, this tripwire is watching a \
             file that no longer exists and would never fire.",
            path.display()
        )
    })
}

/// Every `sensor_type` the Matter bridge can mint, read out of its cluster map.
///
/// Each entry is written as `{ cluster: …, attribute: …, sensorType: "name", … }`,
/// so the signal name is the first string literal after each `sensorType: "`.
/// Collected by shape rather than by looking for the names this file already
/// knows, which is the difference between finding a new one and confirming the
/// old ones.
fn matter_sensor_types(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for fragment in body.split("sensorType: \"").skip(1) {
        if let Some((literal, _)) = fragment.split_once('"') {
            if !out.iter().any(|s| s == literal) {
                out.push(literal.to_string());
            }
        }
    }
    out
}

/// Vacuity control, and the reason the assertion below can be trusted: its
/// subject is another crate's source, so a wrong path or a changed spelling
/// would make it pass by reading nothing.
#[test]
fn the_tripwire_is_reading_the_matter_cluster_map() {
    let body = matter_sensor_source();
    assert!(
        body.contains("Reading"),
        "the file this test reads no longer produces sensor readings, so it is not the bridge \
         whose vocabulary this rule is calibrated against"
    );

    let found = matter_sensor_types(&body);
    assert!(
        found.len() >= 4,
        "the cluster map yielded only {found:?}. Either the mapping is now written differently — \
         in which case this tripwire cannot see it and is watching nothing — or the bridge lost \
         most of its clusters."
    );
    // Both directions must be represented, or the disposition test below is
    // quantified over one kind of signal and proves half of what it says.
    assert!(
        found.iter().any(|s| s == "contact"),
        "no discrete signal found in {found:?}"
    );
    assert!(
        found.iter().any(|s| s == "temperature"),
        "no continuous signal found in {found:?}"
    );
}

/// Every signal the Matter bridge can emit is dispositioned: kept as a
/// transition, or named as a measurement that is deliberately dropped.
#[test]
fn every_matter_signal_is_either_kept_or_named_as_a_measurement() {
    let found = matter_sensor_types(&matter_sensor_source());

    let undecided: Vec<&String> = found
        .iter()
        .filter(|s| {
            !DISCRETE_SENSOR_TYPES.contains(&s.as_str()) && !KNOWN_CONTINUOUS.contains(&s.as_str())
        })
        .collect();

    assert!(
        undecided.is_empty(),
        "the Matter bridge now emits {undecided:?}, and the on-pond producer has never heard of \
         it. `DISCRETE_SENSOR_TYPES` is an allow-list, so those readings are silently discarded — \
         the household's new device becomes the one thing the assistant never mentions, with \
         nothing anywhere reporting why. Decide which it is: add it to DISCRETE_SENSOR_TYPES in \
         crates/pond-core/src/context/producer.rs if it is a transition, or to KNOWN_CONTINUOUS in \
         this file if it is a measurement the reading series already holds. Signals found: \
         {found:?}"
    );

    // The two lists must stay disjoint, or a signal is both kept and recorded as
    // deliberately dropped and the disposition above means nothing.
    for signal in KNOWN_CONTINUOUS {
        assert!(
            !DISCRETE_SENSOR_TYPES.contains(signal),
            "`{signal}` is named here as a measurement and is also in DISCRETE_SENSOR_TYPES"
        );
    }
}
