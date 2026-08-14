//! The registration list and the tool-group catalog describe the same set.
//!
//! PAI-6 P5 owed this test, and the reason it was owed is that the failure mode
//! is silent AND it widens. `tool_selection.rs :: is_catalog_extension` treats
//! anything the catalog does not know about as a **user-added MCP server**, which
//! selection deliberately never narrows — the user added it on purpose and GIAP
//! has no description to score it against. So a builtin registered under a name
//! the catalog does not carry is not "missing from a list": it becomes the one
//! extension that can never be selected away, is never subtracted for a guest by
//! the group-level pass, and is never withheld from a subagent. For
//! `giap-orchestrator`, which should be the least present extension on the pond,
//! that inverts the whole intent.
//!
//! It lives in `pond-core`'s test directory rather than in `pond-adapters-goose`
//! for one reason: CI runs `cargo test -p pond-core` and only `cargo check`s
//! `pond-adapters-goose`. A guard that CI never executes is a guard that fails
//! for the first time during a release.
//!
//! # Why this parses source instead of calling `register_giap_extensions`
//!
//! Calling it would need the goose submodule, twelve repositories and a
//! `Settings` with every toggle on — and `REGISTERED_EXTENSIONS` is a `OnceLock`,
//! so a second call in the same process is a no-op. Source is the honest input
//! here; what matters is that the parser cannot silently see nothing.

use pond_core::mcp::domain::tool_group::{ORCHESTRATOR_EXTENSION, TOOLKIT_EXTENSION, TOOL_GROUPS};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // crates/pond-core -> repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn registration_source() -> String {
    let path = workspace_root().join("crates/pond-adapters-goose/src/giap_registration.rs");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    // Line comments out, so a commented-out registration cannot be counted and
    // so prose in a doc comment cannot satisfy this guard. (Recorded vacuity
    // shape 1: a guard satisfied by comment text rather than by code.)
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first argument of every `register_builtin_extension(...)` call, resolved
/// to the extension name it actually registers.
///
/// **The resolver refuses to skip.** An argument it cannot resolve panics rather
/// than being dropped, because dropping one is precisely how this programme got
/// the extension count wrong twice: `giap-toolkit` registers through a const, so
/// every grep for a `"giap-*"` string literal undercounted by one, and the
/// correction was applied in the wrong direction with confidence.
fn registered_extensions_from_source() -> Vec<String> {
    let src = registration_source();
    let mut names = Vec::new();
    let mut rest = src.as_str();
    const CALL: &str = "register_builtin_extension(";
    while let Some(i) = rest.find(CALL) {
        rest = &rest[i + CALL.len()..];
        // The first argument runs to the first comma at paren depth 0.
        let mut depth = 0usize;
        let mut end = rest.len();
        for (j, c) in rest.char_indices() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' if depth > 0 => depth -= 1,
                ')' if depth == 0 => {
                    end = j;
                    break;
                }
                ',' if depth == 0 => {
                    end = j;
                    break;
                }
                _ => {}
            }
        }
        let arg = rest[..end].trim();
        names.push(resolve_argument(arg));
        rest = &rest[end..];
    }
    names
}

/// Turn one call argument into the name it registers.
fn resolve_argument(arg: &str) -> String {
    if let Some(literal) = arg.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return literal.to_string();
    }
    // A path ending in a const this crate owns. Compared on the LAST segment so
    // the `pond_core::mcp::domain::tool_group::` prefix is irrelevant.
    let last = arg.rsplit("::").next().unwrap_or(arg).trim();
    match last {
        "TOOLKIT_EXTENSION" => TOOLKIT_EXTENSION.to_string(),
        "ORCHESTRATOR_EXTENSION" => ORCHESTRATOR_EXTENSION.to_string(),
        _ => panic!(
            "giap_registration.rs registers an extension under `{arg}`, which this test cannot \
             resolve to a name. Do NOT delete the call from the parser's view and do not make \
             the parser skip it -- an unresolved registration is exactly how the extension count \
             was recorded wrong twice. Add the const to `resolve_argument`."
        ),
    }
}

