//! PAI-2 P3: does *production* route through the redactor?
//!
//! The unit tests around `RedactingEventLog` and `RedactingMemoryRepository`
//! construct the decorator by hand. They prove the decorator redacts. They say
//! nothing at all about whether `main.rs` wired it, which is where every
//! bypass in this phase actually lives -- and there were three:
//!
//! * `SqliteSecurityPolicy` is handed its own, independently constructed
//!   `SqliteEventLog`, several dozen lines above the shared `event_log`
//!   binding. Wrapping only the shared one leaves every audit row PAI-2 P1
//!   writes -- `token:<client_id>` principal labels, remote addresses --
//!   unredacted, by the one component whose entire job is the audit trail.
//! * `run_chat` and `run_agent_cmd` build their own `memory_repo` and hand it
//!   to `build_goose_backend`, which registers `giap-memory`. A CLI or voice
//!   session writes memories exactly like the server does.
//!
//! Every one of those compiles, runs, and looks correct. Nothing warns you.
//!
//! This guard reads `main.rs` as text, which is a weaker instrument than an
//! integration test and is chosen deliberately: `pond-server` is not in
//! `ci.yml`'s test list (it is covered only by `cargo check`), so a guard that
//! lives there never runs on a pull request. `pond-infra` owns `RuleRedactor`,
//! the adapter these bindings exist to install, and it *is* in that list.
//! Reading a file with `include_str!` creates no dependency edge -- it does not
//! link `pond-server`, it just refuses to compile if the path moves.
//!
//! What it cannot prove: that the wrapper is reached at runtime. That is what
//! the live run in `scripts/live-test.sh` is for. What it can prove, cheaply
//! and on every PR, is that nobody quietly rebound one of these to the raw
//! store -- which is the exact hazard PAI-2 P5 walks into next, because it
//! reads the `event_log` binding via `set_egress_sink`.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

/// The wrapper must appear on the binding's own line or in the few lines above
/// it -- `rustfmt` puts `Arc::new(` and the type on separate lines, so a
/// single-line check would miss it.
///
/// `construction` bounds the search: the window must never reach back over an
/// EARLIER construction of the same type.
///
/// It did, and that made this guard accept the thing it exists to reject. The
/// window is six lines because rustfmt splits `Arc::new(` from the type, but a
/// second bare construction three lines below a wrapped one had the wrapped
/// one's `Redacting...::new(` inside its own window, so it was reported as
/// wrapped. Demonstrated by inserting a bare `SqliteMemoryRepository::new` next
/// to the wrapped one in `run_server`: the suite stayed green while that
/// repository would have written credentials verbatim to `pond_system.db`.
fn wrapped_within(lines: &[&str], site: usize, wrapper: &str, construction: &str) -> bool {
    let floor = site.saturating_sub(6);
    let mut start = floor;
    for i in (floor..site).rev() {
        if lines[i].contains(construction) {
            start = i + 1;
            break;
        }
    }
    lines[start..=site].iter().any(|l| l.contains(wrapper))
}

