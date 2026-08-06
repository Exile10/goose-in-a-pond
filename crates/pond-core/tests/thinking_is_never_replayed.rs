//! PAI-5 P6, invariant 3: reasoning text is stored for the USER, never for the
//! MODEL.
//!
//! `session_thinking` holds the passages a model produced while working out an
//! answer. They are candid, unedited, and frequently wrong -- that is what makes
//! them worth showing a person and disqualifying as context. Feeding a model its
//! own discarded scratch work costs tokens to re-read a conclusion it already
//! superseded, and on a Jetson those tokens come out of the same reserve the
//! answer is decoded from.
//!
//! The regression is not malice, it is helpfulness. Section 3.4 names the exact
//! shape: somebody notices the summariser's transcript loses the model's
//! rationale, sees a method that returns exactly that rationale, and joins them
//! "for continuity". Nothing fails. The prompt silently doubles.
//!
//! So this enumerates the permitted callers of
//! `SessionStorage::get_thinking_for_session` and fails naming the file when a
//! new one appears.
//!
//! WHY A SOURCE SCAN AND NOT A TYPE-LEVEL GUARD. The honest alternative is a
//! separate read-only port that only the HTTP layer can name. That was the
//! original design and it was dropped for a concrete reason: a new port needs a
//! new `AppState` field, `AppState` has 29 literal construction sites, and
//! "the adapter exists but production never wired it" is a failure this
//! programme has recorded three times. The method therefore lives on
//! `SessionStorage`, which every prompt-building path can already reach -- so
//! the reachability has to be constrained by a guard rather than by the borrow
//! checker, and the guard has to be honest about being weaker.
//!
//! THE TWO VACUITY DEFECTS THIS FILE INHERITS FIXES FOR, both proven in
//! `egress_guard.rs` by mutation and both reproduced here rather than
//! rediscovered:
//!
//! * A bare-symbol grep is satisfied by COMMENT PROSE. The doc comment on the
//!   port method above names `get_thinking_for_session` several times, and the
//!   handler's comment names it once. Matching the CALL FORM (with the opening
//!   paren) is half the fix; [`strip_line_comments`] is the other half, because
//!   `// never call get_thinking_for_session(...)` defeats the call form too.
//!   Without both, this guard would have been green on the day it landed for
//!   reasons that have nothing to do with the code.
//! * A walk that matches nothing reports success. [`FLOOR_FILES`] and the
//!   must-find assertion on the two known callers are the vacuity controls: if
//!   the port method is renamed, or the walk breaks, this fails rather than
//!   certifying an empty result.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The call form, never the bare symbol. See the module docs.
const READ_CALL: &str = "get_thinking_for_session(";
/// The write side, guarded the same way: a prompt path that WRITES reasoning is
/// not the hazard, but a prompt path that has any business with this table at
/// all is a signal, and the two lists are cheap to keep together.
const WRITE_CALL: &str = "add_thinking(";

/// Below this the walk has broken, not the tree shrunk.
const FLOOR_FILES: usize = 300;

/// The only files allowed to READ stored reasoning.
///
/// `session_storage.rs` defines the method and `sqlite_session_storage.rs`
/// implements it -- neither is a caller in the sense that matters, but both
/// contain the call form (the impl's own signature, and its unit tests' calls,
/// which `production_source` does not remove because they are `#[cfg(test)]`
/// items it strips... and therefore do not appear; the signature does).
///
/// `routes.rs` is the ONE real caller: `get_session_messages` serialises the
/// blocks into an HTTP response. Nothing in that handler reaches a model.
const READERS_ALLOWED: &[&str] = &[
    "crates/pond-core/src/user_data/ports/session_storage.rs",
    "crates/pond-infra/src/sqlite_session_storage.rs",
    "crates/pond-api/src/routes.rs",
];

/// The only files allowed to WRITE stored reasoning.
///
/// `chat.rs` is the sole owner of turn persistence -- the same rule that keeps
/// memory extraction out of handlers keeps this out of them too.
const WRITERS_ALLOWED: &[&str] = &[
    "crates/pond-core/src/user_data/ports/session_storage.rs",
    "crates/pond-infra/src/sqlite_session_storage.rs",
    "crates/pond-core/src/shared/services/chat.rs",
];

/// Files that MUST contain the read call, so a rename cannot empty the scan and
/// leave every assertion below trivially satisfied.
const READERS_REQUIRED: &[&str] = &[
    "crates/pond-core/src/user_data/ports/session_storage.rs",
    "crates/pond-api/src/routes.rs",
];