/// The parser sees real registrations, so every assertion below is about the
/// content rather than about an empty set.
///
/// Recorded vacuity shape: a source-reading guard that silently matches nothing
/// passes everything. The bound is deliberately loose — it is a "the parser
/// works" control, not a pin on today's number, which the set equality below
/// already is.
#[test]
fn the_parser_actually_finds_registrations() {
    let found = registered_extensions_from_source();
    assert!(
        found.len() > 10,
        "the registration parser found only {} extension(s) in giap_registration.rs. That is a \
         broken parser, not a shrunken registry -- every other assertion in this file would pass \
         vacuously.",
        found.len()
    );
    assert!(
        found.iter().any(|n| n == TOOLKIT_EXTENSION),
        "the parser did not resolve the one registration that goes through a const. That is the \
         exact blind spot that made this programme's extension count wrong twice."
    );
    assert!(
        found.iter().all(|n| n.starts_with("giap-")),
        "resolved a registration to something that is not a giap extension name: {found:?}"
    );
}

/// Registration and catalog describe the SAME set, in both directions.
///
/// - A registered extension the catalog does not know about is treated as a
///   user-added MCP server: never narrowed by selection, never subtracted for a
///   guest by the group pass, never withheld from a subagent.
/// - A catalogued extension nothing registers is a group the scorer can choose,
///   `giap-toolkit` can be asked to enable, and which does not exist — a dead
///   end the model spends turns discovering.
#[test]
fn every_registered_extension_is_in_the_catalog_and_the_reverse() {
    let registered: BTreeSet<String> = registered_extensions_from_source().into_iter().collect();
    let catalogued: BTreeSet<String> = TOOL_GROUPS
        .iter()
        .map(|g| g.extension.to_string())
        .collect();

    let uncatalogued: Vec<&String> = registered.difference(&catalogued).collect();
    assert!(
        uncatalogued.is_empty(),
        "registered but not in TOOL_GROUPS: {uncatalogued:?}. An uncatalogued extension is \
         treated as a user-added MCP server, which selection never narrows and neither denylist \
         can remove."
    );

    let unregistered: Vec<&String> = catalogued.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "in TOOL_GROUPS but never registered: {unregistered:?}. The scorer can select it and \
         giap-toolkit can be asked to enable it, and it does not exist."
    );
}

/// The extension count is a number that goes stale. It has been wrong twice in
/// this programme — once at 14 when the answer was 15, and once "corrected" from
/// 15 back to 14 by a grep that could not see the const.
///
/// # Why this pins a literal instead of reading the guidance document
///
/// It used to read that number out of `CLAUDE.md`, and then `AGENTS.md`, and
/// assert the prose against the code. That tie is no longer possible: the agent
/// guidance is deliberately **not tracked** (`/AGENTS.md` and `/CLAUDE.md` are
/// both gitignored), so on a CI checkout there is no file to read and the test
/// would fail with "cannot read" rather than with anything about extensions.
///
/// Reading it *if present* was the other option and is worse: it passes
/// vacuously wherever the file is absent, which is exactly where the guard is
/// supposed to run. So the claim is re-anchored to the only place left in the
/// tree — this literal. Changing the number still costs a deliberate edit with
/// this comment in front of it, which is the property that mattered. Update the
/// local `AGENTS.md` sentence in the same change; nothing can check that for you
/// any more.
#[test]
fn the_extension_count_is_pinned() {
    const CLAIMED: usize = 17;

    // The REGISTRATION count, not the catalog's length: it is what the counting
    // recipe in the guidance produces, and the two are pinned to each other by
    // the test above.
    let registered = registered_extensions_from_source().len();
    assert_eq!(
        CLAIMED, registered,
        "this test claims {CLAIMED} builtin extensions; giap_registration.rs has {registered} \
         `register_builtin_extension(` call sites. Count the CALL SITES -- the import line has \
         no open paren, so nothing is subtracted from the grep, and two call sites pass a const \
         rather than a string literal."
    );
}

// ── The third list in the family ───────────────────────────────────────────

