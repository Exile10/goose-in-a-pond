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

/// `CLAUDE.md` states the extension count in prose, and a number in prose is the
/// thing that goes stale. It has been wrong twice in this programme — once at 14
/// when the answer was 15, and once "corrected" from 15 back to 14 by a grep
/// that could not see the const.
///
/// This ties the sentence to the code. If the wording changes, this test fails
/// asking for the claim to be re-verified rather than quietly stopping.
#[test]
fn the_extension_count_in_claude_md_is_the_real_one() {
    let path = workspace_root().join("CLAUDE.md");
    let doc = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

    const PREFIX: &str = "dispatches the ";
    const SUFFIX: &str = " `giap-*` builtin MCP extensions";
    let start = doc.find(PREFIX).unwrap_or_else(|| {
        panic!(
            "CLAUDE.md no longer contains the sentence `{PREFIX}N{SUFFIX}`. If the claim was \
             reworded, re-verify the number against giap_registration.rs and update this test to \
             match; do not delete it."
        )
    }) + PREFIX.len();
    let end = doc[start..].find(SUFFIX).unwrap_or_else(|| {
        panic!("CLAUDE.md's extension-count sentence changed shape; re-verify and re-tie it.")
    }) + start;
    let claimed: usize = doc[start..end]
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("CLAUDE.md claims `{}` extensions: {e}", &doc[start..end]));

    // Compared against the REGISTRATION count rather than the catalog's length,
    // because that is what the sentence in CLAUDE.md is a claim about — and
    // because it is what the counting recipe next to it produces. The two are
    // pinned to each other by the test above.
    let registered = registered_extensions_from_source().len();
    assert_eq!(
        claimed, registered,
        "CLAUDE.md says {claimed} builtin extensions; giap_registration.rs has {registered} \
         `register_builtin_extension(` call sites. Count the CALL SITES -- the import line has \
         no open paren, so nothing is subtracted from the grep, and two call sites pass a const \
         rather than a string literal."
    );
}
