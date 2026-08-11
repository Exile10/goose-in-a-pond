//! PAI-8's ingest pipeline and its MCP surface exist, and **nothing in
//! production reaches either**. This file is the proof of the second half, and
//! it is written to fail the day the first caller lands.
//!
//! # Why this file exists at all
//!
//! Because its absence caused a documentation error on 2026-08-11, in the same
//! session that wrote it. Three PAI-8 commits landed that day — the domain, the
//! migrations, the SQLite adapter, the ingest pipeline, retention, retrieval,
//! scope filtering and the `giap-context` MCP server — and the checklist row
//! still read `DESIGNED`. Working from the row rather than from the tree, I then
//! asserted in three separate documents that PAI-8 was "the only workstream with
//! no landed code". That is exactly what the checklist's own section 2.1 item 3
//! exists to prevent, and a comment would not have prevented it either.
//!
//! PAI-7 P3a shipped with a guard of this shape and it worked: it failed the day
//! the REST surface arrived, with a message telling its reader to correct the
//! stamp and delete it, and both were done in that order. PAI-8 shipped without
//! one. This is that guard, arriving late.
//!
//! # What "unreached" means here, precisely
//!
//! `pond-mcp-server/src/lib.rs` says `pub mod context;`, so the code COMPILES
//! and `cargo check` is happy. Nothing calls `init_context_deps`, nothing
//! registers `spawn_context_server` as a builtin extension, nothing constructs
//! `SqliteContextRepository`, and nothing builds an `IngestPipeline`. A `pub`
//! item in a library crate never earns a `dead_code` warning, which is the blind
//! spot that let PAI-1 P5 be recorded as landed while inert and PAI-6 P1's clamp
//! ship with its one call site missing.
//!
//! So: the corpus can be stored, searched, scoped, ranked and swept, and no
//! household will ever have a row in it, because no source can be created and
//! nothing ingests. That is a legitimate state for a storage-and-domain phase —
//! PAI-6 P1 was one — but only when it is stated, and it must be stated as an
//! assertion rather than as prose.

use std::path::{Path, PathBuf};

/// The three files that legitimately own these symbols. Everything else naming
/// one is a caller, and a caller is what this test is watching for.
const OWNING_FILES: &[&str] = &[
    "crates/pond-core/src/context/",
    "crates/pond-infra/src/sqlite_context.rs",
    "crates/pond-mcp-server/src/context.rs",
];

/// Symbols whose appearance outside the owning files means production has
/// started using PAI-8.
const WIRING_MARKERS: &[&str] = &[
    "SqliteContextRepository",
    "IngestPipeline::new",
    "init_context_deps",
    "spawn_context_server",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/pond-core.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pond-core must live two directories below the workspace root")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // `target` is build output and `goose` is the submodule; neither is
            // this workspace's source and both are enormous.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name == "goose" || name == "node_modules" {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn is_owned(relative: &str) -> bool {
    OWNING_FILES.iter().any(|owner| relative.starts_with(owner))
}

/// Guard against the guard: the walk must actually find the symbols where they
/// live. Without this, a wrong root or a broken walk makes every assertion below
/// pass by reading nothing — four of this programme's recorded vacuous-test
/// incidents were exactly that shape, and this one has the extra hazard that
/// its subject is an ABSENCE, where finding nothing is the expected answer.
#[test]
fn the_walk_can_see_the_files_it_is_excluding() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "the walk found only {} .rs files under crates/ — it is not reading this workspace",
        files.len()
    );

    for marker in WIRING_MARKERS {
        let found_in_owner = files.iter().any(|path| {
            let relative = path.strip_prefix(&root).unwrap_or(path).to_string_lossy();
            is_owned(&relative)
                && std::fs::read_to_string(path)
                    .map(|body| body.contains(marker))
                    .unwrap_or(false)
        });
        assert!(
            found_in_owner,
            "`{marker}` was not found in any of the owning files {OWNING_FILES:?}. Either the \
             symbol was renamed — in which case this guard is watching for something that no \
             longer exists and would never fire — or PAI-8's module has moved."
        );
    }
}

/// The claim. When it fails, PAI-8 has a production caller, and the stamps have
/// to move before the test does.
#[test]
fn nothing_outside_pai_8s_own_module_reaches_the_context_pipeline() {
    let root = workspace_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);

    let mut offenders: Vec<String> = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_owned(&relative) {
            continue;
        }
        // This file names every marker in its own constants; excluding it is not
        // a loophole, it is the difference between a guard and a mirror.
        if relative.ends_with("context_pipeline_is_not_wired_yet.rs") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        for marker in WIRING_MARKERS {
            if body.contains(marker) {
                offenders.push(format!("{relative} names `{marker}`"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "PAI-8's pipeline now has a caller: {offenders:?}.\n\
         Before deleting this test, do all three of these, in this order:\n\
         1. Say which phase landed, in docs/architecture/pai/08-personal-context-streaming.md \
            section 4 — P1 is the ingest pipeline, P2 is retrieval and the `giap-context` \
            extension, P3 is `POST /context/ingest`.\n\
         2. Correct the PAI-8 rows in docs/architecture/pai/00-checklist.md AND in \
            docs/architecture/personal-agentic-intelligence.md. Both, or they disagree.\n\
         3. Check what the caller implies for PAI-2 P6b's third part, which has been blocked \
            on PAI-8 creating a redaction call site and may now be unblocked.\n\
         Then delete this file, and leave a retirement note saying what it caught — the \
         convention this programme uses for a guard whose absence has ended."
    );
}

/// The extension is not registered either, which is a separate claim from "no
/// Rust calls it": a builtin can be registered and its deps left uninstalled,
/// and PAI-6 P5 records that as the shape that produces a tool refusing every
/// call while looking like a working guard.
#[test]
fn the_context_extension_is_not_in_the_builtin_registry_yet() {
    let root = workspace_root();
    let registration = root.join("crates/pond-adapters-goose/src/giap_registration.rs");
    let body = std::fs::read_to_string(&registration)
        .expect("giap_registration.rs must exist — it is where every builtin is registered");
    // Control: the file really is the registry.
    assert!(
        body.contains("register_builtin_extension("),
        "giap_registration.rs no longer registers builtins; this guard is reading the wrong file"
    );
    assert!(
        !body.contains("giap-context"),
        "`giap-context` is registered. That changes the tool count this repo asserts in three \
         places — CLAUDE.md's sentence, `TOOL_GROUPS` in tool_group.rs, and \
         registration_matches_the_catalog.rs, which fails if any two disagree. Update all of \
         them, move PAI-8's P2 stamp, and delete this test."
    );
}