fn sites(lines: &[&str], needle: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn every_memory_repository_construction_goes_through_the_redactor() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteMemoryRepository::new(");
    // Pinned, not a floor. `>= 4` let a FIFTH construction appear unnoticed,
    // which is the likeliest shape for a new bypass -- a write path added
    // beside an existing one. Its sibling below already pins its count for the
    // same reason; this one did not, and the asymmetry was the defect.
    assert_eq!(
        found.len(),
        4,
        "found {} SqliteMemoryRepository::new( sites in main.rs; there were 4 \
         (run_server, run_chat, run_agent_cmd, run_memories_cmd). A new one is \
         a new write path and needs wrapping; a missing one means this guard \
         has stopped matching and is asserting nothing.",
        found.len()
    );
    for site in found {
        assert!(
            wrapped_within(
                &lines,
                site,
                "RedactingMemoryRepository::new(",
                "SqliteMemoryRepository::new("
            ),
            "main.rs:{} constructs SqliteMemoryRepository outside \
             RedactingMemoryRepository. Chokepoint 1 is bypassed on that path: \
             whatever writes through this repo -- extraction, the giap-memory \
             MCP tool, POST /memories, consolidation -- stores credentials \
             verbatim in pond_system.db. Wrap it, or if this really is a \
             read-only handle, say so here.",
            site + 1
        );
    }
}

#[test]
fn every_event_log_write_sink_goes_through_the_redactor() {
    let lines: Vec<&str> = MAIN.lines().collect();
    let found = sites(&lines, "SqliteEventLog::new(");
    assert!(
        found.len() >= 2,
        "found {} SqliteEventLog::new( sites in main.rs; there were 2. This \
         guard has stopped matching and is asserting nothing. It was 5 until the \
         2026-09-10 group deletions took `giap-audit` and with it the three \
         `.into_dyn()` read handles `init_audit_deps` consumed.",
        found.len()
    );

    let mut write_sinks = 0;
    for site in found {
        // `.into_dyn()` is the read-handle form. Its consumer was
        // `init_audit_deps` -- the giap-audit extension's window onto the store
        // -- which went with that group on 2026-09-10. Kept as a branch because
        // redacting a read handle would scrub nothing on the way in and
        // double-scrub on the way out, whoever reads next.
        if lines[site].contains(".into_dyn()") {
            continue;
        }
        write_sinks += 1;
        assert!(
            wrapped_within(
                &lines,
                site,
                "RedactingEventLog::new(",
                "SqliteEventLog::new("
            ),
            "main.rs:{} builds a write-path SqliteEventLog outside \
             RedactingEventLog. Chokepoint 2 is bypassed: event attributes -- \
             egress URLs with their query strings, policy audit rows carrying \
             a principal label and a remote address -- reach pond_logs.db \
             unredacted.",
            site + 1
        );
    }
    assert_eq!(
        write_sinks, 2,
        "expected exactly 2 write-path event logs (the shared binding and the \
         SqliteSecurityPolicy audit sink); found {write_sinks}. A new one needs \
         its own wrapper and a line in this count."
    );
}

/// PAI-2 P5 wires the egress sink and lands after this phase. It must read the
/// binding P3 wrapped, not build its own: outbound URLs are the single richest
/// source of query-string PII in the system, and both spellings compile.
#[test]
fn the_egress_sink_reads_the_redacting_binding() {
    assert!(
        MAIN.contains("set_egress_sink(event_log.clone())"),
        "set_egress_sink no longer takes the wrapped `event_log` binding. If it \
         was rebound to a fresh SqliteEventLog, every recorded outbound call \
         bypasses the redactor -- and it compiles either way."
    );
}

/// The bug this guard had, pinned as a fixture rather than by mutating main.rs.
///
/// `wrapped_within` scanned a fixed six-line window upward, so a SECOND, bare
/// construction sitting a few lines below a wrapped one found the wrapped one's
/// `Redacting...::new(` inside its own window and was reported as wrapped. A
/// review demonstrated it by adding a bare `SqliteMemoryRepository::new` three
/// lines below the wrapped one in `run_server`: all three tests stayed green
/// while that repository would have written credentials verbatim to
/// `pond_system.db`, which is chokepoint 1's entire purpose.
///
/// Pinned here, on synthetic lines, because the alternative is editing
/// `main.rs` in place and asserting the suite goes red -- which proves it once,
/// leaves nothing behind, and cannot run in CI.
#[test]
fn the_window_never_reaches_back_over_an_earlier_construction() {
    let lines = vec![
        "let memory_repo = Arc::new(RedactingMemoryRepository::new(",
        "    SqliteMemoryRepository::new(db.system.clone()),",
        "    redactor.clone(),",
        "));",
        "let leaky = Arc::new(SqliteMemoryRepository::new(db.system.clone()));",
    ];
    let sites: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("SqliteMemoryRepository::new("))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(sites, vec![1, 4], "fixture must contain both constructions");

    assert!(
        wrapped_within(
            &lines,
            1,
            "RedactingMemoryRepository::new(",
            "SqliteMemoryRepository::new("
        ),
        "the genuinely wrapped construction must still read as wrapped -- \
         without this the guard could pass by rejecting everything"
    );
    assert!(
        !wrapped_within(
            &lines,
            4,
            "RedactingMemoryRepository::new(",
            "SqliteMemoryRepository::new("
        ),
        "a bare construction below a wrapped one must NOT inherit its wrapper"
    );
}
