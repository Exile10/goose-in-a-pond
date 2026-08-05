//! PAI-2 P5 guard: no source file sends an HTTP request without being classified.
//!
//! The programme's rule is "every use of `reqwest` implies a `record_egress`
//! call". Taken literally that guard fails for 8 of the 11 `reqwest` crates on
//! the day it lands, and most of what it flags is a health probe against a model
//! server on 127.0.0.1. A guard that reports loopback traffic as egress gets
//! switched off within a week, so this one forces a CLASSIFICATION, and checks
//! each of the three answers rather than trusting it:
//!
//! * `EGRESS_TRACKED` - the file reaches the shared tracker. Checked by symbol,
//!   so deleting the `record_egress`/`check_egress` call fails the build.
//! * `LOOPBACK_ONLY` - the file only ever talks to loopback. Checked by reading
//!   every URL literal in the file: a non-loopback destination that is not on
//!   the entry's `non_target_urls` list fails the build, so an exemption cannot
//!   quietly grow a third-party host.
//! * `UNGATED_SENDERS` - real egress P5 did not reach. Enumerated, each with the
//!   phase that removes it, under a cap that only ever moves down.
//!
//! The failure mode of a source-scanning test is matching nothing and reporting
//! success, and this programme has four recorded vacuous-test incidents. So the
//! scan asserts floors on what it found, asserts the three lists PARTITION the
//! senders (an unlisted sender fails; a listed non-sender fails as a stale
//! exemption, same polarity as `public_router_and_allowlist_agree`), and asserts
//! every crate that depends on `reqwest` owns at least one classified file --
//! which is the doc's crate-level rule, kept, in the only form it can hold.
//!
//! Why a runtime walk and not `include_str!` like the route guards: those parse
//! ONE known file. This has to see a file that does not exist yet, which is the
//! entire point, and `include_str!` cannot.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Below this the walk has broken, not the tree shrunk. 363 files today.
const MIN_FILES_SCANNED: usize = 300;
/// Below this the send detector has broken, not the code moved. 18 today.
const MIN_SENDERS: usize = 15;
/// P5 leaves six real-egress files unreached. This number only ever goes down.
const MAX_UNGATED: usize = 6;

/// Any of these in a file's production source means it reaches the tracker.
const TRACKER_SYMBOLS: &[&str] = &[
    "record_egress",
    "check_egress",
    "egress::begin",
    "traced_send",
    "traced_get",
];

/// Files whose outbound calls reach the shared egress tracker.
const EGRESS_TRACKED: &[&str] = &[
    "crates/pond-adapters-weather/src/lib.rs",
    "crates/pond-infra/src/fcm_push_relay.rs",
    "crates/pond-infra-scheduler/src/webhook_executor.rs",
    "crates/pond-mcp-server/src/http.rs",
    "crates/pond-server/src/schedule_executors.rs",
];

/// A file that sends, but only ever to loopback.
struct Exempt {
    file: &'static str,
    /// Why this is not egress. Read by a human, in a review.
    reason: &'static str,
    /// Non-loopback URLs the file contains that are NOT request targets --
    /// install instructions, catalogue entries other code fetches. Every entry
    /// must still be present in the file, so a stale one fails.
    non_target_urls: &'static [&'static str],
}

