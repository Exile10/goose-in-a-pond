//! RETIRED 2026-08-11, the same day it was written. This file is kept as a
//! record rather than deleted, per the convention this programme uses for a
//! guard whose absence has ended.
//!
//! # What it was for
//!
//! PAI-8 P1 and P2 landed with a complete pipeline, corpus, retention, retrieval
//! and scope filter that **nothing in production reached** -- and no guard
//! saying so. Working from the checklist row rather than the tree, I then wrote
//! "PAI-8 is the only workstream with no landed code" into three documents. This
//! file was the correction: it walked the whole workspace asserting that nothing
//! outside `pond-core/src/context/`, `sqlite_context.rs` and the MCP server
//! named `SqliteContextRepository`, `IngestPipeline::new`, `init_context_deps`
//! or `spawn_context_server`, and it carried a control proving the walk could
//! see the files it excluded -- necessary beyond the usual, because its subject
//! was an ABSENCE, where a broken walk passes.
//!
//! # What it caught
//!
//! Exactly what it was written for. `main.rs` now builds the repository, the
//! `IngestPipeline` and a `BusIngest` subscriber, and calls `init_context_deps`;
//! the test failed naming all three, with the three-step instruction it carried.
//! All three were done, in order:
//!
//! 1. PAI-8's section 4 says which phase landed and what is still missing.
//! 2. The rows in `00-checklist.md` and `personal-agentic-intelligence.md` were
//!    both corrected, so they cannot disagree.
//! 3. PAI-2 P6b's third part -- redaction before a body leaves the pond -- had
//!    been recorded as circularly blocked on PAI-8 creating a call site.
//!    `IngestPipeline::new` takes a `Redactor` that is deliberately not an
//!    `Option`, and `main.rs` now constructs one, so that chokepoint has a
//!    production caller. The dependency was never two-way.
//!
//! # What is still true, and what replaced this
//!
//! `giap-context` is still **not registered** in `giap_registration.rs`, so
//! PAI-8 P2's `search_context` and `get_recent_context` are unreachable by the
//! model even though `init_context_deps` has given them their handles. That is
//! deliberate ordering, not an oversight -- see PAI-8 section 4 -- and
//! `the_context_extension_is_not_in_the_builtin_registry_yet` below is the one
//! assertion from this file still worth running, so it is the only one kept.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pond-core must live two directories below the workspace root")
        .to_path_buf()
}

/// The extension is not registered, which is a separate claim from "no Rust
/// calls it": a builtin can be registered and its deps left uninstalled, and
/// PAI-6 P5 records that as the shape that produces a tool refusing every call
/// while looking like a working guard. Here it is the reverse -- the deps are
/// installed and the registration is withheld -- and that is the intended state
/// until a household can actually have context items worth searching.
#[test]
fn the_context_extension_is_not_in_the_builtin_registry_yet() {
    let root = workspace_root();
    let registration = root.join("crates/pond-adapters-goose/src/giap_registration.rs");
    let body = std::fs::read_to_string(&registration)
        .expect("giap_registration.rs must exist -- it is where every builtin is registered");
    // Control: the file really is the registry.
    assert!(
        body.contains("register_builtin_extension("),
        "giap_registration.rs no longer registers builtins; this guard is reading the wrong file"
    );
    assert!(
        !body.contains("giap-context"),
        "`giap-context` is registered. That changes the tool count this repo asserts in three \
         places -- CLAUDE.md's sentence, `TOOL_GROUPS` in tool_group.rs, and \
         registration_matches_the_catalog.rs, which fails if any two disagree. Update all of \
         them, give it an off-by-default settings toggle with its own named `default_*` fn, move \
         PAI-8's P2 stamp, and delete this test."
    );
}
