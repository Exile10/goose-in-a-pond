//! The on-pond producer's camera rule drops the label `pond-adapters-vision`
//! emits when nothing classified the frame. This file ties the two together.
//!
//! # Why this is a file and not a comment
//!
//! `UNCLASSIFIED_CAMERA_EVENT_TYPES` is the only thing standing between a
//! camera source and 8 640 rows a day. The pipeline's fallback label is a string
//! literal in another crate that `pond-core` cannot depend on (the dependency
//! direction is inward), so the coupling is real and invisible: rename
//! `"motion"` to `"movement"` in `pipeline.rs` and every frame the camera
//! notices becomes a durable, prompt-injected context item, with no compile
//! error and no failing test anywhere near the change.
//!
//! The failure is SILENT and it is in the expensive direction, which is the pair
//! of properties this programme has recorded twelve incidents about. So the
//! constant is asserted against the literal the other crate actually emits, by
//! reading its source — the same technique
//! `context_pipeline_is_not_wired_yet.rs` uses, and for the same reason: some
//! couplings cannot be expressed as types.
//!
//! This is a **tripwire, not coverage**. It proves the label this repo emits
//! today is refused. It cannot prove anything about a third-party camera
//! adapter, and the `pond-core` unit tests carry the behavioural claims.

use std::path::{Path, PathBuf};

use pond_core::context::producer::UNCLASSIFIED_CAMERA_EVENT_TYPES;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/pond-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pond-core must live two directories below the workspace root")
        .to_path_buf()
}

fn pipeline_source() -> String {
    let path = workspace_root().join("crates/pond-adapters-vision/src/pipeline.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. If the vision pipeline moved, this tripwire is watching a file \
             that no longer exists and would never fire.",
            path.display()
        )
    })
}

/// Vacuity control, and it is the whole reason this file can be trusted: the
/// assertions below are about the CONTENT of another crate's source, so a wrong
/// path or an emptied file would make every one of them pass by reading nothing.
#[test]
fn the_tripwire_is_reading_the_vision_pipeline() {
    let body = pipeline_source();
    assert!(
        body.contains("BusEvent::Camera("),
        "the file this test reads no longer publishes camera events to the bus, so it is not the \
         producer whose labels this rule is calibrated against"
    );
    assert!(
        body.contains("CameraEvent {"),
        "the file this test reads no longer constructs a CameraEvent"
    );
    assert!(
        body.contains("classifier"),
        "the file this test reads no longer has a classifier, so the fallback label it is being \
         checked for may no longer exist"
    );
}

/// Every label the pipeline falls back to when nothing classified the frame is
/// one the producer refuses.
///
/// The pipeline reaches its fallback in three ways — no classifier configured,
/// the classifier erroring, and the classifier finding nothing above its own
/// floor — and all three write the same literal. That literal is what a build
/// without `vision-onnx` emits for EVERY event, at the pipeline's 10-second
/// minimum interval.
#[test]
fn the_pipelines_unclassified_label_is_one_the_producer_refuses() {
    let body = pipeline_source();

    // The fallback is written as `("motion".to_string(), ...)`. Collect every
    // string literal the file turns into an owned event_type that way, rather
    // than looking for one known spelling — if the label is renamed, this finds
    // the new name and fails on it instead of silently finding nothing.
    let mut fallbacks: Vec<String> = Vec::new();
    for fragment in body.split("(\"").skip(1) {
        let Some((literal, rest)) = fragment.split_once('"') else {
            continue;
        };
        if rest.starts_with(".to_string(),") {
            fallbacks.push(literal.to_string());
        }
    }

    assert!(
        !fallbacks.is_empty(),
        "no `(\"label\".to_string(), ...)` fallback was found in the vision pipeline. Either the \
         fallback is now written differently -- in which case this tripwire cannot see it and is \
         watching nothing -- or the pipeline no longer has one."
    );

    for label in &fallbacks {
        assert!(
            UNCLASSIFIED_CAMERA_EVENT_TYPES.contains(&label.as_str()),
            "pond-adapters-vision emits `{label}` when nothing classified the frame, and the \
             on-pond producer does NOT refuse it. On a build without the vision-onnx classifier \
             that is every camera event, so every camera source would ingest roughly 8640 rows a \
             day saying only that the pixels changed -- and each of them is read back into a \
             model's context window. Add `{label}` to UNCLASSIFIED_CAMERA_EVENT_TYPES in \
             crates/pond-core/src/context/producer.rs, or say in that constant's docs why this \
             one names a household fact. Labels found: {fallbacks:?}"
        );
    }
}