const LOOPBACK_ONLY: &[Exempt] = &[
    Exempt {
        file: "crates/pond-adapters-llamafile/src/lib.rs",
        reason: "talks to the llamafile server on 127.0.0.1:8080",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-adapters-ollama/src/lib.rs",
        reason: "talks to the Ollama daemon on localhost:11434",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-agent/src/ollama_provider.rs",
        reason: "quarantined PondAgent loop (Q2-05); talks to localhost:11434",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-infra/src/ollama_provider.rs",
        reason: "talks to the Ollama daemon on localhost:11434",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-server/src/llamafile_process.rs",
        reason: "health-probes the llamafile child process on 127.0.0.1",
        non_target_urls: &[],
    },
    Exempt {
        file: "crates/pond-server/src/ollama_process.rs",
        reason: "health-probes the Ollama daemon on 127.0.0.1",
        // Printed in a "how to install Ollama" error message; never fetched.
        non_target_urls: &["https://ollama.com/install.sh"],
    },
    Exempt {
        file: "crates/pond-server/src/composite_model_catalog_provider.rs",
        reason: "its only request is GET localhost:11434/api/tags",
        // Catalogue rows. The download that fetches them is model_download.rs.
        non_target_urls: &[
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/",
            "https://huggingface.co/rhasspy/piper-voices/resolve/main/",
            "https://huggingface.co/Mozilla/",
            "https://huggingface.co/{}/resolve/main/{}",
        ],
    },
];

/// Real egress that P5 did NOT gate, with the phase that fixes it.
///
/// This list is the honest scope of `network_mode = "offline"`: these calls
/// still leave the machine. It exists instead of a silent gap, and the cap
/// above is what stops it becoming a parking lot.
const UNGATED_SENDERS: &[(&str, &str)] = &[
    (
        "crates/pond-api/src/routes.rs",
        "HF model search/info, GitHub releases, Spotify, OAuth token exchange -- \
         9 sites mixed with 7 loopback ones (piper, whisper, ollama). PAI-2 P6.",
    ),
    (
        "crates/pond-server/src/main.rs",
        "OAuth refresh loop against each provider's token_url. PAI-2 P6.",
    ),
    (
        "crates/pond-server/src/model_download.rs",
        "model/voice/ONNX downloads from huggingface.co and github.com. PAI-2 P6.",
    ),
    (
        "crates/pond-hf-cache/src/lib.rs",
        "redirect-following HF blob fetches. PAI-2 P6.",
    ),
    (
        "crates/pond-adapters-goose/src/vision_encoder.rs",
        "downloads the vision encoder. Not in the fast-crate test set. PAI-2 P6.",
    ),
    (
        "crates/pond-adapters-goose/src/extension_manager.rs",
        "connectivity-probes a user-supplied MCP server URI. PAI-2 P6.",
    ),
];

// -- the scan -----------------------------------------------------------------

