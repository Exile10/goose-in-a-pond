//! Asserts `main.rs` still reaches the context ingest pipeline.
//! CI only `cargo check`s pond-server, and an unreached `pub` item never warns as dead code.

const MAIN: &str = include_str!("../../pond-server/src/main.rs");

#[test]
fn the_file_this_test_reads_is_the_one_it_thinks_it_is() {
    assert!(
        MAIN.contains("async fn run_server"),
        "MAIN is not pond-server/src/main.rs"
    );
}

#[test]
fn the_bus_subscriber_that_feeds_the_corpus_is_still_spawned() {
    assert!(
        MAIN.contains("BusIngest::new"),
        "nothing builds a `BusIngest`, so no bus event can ever become a context item and \
         `context_items` is empty on every pond again. PAI-8 P1 is off."
    );
    assert!(
        MAIN.contains(".absorb(&settings, &bus_event"),
        "the context ingest no longer absorbs bus events. A `BusIngest` that is constructed and \
         never called is exactly the shape this file's predecessor existed to catch."
    );
}

/// Sole guard on this redaction chokepoint: the type only protects a pipeline that gets built.
#[test]
fn the_ingest_pipeline_is_constructed_with_a_redactor() {
    assert!(
        MAIN.contains("IngestPipeline::new"),
        "production no longer constructs an `IngestPipeline`. PAI-2 P6b's third part -- redaction \
         before a body leaves the pond -- goes back to having no call site, which is the state \
         the ledger recorded as blocked."
    );
    assert!(
        MAIN.contains("init_context_deps"),
        "`init_context_deps` is not called, so `search_context` and `get_recent_context` refuse \
         every call -- while looking exactly like a working guard, which PAI-6 P5 records as the \
         most misleading failure shape available here."
    );
    assert!(
        MAIN.matches("init_context_deps").count() >= 2,
        "`init_context_deps` is called on only one binary path. `serve` and the voice/CLI chat \
         command wire their MCP deps independently; the path without the call panics the first \
         time a session loads giap-context."
    );
}

/// Without a speaker authority `scope_for` answers `Unresolved`, so every member-scoped tool
/// refuses while looking like a working guard.
#[test]
fn the_speaker_authority_is_installed_on_every_binary_path() {
    for entry in [
        "async fn run_server(",
        "async fn run_chat(",
        "async fn run_agent_cmd(",
    ] {
        let start = MAIN
            .find(entry)
            .unwrap_or_else(|| panic!("no `{entry}` in main.rs"));
        let body = &MAIN[start..];
        let end = body[1..].find("\nasync fn ").map_or(body.len(), |i| i + 1);
        assert!(
            body[..end].contains("init_speaker_authority("),
            "`{entry}` does not call `init_speaker_authority`, so giap-context and giap-travel \
             cannot tell who is speaking on that path and refuse or fall back on every call."
        );
    }
}

/// Once the executor is filled, the already-running scheduler can start a turn on its next tick.
#[test]
fn the_speaker_authority_is_installed_before_schedules_can_run() {
    let install = MAIN
        .find("init_speaker_authority(")
        .expect("no `init_speaker_authority` in main.rs");
    let fill = MAIN
        .find("deferred_executor\n            .init(")
        .expect("no `deferred_executor.init` in main.rs");
    assert!(
        install < fill,
        "`serve` fills the schedule executor before installing the speaker authority, so a \
         schedule's turn can run with member-scoped tools refusing every call."
    );
}