/// `dispatcher.rs`'s routed prefixes, as extension names.
///
/// Parsed from `prefix: PREFIX_X` occurrences rather than from the `PREFIX_*`
/// declarations, because the two differ on purpose: `PREFIX_AUDIT` is declared
/// and deliberately not routed, and a guard that read declarations would call
/// that a match.
fn dispatcher_routed_extensions() -> BTreeSet<String> {
    let path = workspace_root().join("crates/pond-mcp-server/src/dispatcher.rs");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let code: String = src
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    // const PREFIX_WEATHER: &str = "giap-weather__";
    let mut by_const: std::collections::BTreeMap<String, String> = Default::default();
    for line in code.lines() {
        let Some(rest) = line.trim().strip_prefix("const PREFIX_") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once(':') else {
            continue;
        };
        let Some(open) = tail.find('"') else { continue };
        let Some(close) = tail[open + 1..].find('"') else {
            continue;
        };
        let value = &tail[open + 1..open + 1 + close];
        by_const.insert(
            format!("PREFIX_{}", name.trim()),
            value.trim_end_matches("__").to_string(),
        );
    }
    assert!(
        by_const.len() >= 10,
        "found {} PREFIX_* consts in dispatcher.rs — the parser is broken, so an \
         empty result would prove nothing",
        by_const.len()
    );

    let mut routed = BTreeSet::new();
    for (at, _) in code.match_indices("prefix: PREFIX_") {
        let tail = &code[at + "prefix: ".len()..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(tail.len());
        let ident = &tail[..end];
        let ext = by_const.get(ident).unwrap_or_else(|| {
            panic!("dispatcher.rs routes `{ident}`, which is not a PREFIX_* const it declares")
        });
        routed.insert(ext.clone());
    }
    routed
}

/// Extensions the direct dispatcher deliberately does not route, and why.
///
/// Every entry is a REASON, not a name on a list. Adding a new extension makes
/// the test below fail until somebody either routes it or writes down why not,
/// which is the point: this drifted to 11-against-17 silently.
const DISPATCHER_EXCLUSIONS: &[(&str, &str)] = &[
    (
        "giap-audit",
        "needs the EventLog installed by pond-server's init_audit_deps; \
         McpToolDispatcher::new receives no logs DB",
    ),
    (
        "giap-vision",
        "needs the CameraStorage installed by pond-server's init_vision_deps",
    ),
    (
        "giap-sensors",
        "needs the SensorStorage installed by pond-server's init_sensor_deps",
    ),
    (
        "giap-context",
        "needs the context deps AND a resolvable caller; these routes carry no \
         engine session, so scope_for could only ever refuse",
    ),
    (
        "giap-toolkit",
        "widens a SESSION's tool selection, and these routes have no session",
    ),
    (
        "giap-orchestrator",
        "delegation needs a live turn authority, which only a chat turn publishes",
    ),
];

/// The dispatcher, the registration list and the catalog are three views of one
/// set, and only two of them were tied together.
///
/// `dispatcher.rs` is a SECOND live dispatch path — `main.rs` binds it into
/// `AppState` unconditionally and `POST /api/v1/tools/invoke` and
/// `POST /api/v1/mcp/tools/call` serve it. Its `servers` vec had drifted to 11
/// against `giap_registration.rs`'s 17, and for four of the six missing ones the
/// omission was undocumented — so nobody could tell an intentional exclusion
/// from a forgotten one.
///
/// What actually keeps that path safe is `routes.rs :: DIRECT_DISPATCH_ALLOWLIST`,
/// which is three tools. This test does not weaken that: it only requires the
/// two lists to agree about what EXISTS.
#[test]
fn the_dispatcher_routes_a_documented_subset_of_the_registered_extensions() {
    let registered: BTreeSet<String> = registered_extensions_from_source().into_iter().collect();
    let routed = dispatcher_routed_extensions();
    let excluded: BTreeSet<String> = DISPATCHER_EXCLUSIONS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();

    assert!(
        !routed.is_empty(),
        "the dispatcher routes nothing — parser broken"
    );

    // 1. Nothing routed that is not registered: a phantom prefix can never match
    //    a real tool.
    for ext in &routed {
        assert!(
            registered.contains(ext),
            "dispatcher.rs routes '{ext}', which giap_registration.rs does not register"
        );
    }

    // 2. Nothing both excluded and routed — a contradiction inside this test's
    //    own input.
    for ext in &excluded {
        assert!(
            !routed.contains(ext),
            "'{ext}' is on DISPATCHER_EXCLUSIONS and is routed anyway"
        );
    }

    // 3. Every registered extension is routed, or excluded WITH a reason.
    let unexplained: Vec<&String> = registered
        .iter()
        .filter(|e| !routed.contains(*e) && !excluded.contains(*e))
        .collect();
    assert!(
        unexplained.is_empty(),
        "registered but neither routed by dispatcher.rs nor listed in \
         DISPATCHER_EXCLUSIONS with a reason: {unexplained:?}\n\
         Route it, or say why it cannot be — a silent omission is \
         indistinguishable from a forgotten one, which is how this drifted."
    );

    // 4. Vacuity control on the exclusion list: an entry naming an extension
    //    that no longer exists is dead weight that hides real drift.
    for (ext, reason) in DISPATCHER_EXCLUSIONS {
        assert!(
            registered.contains(&ext.to_string()),
            "DISPATCHER_EXCLUSIONS names '{ext}', which is not registered at all"
        );
        assert!(
            !reason.trim().is_empty(),
            "'{ext}' is excluded with no reason"
        );
    }
}