fn workspace_root() -> PathBuf {
    // crates/pond-core -> repo root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

/// The file with every `#[cfg(test)]` ITEM removed, and nothing else.
///
/// Test code has to come out: several of GIAP's unit tests are `--ignored`
/// live-hardware tests that call real APIs with a bare `client.get(url).send()`,
/// and scanning those would classify `knowledge.rs` and `finance.rs` as
/// unclassified senders when their production paths go through `traced_get`.
///
/// This was originally written as "everything before the first `#[cfg(test)]`",
/// which is what the convention looks like -- and mutation-testing this guard is
/// what showed the convention is not a rule. Appending a real sender BELOW a
/// trailing `mod tests` left the guard green, and 11 files in this tree already
/// carry more than one `#[cfg(test)]`, so the truncation was discarding
/// production code between them. Removing the items is the same intent without
/// the blind spot.
///
/// Line-based rather than brace-counting on purpose: a `format!("{{")` inside a
/// test would desynchronise a brace counter, whereas rustfmt guarantees the
/// closing brace of an item sits at the item's own indentation.
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
        // A single-line item (`#[cfg(test)] mod tests;`, `#[cfg(test)] const X
        // = ...;`) has no block to close; drop just the item it annotates, or
        // the search below would eat every line up to the next item's brace.
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

/// `.send()` with empty parens is the `reqwest::RequestBuilder` form; a channel
/// send always carries a message, so this does not collide with `tx.send(msg)`.
fn sends_http(prod: &str) -> bool {
    prod.contains(".send()") || prod.contains("reqwest::get(")
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "localhost"
        || h == "::1"
        || h == "[::1]"
        || h.starts_with("127.")
        || h.ends_with(".localhost")
}

/// Every `http://` / `https://` literal in `src`, as (full literal, host).
fn urls_in(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for scheme in ["https://", "http://"] {
        let mut from = 0usize;
        while let Some(i) = src[from..].find(scheme) {
            let at = from + i;
            let rest = &src[at..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '`' || c == ')')
                .unwrap_or(rest.len());
            let full = rest[..end].to_string();
            let after = &full[scheme.len()..];
            let host = after
                .split(['/', ':'])
                .next()
                .unwrap_or_default()
                .to_string();
            out.push((full, host));
            from = at + scheme.len();
        }
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
            // `tests/` holds integration tests, which are tests by definition.
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

/// (all scanned files, sender files) -- relative, forward-slashed, sorted.
fn scan() -> (Vec<String>, BTreeMap<String, String>) {
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
        files.len() >= MIN_FILES_SCANNED,
        "scanned only {} .rs files under crates/ (floor {MIN_FILES_SCANNED}) -- \
         the walk has broken, not the tree shrunk",
        files.len()
    );

    let mut senders = BTreeMap::new();
    for rel in &files {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
        let prod = production_source(&src);
        if sends_http(&prod) {
            senders.insert(rel.clone(), prod);
        }
    }

    assert!(
        senders.len() >= MIN_SENDERS,
        "found only {} HTTP-sending files (floor {MIN_SENDERS}) -- the detector \
         has broken. A guard that matches nothing reports success.",
        senders.len()
    );

    (files, senders)
}

// -- the assertions -----------------------------------------------------------

/// Every sender is in exactly one list, and every list entry is a sender.
#[test]
fn every_http_sender_is_classified_exactly_once() {
    let (_, senders) = scan();

    let mut listed: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for f in EGRESS_TRACKED {
        listed.entry(f).or_default().push("EGRESS_TRACKED");
    }
    for e in LOOPBACK_ONLY {
        listed.entry(e.file).or_default().push("LOOPBACK_ONLY");
    }
    for (f, _) in UNGATED_SENDERS {
        listed.entry(f).or_default().push("UNGATED_SENDERS");
    }

    let duplicated: Vec<String> = listed
        .iter()
        .filter(|(_, l)| l.len() > 1)
        .map(|(f, l)| format!("{f} in {l:?}"))
        .collect();
    assert!(
        duplicated.is_empty(),
        "a file must be in exactly one list:\n  {}",
        duplicated.join("\n  ")
    );

    let listed_set: BTreeSet<&str> = listed.keys().copied().collect();
    let sender_set: BTreeSet<&str> = senders.keys().map(|s| s.as_str()).collect();

    let unclassified: Vec<&&str> = sender_set.difference(&listed_set).collect();
    assert!(
        unclassified.is_empty(),
        "these files send HTTP and are in no list:\n  {:?}\n\
         Decide: does it reach the tracker (EGRESS_TRACKED), is it loopback-only \
         (LOOPBACK_ONLY, with a reason), or is it ungated egress (UNGATED_SENDERS, \
         which is capped at {MAX_UNGATED} and shrinking)?",
        unclassified
    );

    let stale: Vec<&&str> = listed_set.difference(&sender_set).collect();
    assert!(
        stale.is_empty(),
        "these listed files no longer send HTTP -- an exemption that outlives its \
         reason:\n  {:?}\nDelete the entry.",
        stale
    );
}

/// A tracked file that stops calling the tracker fails the build.
#[test]
fn egress_tracked_files_reach_the_tracker() {
    let (_, senders) = scan();
    let mut broken = Vec::new();
    for f in EGRESS_TRACKED {
        let Some(prod) = senders.get(*f) else {
            continue; // the partition test owns this case
        };
        if !TRACKER_SYMBOLS.iter().any(|s| prod.contains(s)) {
            broken.push(*f);
        }
    }
    assert!(
        broken.is_empty(),
        "these files are listed EGRESS_TRACKED but reference none of {TRACKER_SYMBOLS:?}:\n  {:?}",
        broken
    );
}

/// A loopback exemption that grows a third-party destination fails the build.
///
/// This is the half that makes the exemption list safe to have. Without it,
/// "it only talks to Ollama" is a claim in a comment.
#[test]
fn loopback_exemptions_contain_no_third_party_url() {
    let (_, senders) = scan();
    let mut offenders = Vec::new();
    let mut stale_allowances = Vec::new();

    for e in LOOPBACK_ONLY {
        let Some(prod) = senders.get(e.file) else {
            continue; // the partition test owns this case
        };
        for (full, host) in urls_in(prod) {
            if is_loopback_host(&host) {
                continue;
            }
            if e.non_target_urls.iter().any(|a| full.starts_with(a)) {
                continue;
            }
            offenders.push(format!("{} -> {full}", e.file));
        }
        for allowed in e.non_target_urls {
            if !prod.contains(allowed) {
                stale_allowances.push(format!("{} no longer contains {allowed}", e.file));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these LOOPBACK_ONLY files contain a non-loopback destination:\n  {}\n\
         Either route the call through the egress tracker and move the file to \
         EGRESS_TRACKED, or -- if the URL is genuinely never fetched here -- add \
         it to that entry's non_target_urls with a reason.",
        offenders.join("\n  ")
    );
    assert!(
        stale_allowances.is_empty(),
        "stale non_target_urls entries (the URL is gone, the exemption is not):\n  {}",
        stale_allowances.join("\n  ")
    );
}

/// The ungated list only shrinks.
#[test]
fn ungated_egress_is_capped_and_shrinking() {
    assert!(
        UNGATED_SENDERS.len() <= MAX_UNGATED,
        "UNGATED_SENDERS has {} entries, cap {MAX_UNGATED}. This list is the honest \
         scope of network_mode=\"offline\": every file on it still phones out. \
         Gate the new sender instead of raising the cap.",
        UNGATED_SENDERS.len()
    );
    for (file, why) in UNGATED_SENDERS {
        assert!(
            why.contains("PAI"),
            "{file} must name the phase that removes it, not just describe itself"
        );
    }
}

/// The doc's crate-level rule, in the only form that can hold: every crate that
/// depends on `reqwest` owns at least one classified file.
///
/// This is what catches a crate that starts sending through a form the file-level
/// detector does not know -- `Client::execute`, `reqwest::blocking`, a wrapper.
#[test]
fn every_reqwest_crate_owns_a_classified_sender() {
    let root = workspace_root();
    let (_, senders) = scan();

    let sender_crates: BTreeSet<String> = senders
        .keys()
        .filter_map(|p| p.split('/').nth(1).map(str::to_string))
        .collect();

    let mut reqwest_crates = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ readable") {
        let entry = entry.expect("readable dir entry");
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        // A dependency line, not the prose in pond-adapters-whisper's manifest
        // recording that the dep was REMOVED.
        let declares = text.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("reqwest") && l.contains('=')
        });
        if declares {
            reqwest_crates.insert(entry.file_name().to_string_lossy().to_string());
        }
    }

    assert!(
        reqwest_crates.len() >= 10,
        "found only {} crates depending on reqwest -- the manifest scan has broken",
        reqwest_crates.len()
    );

    let unaccounted: Vec<&String> = reqwest_crates.difference(&sender_crates).collect();
    assert!(
        unaccounted.is_empty(),
        "these crates depend on reqwest but own no file this guard sees sending:\n  {:?}\n\
         Either the crate no longer needs reqwest (drop the dep), or it sends \
         through a form the detector misses (Client::execute, reqwest::blocking, \
         a wrapper) -- teach `sends_http` about it.",
        unaccounted
    );
}
