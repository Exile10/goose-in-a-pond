//! There is one table of built-in prompt templates, and every writer reads it.
//!
//! Five call sites used to carry their own copy of `(name, content, description)`:
//! `run_setup`, the boot reseed, the chat-CLI reseed, `pond prompts reset`, and
//! `builtin_template_content`. They had already drifted — three said balanced was
//! *"…complete behaviour rules. Default for most households."* and two said
//! *"…general-purpose. Default."*.
//!
//! The drift was not cosmetic, because the disagreeing writers run in sequence.
//! `seed_system_template` upserts `description = excluded.description` for any row
//! that is not customized, so `pond prompts reset` wrote one description and the
//! next boot replaced it with the other. A user could watch the field change
//! without touching anything.
//!
//! Content had a weaker version of the same problem and a stronger defence: the
//! copies referenced the `PROMPT_*` constants, so a prompt rewrite propagated.
//! Descriptions were string literals, so nothing propagated and nothing failed.
//! **That asymmetry is the whole lesson** — a duplicated reference is a smell, a
//! duplicated literal is a bug waiting for someone to edit one of them.
//!
//! So this asserts the literals live in exactly one file. Re-add a description to
//! `main.rs` and it fails naming `main.rs`.
//!
//! WHY A SOURCE SCAN. The alternative is a type that only `prompts.rs` can build,
//! which does stop a second *table* but not a second *literal* — `upsert` takes a
//! `PromptTemplate` with a plain `String` description, and always will, because
//! user-created templates have descriptions too. The hazard is a hand-written
//! string somewhere in the workspace, and only a scan sees those.
//!
//! VACUITY CONTROLS, both of which have caught a broken guard in this repo before:
//! a walk that matches nothing reports success, so [`FLOOR_FILES`] fails if the
//! walk breaks, and each description must be found *at least* once or the needle
//! has rotted away from what the code says.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pond_core::prompts::{builtin_template_content, BUILTIN_PROMPT_TEMPLATES};

/// The one file allowed to spell a factory description.
const HOME: &str = "crates/pond-core/src/prompts.rs";

/// Below this the walk has broken, not the tree shrunk.
const FLOOR_FILES: usize = 300;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

/// The file with every `#[cfg(test)]` ITEM removed. Lifted from
/// `thinking_is_never_replayed.rs`, which lifted it from `egress_guard.rs`.
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
///
/// Load-bearing here: `prompts.rs` quotes both historical descriptions in the
/// doc comment explaining the drift, and this file's own module docs quote them
/// too. Without the strip, the guard would report a copy in the very comment
/// that documents why copies are forbidden.
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

/// (files scanned, files whose production source contains `needle`)
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

    let mut hits = BTreeSet::new();
    for rel in &files {
        let Ok(src) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        if strip_line_comments(&production_source(&src)).contains(needle) {
            hits.insert(rel.clone());
        }
    }
    (files.len(), hits)
}

/// A factory description is spelled in one file. Everywhere else reads the table.
#[test]
fn no_second_copy_of_a_factory_description() {
    assert!(
        !BUILTIN_PROMPT_TEMPLATES.is_empty(),
        "the built-in table is empty -- this guard would certify nothing"
    );

    for &(name, _, description) in BUILTIN_PROMPT_TEMPLATES {
        let (files, hits) = scan(description);
        assert!(
            files >= FLOOR_FILES,
            "walked only {files} files (floor {FLOOR_FILES}) -- the walk is broken, \
             so an empty result proves nothing"
        );
        assert!(
            hits.contains(HOME),
            "'{name}': its description is not in {HOME}. Either the table moved or \
             this guard's needle has rotted; found in {hits:?}"
        );
        assert_eq!(
            hits.len(),
            1,
            "'{name}': its description is spelled in {} files, not just {HOME}: {hits:?}\n\
             Every writer must read BUILTIN_PROMPT_TEMPLATES. A second literal is how \
             `pond prompts reset` and the boot reseed came to disagree.",
            hits.len()
        );
    }
}

/// The lookup covers the whole table — a row nothing can resolve is a row that
/// seeds at boot and 404s on reset.
#[test]
fn every_row_resolves_through_the_public_lookup() {
    for &(name, content, description) in BUILTIN_PROMPT_TEMPLATES {
        let found = builtin_template_content(name)
            .unwrap_or_else(|| panic!("'{name}' is in the table but does not resolve"));
        assert_eq!(found.0, content, "'{name}': lookup returned other content");
        assert_eq!(
            found.1, description,
            "'{name}': lookup returned another description"
        );
    }
    assert!(
        builtin_template_content("definitely-not-a-builtin").is_none(),
        "an unknown name must not resolve, or `prompts reset` would invent a template"
    );
}

/// Names are what `Settings.prompt_style` is set to and what the Prompts tab
/// shows. Losing one silently drops a style users can already be on.
#[test]
fn the_four_shipped_styles_are_all_present() {
    let names: BTreeSet<&str> = BUILTIN_PROMPT_TEMPLATES
        .iter()
        .map(|&(n, _, _)| n)
        .collect();
    for expected in ["balanced", "concise", "technical", "warm"] {
        assert!(
            names.contains(expected),
            "style '{expected}' is gone from the built-in table; installs already \
             carry it in Settings.prompt_style and would fall back to balanced"
        );
    }
}
