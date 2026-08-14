//! PAI-8's ingest pipeline is reached from production, and this asserts it.
//!
//! # The record this file replaces
//!
//! It was `context_pipeline_is_not_wired_yet.rs`, written on 2026-08-11 and
//! retired the same day, and the story is worth keeping because it is the one
//! this programme keeps re-learning.
//!
//! PAI-8 P1 and P2 landed with a complete pipeline, corpus, retention, retrieval
//! and scope filter that **nothing in production reached** -- and no guard
//! saying so. `pub mod context;` makes it all compile, and a `pub` item in a
//! library crate never earns a `dead_code` warning, so nothing complained.
//! Reading the checklist row rather than the tree, I then wrote "PAI-8 is the
//! only workstream with no landed code" into three documents. The guard was the
//! correction: it walked the workspace asserting the absence, with a control
//! proving the walk could see the files it excluded -- necessary beyond the
//! usual, because its subject was an ABSENCE, where a broken walk passes.
//!
//! It fired the same day, naming all three new call sites, and carried a
//! three-step instruction that was followed in order: stamp the phase, correct
//! BOTH status documents, and re-check what the caller meant for PAI-2 P6b's
//! third part (which turned out never to have been circularly blocked).
//!
//! # What is asserted now, and why it is not covered elsewhere
//!
//! `ci.yml` runs `cargo check -p pond-server` and never `cargo test -p
//! pond-server`, so every line of wiring in `main.rs` is verified to COMPILE and
//! nothing verifies it is still called. The same gap `proactive_reviewer_is_wired.rs`
//! covers for PAI-7 P4.
//!
//! The registration half is NOT re-asserted here: `registration_matches_the_catalog.rs`
//! already ties `giap_registration.rs`, `TOOL_GROUPS` and AGENTS.md's sentence
//! to each other and fails if any two disagree.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

/// Guard against the guard. Four of this programme's recorded vacuous-test
/// incidents were an `include_str!` that had stopped pointing at its subject,
/// after which every assertion passes by matching nothing.
#[test]
fn the_file_this_test_reads_is_the_one_it_thinks_it_is() {
    assert!(
        MAIN.contains("async fn run_server"),
        "MAIN is not pond-server/src/main.rs"
    );
}

/// The corpus has a producer, and the producer is subscribed to the bus.
///
/// Without the subscriber the pipeline is built and never called, which is
/// indistinguishable at compile time from this working.
#[test]
fn the_bus_subscriber_that_feeds_the_corpus_is_still_spawned() {
    assert!(
        MAIN.contains("BusIngest::new"),
        "nothing builds a `BusIngest`, so no bus event can ever become a context item and \
         `context_items` is empty on every pond again. PAI-8 P1 is off."
    );
    assert!(
        MAIN.contains(".absorb(&settings, &bus_event"),
        "the context ingest no longer absorbs bus events. A `BusIngest` that is constructed and \
         never called is exactly the shape this file's predecessor existed to catch."
    );
}

/// PAI-2 P3's third redaction chokepoint, which has no other guard.
///
/// `IngestPipeline::new` takes a `Redactor` by value rather than as an `Option`,
/// so redaction-before-persistence is enforced by the type -- but only for code
/// that constructs a pipeline. This asserts production still does, because the
/// ledger recorded that chokepoint as blocked for weeks and its unblocking is
/// this one call site.
#[test]
fn the_ingest_pipeline_is_constructed_with_a_redactor() {
    assert!(
        MAIN.contains("IngestPipeline::new"),
        "production no longer constructs an `IngestPipeline`. PAI-2 P6b's third part -- redaction \
         before a body leaves the pond -- goes back to having no call site, which is the state \
         the ledger recorded as blocked."
    );
    assert!(
        MAIN.contains("init_context_deps"),
        "`init_context_deps` is not called, so `search_context` and `get_recent_context` refuse \
         every call -- while looking exactly like a working guard, which PAI-6 P5 records as the \
         most misleading failure shape available here."
    );
}