/// Directories whose files build a prompt. A hit here is reported with the
/// reason, because "you have added reasoning text to the model's input" is a
/// more useful message than "unlisted caller".
const PROMPT_BUILDING_PATHS: &[(&str, &str)] = &[
    (
        "models/services/context/",
        "the context pipeline -- trimming, compaction and budgeting all feed \
         the next prompt",
    ),
    (
        "shared/services/session_summary.rs",
        "the rolling summariser, whose output is prepended to every subsequent \
         turn",
    ),
    (
        "prompt_builder",
        "the prompt builder -- everything it assembles is sent to the model",
    ),
    (
        "prompts.rs",
        "the system-prompt templates, rendered into every turn",
    ),
    (
        "turn_trimmer",
        "the turn trimmer, which decides what survives into the next prompt",
    ),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

/// The file with every `#[cfg(test)]` ITEM removed. Lifted from
/// `egress_guard.rs`, including its lesson: "everything before the first
/// `#[cfg(test)]`" is what the convention looks like and is not a rule, so this
/// removes the items instead of truncating at the first one.
fn production_source(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.trim_start().starts_with("#[cfg(test)]") {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let opens_block = lines
            .get(i + 1)
            .map(|l| l.trim_end().ends_with('{'))
            .unwrap_or(false);
        if !opens_block {
            i += 2;
            continue;
        }
        let closer = format!("{}}}", " ".repeat(indent));
        let mut j = i + 1;
        while j < lines.len() && lines[j].trim_end() != closer {
            j += 1;
        }
        i = j + 1;
    }
    out
}

/// The source with every `//` line comment removed, string literals intact.
/// Lifted from `egress_guard.rs`. Without it, this file's own explanatory
/// comments -- and the port method's docs, which name the call several times --
/// would register as callers.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut cut = line.len();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name == "target" || name == "tests" || name.starts_with('.') {
                continue;
            }
            collect_rs(&path, root, out);
        } else if name.ends_with(".rs") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// (files scanned, files containing `needle` in production, comment-free source)
fn scan(needle: &str) -> (usize, BTreeSet<String>) {
    let root = workspace_root();
    let crates = root.join("crates");
    assert!(
        crates.is_dir(),
        "no {} -- this guard scans the workspace and cannot run without it",
        crates.display()
    );

    let mut files = Vec::new();
    collect_rs(&crates, &root, &mut files);
    files.sort();

    assert!(
        files.len() >= FLOOR_FILES,
        "scanned only {} .rs files under crates/ (floor {FLOOR_FILES}) -- the \
         walk has broken, not the tree shrunk. A guard that matches nothing \
         reports success.",
        files.len()
    );

    let mut hits = BTreeSet::new();
    for rel in &files {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
        let code = strip_line_comments(&production_source(&src));
        if code.contains(needle) {
            hits.insert(rel.clone());
        }
    }
    (files.len(), hits)
}

fn describe_prompt_path(file: &str) -> Option<&'static str> {
    PROMPT_BUILDING_PATHS
        .iter()
        .find(|(frag, _)| file.contains(frag))
        .map(|(_, why)| *why)
}

// -- the assertions -----------------------------------------------------------

/// THE LOAD-BEARING ONE. Stored reasoning may be read by the HTTP history
/// handler and by nothing else.
#[test]
fn stored_reasoning_is_read_only_by_the_history_handler() {
    let (_, hits) = scan(READ_CALL);

    // Vacuity control first: if the known callers are gone, the scan is not
    // proving anything and must say so rather than pass.
    for required in READERS_REQUIRED {
        assert!(
            hits.contains(*required),
            "`{READ_CALL}` no longer appears in the production source of \
             {required}. Either the method was renamed -- in which case this \
             guard is matching a string that no longer exists and is certifying \
             an empty result -- or the history handler stopped replaying stored \
             reasoning to the UI. Update {READ_CALL} here, or delete the \
             feature properly. Files that did match: {hits:?}"
        );
    }

    let allowed: BTreeSet<&str> = READERS_ALLOWED.iter().copied().collect();
    let unexpected: Vec<String> = hits
        .iter()
        .filter(|f| !allowed.contains(f.as_str()))
        .map(|f| match describe_prompt_path(f) {
            Some(why) => format!("{f}\n      ^ this is {why}"),
            None => format!("{f}"),
        })
        .collect();

    assert!(
        unexpected.is_empty(),
        "stored reasoning text is being read outside the history handler:\n    \
         {}\n\n  `session_thinking` holds passages the model produced while \
         working out an answer -- superseded, unreviewed, and frequently wrong. \
         PAI-5's third invariant is that they are NEVER replayed into a prompt. \
         If this call is on a path that builds model input, the prompt now \
         carries the model's own discarded scratch work, which costs decode \
         tokens out of the same reserve the answer comes from and reintroduces \
         conclusions the turn already rejected.\n\n  If a new UI read genuinely \
         needs this, add the file to READERS_ALLOWED in this test AND say in \
         the commit message why it cannot reach a model.",
        unexpected.join("\n    ")
    );
}

