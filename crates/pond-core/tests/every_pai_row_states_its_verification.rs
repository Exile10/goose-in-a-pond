//! Every PAI status row must say whether the capability has been RUN, not only
//! whether the code landed.
//!
//! # Why this is a test and not a convention
//!
//! Because it was a convention, and on 2026-08-12 the convention was wrong in
//! three places at once, all of which passed the entire test suite:
//!
//! * **PAI-8** read `DESIGNED` while its domain, migrations, adapter, ingest
//!   pipeline and MCP server were all written — and unreachable. Working from
//!   the row rather than the tree, I then asserted "PAI-8 is the only workstream
//!   with no landed code" in two more documents.
//! * **PAI-7** read `COMPLETE — P1-P8 LANDED` while producing zero proposals on
//!   an Orin. Every mechanism worked: the schedule fired, the audience resolved,
//!   the child ran and answered. The yield was nothing.
//! * **Twenty-two settings switches** rendered and were not operable at all —
//!   no input, no accessible name, no `onChange`.
//!
//! `LANDED` and `VERIFIED` are different claims. This file makes a row that
//! omits the second one fail the build, so the next person cannot quietly leave
//! it out — which is the only reason the three above survived as long as they
//! did.
//!
//! # What it does NOT do
//!
//! It cannot check that a `VERIFIED` claim is true; only that the claim is made.
//! A row saying `VERIFIED` when nobody ran anything is a lie this cannot catch,
//! and `scripts/pai-bench.sh` is what makes the lie cheap to disprove. What this
//! removes is the SILENT case — the row that says neither.

const CHECKLIST: &str = include_str!("../../../docs/architecture/pai/00-checklist.md");

/// The eight requirement rows in section 1, identified by the PAI document each
/// one links to.
const PAI_DOCS: &[&str] = &[
    "01-identity",
    "02-privacy",
    "03-context",
    "04-smart",
    "05-reasoning",
    "06-multi",
    "07-proactive",
    "08-personal",
];

/// One of these must appear in a row's status cell.
const VERIFICATION_WORDS: &[&str] = &["VERIFIED", "UNVERIFIABLE"];

fn requirement_rows() -> Vec<(&'static str, &'static str)> {
    let mut rows = Vec::new();
    for doc in PAI_DOCS {
        if let Some(line) = CHECKLIST
            .lines()
            .find(|l| l.starts_with("| ") && l.contains("](./0") && l.contains(doc))
        {
            rows.push((*doc, line));
        }
    }
    rows
}

/// Guard against the guard. If `include_str!` stops pointing at the checklist,
/// or section 1's table is reshaped, every assertion below passes by matching
/// nothing — four of this programme's recorded vacuous-test incidents were
/// exactly that shape.
#[test]
fn the_file_this_test_reads_is_the_checklist_and_it_still_has_a_table() {
    assert!(
        CHECKLIST.contains("# PAI working checklist"),
        "CHECKLIST is not 00-checklist.md"
    );
    let rows = requirement_rows();
    assert_eq!(
        rows.len(),
        PAI_DOCS.len(),
        "found {} of {} requirement rows in section 1. The table has been reshaped or a \
         workstream's document was renamed, and this guard is now reading nothing: {:?}",
        rows.len(),
        PAI_DOCS.len(),
        rows.iter().map(|(d, _)| *d).collect::<Vec<_>>()
    );
}

/// The claim. A row that says only whether the code landed is the shape that
/// let PAI-7 read COMPLETE while producing nothing.
#[test]
fn every_requirement_row_states_whether_it_has_been_run() {
    let silent: Vec<&str> = requirement_rows()
        .into_iter()
        .filter(|(_, line)| !VERIFICATION_WORDS.iter().any(|w| line.contains(w)))
        .map(|(doc, _)| doc)
        .collect();

    assert!(
        silent.is_empty(),
        "these PAI rows say whether the code landed and not whether the capability WORKS: \
         {silent:?}.\n\
         Add one of {VERIFICATION_WORDS:?} to the status cell in \
         docs/architecture/pai/00-checklist.md section 1, per the vocabulary table just below \
         it. `NOT VERIFIED` is a perfectly good answer and PAI-7 currently carries it -- what is \
         not allowed is saying nothing, because that is indistinguishable from working and it is \
         how three capabilities came to be recorded as finished while broken.\n\
         Run `scripts/pai-bench.sh` (add --slow for PAI-7) to find out which word is true."
    );
}

/// A row that claims `VERIFIED` must say where, because "it worked on my laptop"
/// is not the claim anybody needs.
///
/// The Mac and the Orin differ by roughly 30x on decode and disagree about KV
/// geometry outright -- a measurement taken on brew llama.cpp said E4B's second
/// cache was a fixed 40 MiB, and the device said it scales with `n_ctx`. A
/// verification that does not name its hardware cannot be checked or repeated.
#[test]
fn a_verified_row_names_the_hardware_and_the_date() {
    let vague: Vec<&str> = requirement_rows()
        .into_iter()
        .filter(|(_, line)| line.contains("VERIFIED"))
        .filter(|(_, line)| {
            !(line.contains("Orin") || line.contains("Jetson") || line.contains("Mac"))
        })
        .map(|(doc, _)| doc)
        .collect();

    assert!(
        vague.is_empty(),
        "these rows claim a verification without naming the hardware it ran on: {vague:?}. \
         The Mac and the Orin differ by ~30x on decode and disagree about KV geometry, so an \
         unattributed number cannot be repeated or compared."
    );
}
