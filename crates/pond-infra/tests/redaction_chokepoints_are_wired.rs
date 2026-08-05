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
fn wrapped_within(lines: &[&str], site: usize, wrapper: &str) -> bool {
    let start = site.saturating_sub(6);
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
    assert!(
        found.len() >= 4,
        "found {} SqliteMemoryRepository::new( sites in main.rs; there were 4 \
         (run_server, run_chat, run_agent_cmd, run_memories_cmd). Either a \
         write path was deleted or this guard has stopped matching and is \
         asserting nothing.",
        found.len()
    );
    for site in found {
        assert!(
            wrapped_within(&lines, site, "RedactingMemoryRepository::new("),
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
        found.len() >= 5,
        "found {} SqliteEventLog::new( sites in main.rs; there were 5. This \
         guard has stopped matching and is asserting nothing.",
        found.len()
    );

    let mut write_sinks = 0;
    for site in found {
        // `.into_dyn()` is the read-handle form, and the only consumer is
        // `init_audit_deps` -- the giap-audit extension's window onto the
        // store. Redacting a read handle would scrub nothing on the way in and
        // would double-scrub on the way out.
        if lines[site].contains(".into_dyn()") {
            continue;
        }
        write_sinks += 1;
        assert!(
            wrapped_within(&lines, site, "RedactingEventLog::new("),
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