/// The write side. `ChatService` owns turn persistence; this is a corollary of
/// the rule that already keeps memory extraction out of handlers.
#[test]
fn stored_reasoning_is_written_only_by_the_persistence_owner() {
    let (_, hits) = scan(WRITE_CALL);

    assert!(
        hits.contains("crates/pond-core/src/shared/services/chat.rs"),
        "`{WRITE_CALL}` no longer appears in ChatService. Nothing writes \
         reasoning text any more, and the read side, the migration and the \
         setting are all still in the tree describing a feature that does not \
         happen. Files that did match: {hits:?}"
    );

    let allowed: BTreeSet<&str> = WRITERS_ALLOWED.iter().copied().collect();
    let unexpected: Vec<&String> = hits
        .iter()
        .filter(|f| !allowed.contains(f.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "reasoning text is being written outside ChatService: {unexpected:?}. \
         ChatService is the sole owner of turn persistence -- a handler that \
         writes its own rows cannot key them to the assistant message id, which \
         ChatService mints, and will be silently dropped by the next refactor \
         exactly as the inline persistence blocks were."
    );
}

/// The gate is a setting, and a setting that nothing consults is decoration.
///
/// This is the shape PAI-5 P1 shipped and P5 caught: `is_voice` was guarded as
/// an identifier while the composition that produced it was not, so dropping
/// the per-request flag left 108 tests green and leaked reasoning on every
/// desktop voice turn. So this asserts the SETTING reaches the builder, in both
/// stream handlers, by call form -- not that the word `persist_thinking` occurs
/// somewhere in `routes.rs`.
#[test]
fn both_stream_handlers_hand_the_users_choice_to_the_persistence_owner() {
    let root = workspace_root();
    let src = std::fs::read_to_string(root.join("crates/pond-api/src/routes.rs"))
        .expect("routes.rs readable");
    let code = strip_line_comments(&production_source(&src));

    let with_thinking = code.matches(".with_thinking(").count();
    assert_eq!(
        with_thinking, 2,
        "expected exactly 2 `.with_thinking(` calls in routes.rs (one per \
         streaming chat handler); found {with_thinking}. Fewer means a handler \
         builds a ChatService that silently defaults to NOT persisting -- which \
         is the safe direction but means the user's setting does nothing on \
         that route. More means a third stream handler appeared and this guard \
         has not been told which."
    );

    // The ARGUMENT, not just the call. `.with_thinking(true)` would satisfy a
    // call-form count while ignoring the setting entirely -- a privacy store
    // that overrides its own gate is worse than no store.
    assert!(
        code.contains(".with_thinking(settings.persist_thinking)"),
        "the `/chat/stream` handler must pass `settings.persist_thinking` to \
         `.with_thinking(`, not a literal. A hardcoded `true` keeps every \
         reasoning passage the model has ever produced regardless of what the \
         user chose."
    );
    assert!(
        code.contains(".with_thinking(persist_thinking)")
            && code.contains("|s| s.persist_thinking"),
        "the `/agent/chat/stream` handler must read `persist_thinking` from the \
         settings repository and pass it through. This route reads no other \
         setting, so it is the one where a hardcoded literal would be easiest \
         to write and hardest to notice."
    );

    // And the offer itself: the gate is inside `record_thinking`, so the
    // handlers must actually call it or the buffer is always empty and
    // `persist_thinking = true` does nothing.
    let recorded = code.matches("record_thinking(").count();
    assert_eq!(
        recorded, 2,
        "expected exactly 2 `record_thinking(` calls in routes.rs (one per \
         `AgentStreamEvent::Thinking` arm); found {recorded}. Zero means the \
         thinking frames stream to the browser and are dropped, which is the \
         behaviour P6 exists to change, and every other assertion in this file \
         would still pass."
    );
}
