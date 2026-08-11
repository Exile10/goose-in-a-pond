//! PAI-7 P4's loop exists and **production reaches it**. This file is the proof
//! of the second half.
//!
//! It is the mirror image of `device_profile_rung_is_not_wired_yet.rs`, which
//! asserts an absence and fails the day a caller lands. This asserts a presence
//! and fails the day one goes away — and the reason it is needed is specific:
//! `ci.yml` runs `cargo check -p pond-server` and never `cargo test -p
//! pond-server`, so every line of wiring in `main.rs` is verified to COMPILE and
//! nothing verifies it is still called. Deleting the `tokio::spawn` below leaves
//! the whole workspace green and switches the feature off.
//!
//! `include_str!` creates no dependency edge and needs no link, so this runs in
//! CI's fast pass (PAI-2 P3's lesson).
//!
//! # What this is not
//!
//! It is a tripwire, not coverage. It cannot tell you the reviewer works; it can
//! only tell you the two lines that make it reachable are still there. The
//! behaviour is tested in `pond-core`'s `proactive_review` module, where it is
//! pure functions over plain values. This programme has one recorded incident of
//! a source tripwire that caught only a textual revert while the arguments
//! silently changed underneath it, so the assertions below are about the
//! ARGUMENTS wherever a wrong one would be silent.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

/// Guard against the guard. If `include_str!` stops pointing at the file this
/// test believes it points at, every assertion below passes by matching
/// nothing — four of this programme's recorded vacuous-test incidents were
/// exactly that shape.
#[test]
fn the_file_this_test_reads_is_the_one_it_thinks_it_is() {
    assert!(
        MAIN.contains("async fn run_server"),
        "MAIN is not pond-server/src/main.rs"
    );
    assert!(
        MAIN.contains("async fn run_proactive_reviewer"),
        "the reviewer loop has been renamed or removed from main.rs; if it was moved to another \
         crate this test needs a new path, and if it was deleted so was PAI-7 P4"
    );
}

/// The loop is spawned. Without this line the function compiles, the tests
/// pass, and the pond never has a thought of its own.
#[test]
fn the_reviewer_loop_is_actually_spawned() {
    assert!(
        MAIN.contains("tokio::spawn(run_proactive_reviewer("),
        "nothing spawns the proactive reviewer. `run_proactive_reviewer` is `pub`-less but \
         `dead_code` does not fire for a function reachable from a `tokio::spawn` that was \
         deleted along with it — and if it did, CI never compiles this crate's warnings as \
         errors. PAI-7 P4 is off. Fix the wiring or rewrite the stamp in \
         docs/architecture/pai/07-proactive-intelligence.md."
    );
}

/// The bus subscriber that fills the reviewer's ring must filter through
/// `reviewable`.
///
/// This is the assertion that is about an ARGUMENT rather than a presence, and
/// it is here because pushing the raw event is the natural simplification and
/// its symptom is invisible: the reviewer keeps running, the brief fills with
/// hourly clock ticks and session-lifecycle rows, and the twenty-four-line cap
/// evicts the camera event that was the only thing worth a suggestion. Nothing
/// errors. The pond just gets duller.
#[test]
fn only_reviewable_events_reach_the_reviewers_ring() {
    assert!(
        MAIN.contains("reviewable(&bus_event)"),
        "the reviewer's bus subscriber no longer filters through \
         `proactive_review::reviewable`. `Time` and `Session` events would then fill the ring \
         and crowd out the household facts a review is for"
    );
    // And the brief is built from what `brief_events` returns, not from the raw
    // ring: that is where a presence event about ANOTHER member is dropped.
    assert!(
        MAIN.contains("review::brief_events(&audience, &drained)"),
        "the brief is no longer built through `brief_events`, which is the only place a \
         presence event naming a different household member is removed. The brief is prose \
         handed straight to the model, so PAI-6's scope clamp cannot catch this"
    );
}

/// The reviewer must publish its authority into the registry the `delegate`
/// tool uses, not one of its own.
///
/// `GooseOrchestrator::spawn` looks the parent turn up there and refuses when it
/// finds nothing. A second registry compiles and answers `None` to every lookup,
/// so the failure mode is every review refused with "no live turn holds the
/// authority" — which reads like a guard working correctly.
#[test]
fn the_reviewer_uses_the_installed_orchestrator_and_not_one_of_its_own() {
    assert!(
        MAIN.contains("pond_mcp_server::installed_orchestrator_deps()"),
        "the reviewer no longer reads the installed orchestrator deps. If it now builds its \
         own `GooseOrchestrator`, every review will be refused for want of a live parent turn"
    );
    assert!(
        !MAIN.contains("TurnAuthorityRegistry::new()"),
        "something in main.rs constructs a second TurnAuthorityRegistry. There must be exactly \
         one — the adapter's — and both the `delegate` tool and the reviewer must read it"
    );
    assert!(
        MAIN.contains("review::review_authority(&audience, &session_id)"),
        "the reviewer publishes an authority it built itself rather than `review_authority`, \
         which `plan_review` also uses. Two constructions are two ceilings, and if their \
         session ids drift every spawn is refused"
    );
}
