//! The two chat stream handlers agree about persistence, extraction and scope.
//!
//! PAI-5 P7. `/chat/stream` and `/agent/chat/stream` both run the live
//! `GooseAdapter`, both write to `session_messages`, and both are supposed to
//! hand the turn to `ChatService`. They drifted: `agent_chat_stream` persisted
//! its turns and never extracted memory from them, so an entire conversation
//! held on that route contributed nothing, and nothing in the suite objected.
//!
//! # Half of this file reads source, and half of it drives the route
//!
//! The ORDERING facts below cannot be observed from outside. A drained SSE body
//! shows the frames a turn produced; it cannot show that the `ChatService` was
//! built after the scope was resolved, or that the `done` frame was yielded
//! after persistence rather than before it. Those are the two defects P7 was one
//! line away from shipping, and for them a source guard is the honest
//! instrument -- the same judgement as `pond-adapters-goose`'s
//! `the_turn_authority_is_built_from_the_published_allow_set`.
//!
//! **What a source guard is NOT an excuse for is the behaviour itself.**
//! `chat_integration_test.rs` shows that this crate's world stands up in
//! process: a `MockAgent`, a tempdir SQLite database and `build_router` are the
//! whole cost. So the claim that `/agent/chat/stream` still *works* -- that it
//! answers, that it terminates the stream it opened, that the turn reaches
//! `session_messages` -- is asserted by driving the route, at the bottom of this
//! file. It was not, and deleting that route's entire `done` frame left every
//! suite in the workspace green while a browser on it waited forever.
//!
//! **But a source guard that asserts PRESENCE is nearly worthless**, and this
//! programme has the scar to prove it: the child drain loop's tripwire checked
//! that five strings appeared in a function, and two production-shaped mutations
//! restored the original defect in full with every test still green, because
//! neither mutation moved any of the five strings. So these assertions are about
//! ORDER and ARGUMENTS, which is where the defect actually lives.
//!
//! # The ordering is the safety property, not a tidiness preference
//!
//! `ChatService`'s default scope is `ProfileScope::Household`, and the scope is
//! what memory extraction is attributed to. `agent_chat_stream` resolves the
//! turn's real scope *after* the session row exists, which it must. If the
//! `ChatService` is built before that resolution, it keeps the household
//! default -- and the moment extraction is switched on, which is precisely what
//! P7 does, a Guest's turn or one member's turn is written into the whole
//! household's memory. That default was harmless only while extraction was off.
//!
//! A widening default reached by ordering is still a widening default, and it is
//! invisible to any test that only checks the calls are present.
//!
//! # What the unification half added
//!
//! Both handlers now fold engine events through one `TurnAccumulator::absorb`
//! rather than each carrying its own exhaustive match. That is guarded here in
//! two directions, and it needs both. As an ABSENCE -- no handler destructures
//! an engine event itself -- because the defect it prevents is a new variant
//! written into one copy and not the other. And as a PRESENCE, that each
//! handler still calls the translator, because an absence assertion is equally
//! satisfied by a route that stopped folding altogether, which is the larger
//! regression of the two.
//!
//! The frames that come out of the translator are guarded behaviourally, in
//! `routes.rs`'s own test module, where real `AgentStreamEvent` values go in and
//! the JSON a browser receives comes out. Neither test is worth much without the
//! other: this one proves there is a single answer, that one proves the answer
//! is right.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::shared::mocks::mock_agent::MockAgent;
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_sensor::{MockCameraStorage, MockSensorStorage};
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use pond_infra::mock_handshake::MockHandshake;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use reqwest::Client as ReqwestClient;
use serde_json::{json, Value};
use tower::ServiceExt;

const ROUTES: &str = include_str!("../src/routes.rs");

/// The two functions that actually hold the streaming bodies.
///
/// **They are not symmetrical, and that asymmetry is part of what P7 is about.**
/// `chat_stream` is a thin wrapper that takes an SSE permit and delegates to
/// `chat_stream_inner`; `agent_chat_stream` carries its body inline with no
/// inner. Pointing this guard at `chat_stream` finds thirty lines of preamble and
/// none of the persistence, which is exactly the false pass the vacuity control
/// below exists to catch -- it caught it while this file was being written.
const CHAT: &str = "chat_stream_inner";
const AGENT: &str = "agent_chat_stream";

/// The one function that turns an engine event into an SSE frame.
///
/// It is a method on `TurnAccumulator`, so `handler_body` is the wrong slicer
/// for it -- see [`method_body`], which exists because the wrong slicer let a
/// mutation of this very constant pass.
const TRANSLATOR: &str = "absorb";

/// `routes.rs` with its test module removed.
///
/// Every count below has to be over production code. The unit tests for the
/// translator construct `AgentStreamEvent` values by the dozen, and a guard that
/// counted those would report a second match on the day somebody wrote a test
/// for the first one.
fn production() -> &'static str {
    let code = ROUTES
        .split_once("#[cfg(test)]")
        .map(|(before, _)| before)
        .unwrap_or_else(|| panic!("routes.rs has no test module -- did the file move?"));
    // Vacuity control for the split itself. A `#[cfg(test)]` added ABOVE the
    // handlers would shrink this to a preamble, and every `assert!(!contains)`
    // below would pass for the wrong reason.
    for needle in [
        "fn chat_stream_inner(",
        "async fn agent_chat_stream(",
        "fn absorb(",
    ] {
        assert!(
            code.contains(needle),
            "the production slice of routes.rs no longer contains `{needle}` -- \
             a `#[cfg(test)]` item now sits above it, so this file is measuring \
             a preamble and its absence assertions mean nothing"
        );
    }
    code
}

/// The body of one function, from its signature to the start of the next
/// top-level item.
///
/// Panics rather than returning an empty string on a miss. A slicer that
/// silently finds nothing turns every assertion below into a vacuous pass, which
/// is the failure shape this file exists to avoid.
fn handler_body(name: &str) -> &'static str {
    let src = production();
    let (sig, start) = [format!("async fn {name}("), format!("fn {name}(")]
        .into_iter()
        .find_map(|sig| src.find(&sig).map(|at| (sig, at)))
        .unwrap_or_else(|| panic!("{name} is gone from routes.rs -- this guard needs rewriting"));
    let rest = &src[start + sig.len()..];
    // The next top-level `async fn` / `fn` at column zero ends the body.
    let end = rest
        .find("\nasync fn ")
        .into_iter()
        .chain(rest.find("\nfn "))
        .chain(rest.find("\npub async fn "))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The body of one INDENTED method, from its signature to its own closing
/// brace.
///
/// `handler_body` cannot do this job, and finding out why is the reason this
/// function exists. Its region ends at the next item at column zero, and a
/// method sits inside an `impl` -- so pointing it at `tool_result_frame`, the
/// top-level helper immediately above `TurnAccumulator`, returned a region that
/// swallowed the whole `impl` and therefore the translator itself. The mutation
/// "point the guard at the wrong function" passed with five green tests.
///
/// Ending at `\n    }` works because rustfmt puts an item's closing brace at its
/// own indentation, which `egress_offline_routes.rs` leans on for the same
/// reason. Applied to a top-level `fn` it stops at the first four-space brace
/// INSIDE it, which is why the caller's size floor now catches that mutation
/// twice over.
fn method_body(name: &str) -> &'static str {
    let src = production();
    let sig = format!("fn {name}(");
    let start = src
        .find(&sig)
        .unwrap_or_else(|| panic!("{name} is gone from routes.rs -- this guard needs rewriting"));
    let rest = &src[start + sig.len()..];
    let end = rest.find("\n    }").unwrap_or(rest.len());
    &rest[..end]
}

/// Line comments removed, so that a count below is a count of CODE.
///
/// Rule one of this programme's vacuous-test list is a guard satisfied by
/// comment prose. Without this, ten `// AgentStreamEvent::Whatever` lines in the
/// translator would clear its vacuity floor with no match present at all, and a
/// single one in a handler would fail this file for a comment.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(at) => &l[..at],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Does `body` use `word` as a whole identifier, rather than as a fragment of a
/// longer one?
///
/// `str::contains("Status")` cannot tell `AgentStreamEvent::Status` from
/// `StatusCode::OK`, and a guard that forbids the enum's variant names would
/// then fail on any handler that returns an HTTP status. The two live variant
/// names most at risk of that are checked by the vacuity control below.
fn mentions_identifier(body: &str, word: &str) -> bool {
    let is_ident = |c: Option<char>| matches!(c, Some(c) if c.is_alphanumeric() || c == '_');
    body.match_indices(word).any(|(at, _)| {
        !is_ident(body[..at].chars().next_back())
            && !is_ident(body[at + word.len()..].chars().next())
    })
}

/// Vacuity control for [`mentions_identifier`].
///
/// It is the search this file's strongest assertion rests on, and a search that
/// answers `true` to everything makes that assertion fail for the wrong reason
/// while a search that answers `false` to everything makes it vacuous. Both
/// directions are pinned, against the exact strings that would produce each.
#[test]
fn the_identifier_search_can_tell_a_name_from_a_longer_one() {
    assert!(
        mentions_identifier("match ev { Ev::Status { content } => {}", "Status"),
        "the search misses a variant name that IS there, so every assertion \
         built on it is vacuous"
    );
    assert!(
        !mentions_identifier("return StatusCode::BAD_REQUEST.into_response();", "Status"),
        "the search reports a variant name inside `StatusCode`, so it would fail \
         this file for a handler doing ordinary HTTP"
    );
    assert!(
        !mentions_identifier("let text_content = body.text;", "Text"),
        "the search is not case-sensitive or not boundary-aware"
    );
}

fn position(body: &str, needle: &str, handler: &str) -> usize {
    body.find(needle).unwrap_or_else(|| {
        panic!("{handler} no longer contains `{needle}` -- PAI-5 P7 parity has regressed")
    })
}

/// Both handlers hand the turn to `ChatService` for persistence AND extraction.
///
/// `persist_assistant_turn_with_extraction` is one call that owns both concerns
/// on purpose: there is no separate extraction call for a later refactor to drop.
/// A handler that calls the plain `persist_assistant_turn` has silently opted out
/// of memory, which is what `/agent/chat/stream` did.
#[test]
fn both_stream_handlers_extract_memory_from_the_turn() {
    for handler in [CHAT, AGENT] {
        let body = handler_body(handler);
        assert!(
            body.contains("persist_assistant_turn_with_extraction"),
            "{handler} persists its turn without extracting from it, so a conversation held \
             there contributes nothing to memory"
        );
        assert!(
            body.contains("with_memory_extraction"),
            "{handler} never wires the extractor onto its ChatService, so \
             persist_assistant_turn_with_extraction has nothing to spawn"
        );
    }
}

/// Vacuity control for the test above.
///
/// If `handler_body` ever slices wrongly -- picks the whole file, or an empty
/// range -- the assertions above pass or fail for reasons that have nothing to do
/// with parity. This pins that the two bodies are found, are different, and are
/// each a plausible size for a handler rather than the whole 13k-line file.
#[test]
fn the_two_handler_bodies_are_really_two_different_handlers() {
    let chat = handler_body(CHAT);
    let agent = handler_body(AGENT);
    assert_ne!(
        chat, agent,
        "the slicer returned the same text for both handlers"
    );
    for (name, body) in [(CHAT, chat), (AGENT, agent)] {
        assert!(
            body.len() > 2_000,
            "{name}'s body came out at {} bytes, which is too small to be the handler -- the \
             slicer is broken and every assertion in this file is vacuous",
            body.len()
        );
        assert!(
            body.len() < ROUTES.len() / 2,
            "{name}'s body came out at {} bytes of a {}-byte file -- the slicer ran past the \
             end of the handler",
            body.len(),
            ROUTES.len()
        );
    }
    // `chat_stream`'s name is a prefix of nothing else, but `agent_chat_stream`
    // contains it as a substring; prove the slicer did not match the wrong one.
    assert!(
        agent.contains("model_role: \"task\""),
        "the body claimed to be agent_chat_stream does not look like it"
    );
}

/// The scope must be resolved BEFORE the `ChatService` is built, or extraction is
/// attributed to `ProfileScope::Household` whoever was actually speaking.
///
/// This is the assertion that would have caught the defect P7 was one line away
/// from shipping. Presence of both calls is not enough and never was: before this
/// change `agent_chat_stream` contained `resolve_turn_scope` and a `ChatService`
/// and was still wrong, because the resolution came thirty lines too late.
#[test]
fn agent_chat_stream_knows_who_is_speaking_before_it_builds_the_service() {
    let body = handler_body(AGENT);
    let resolved = position(body, "let turn_scope = resolve_turn_scope(", AGENT);
    let built = position(body, "ChatService::new(", AGENT);
    let scoped = position(body, ".with_profile_scope(turn_scope", AGENT);

    assert!(
        resolved < built,
        "the turn's scope is resolved AFTER the ChatService is built, so the service keeps its \
         Household default and every memory extracted from this route is attributed to the whole \
         household -- whoever was speaking"
    );
    assert!(
        scoped > built,
        "with_profile_scope is not applied to the constructed service"
    );
}

/// The match on `AgentStreamEvent` lives in EXACTLY ONE place.
///
/// This is the property the unification half of P7 bought, and the only thing
/// that keeps it: two exhaustive matches over an enum that is still growing is a
/// standing promise to write every new variant twice. PAI-6 P6 added one --
/// `SubagentProgress` -- and wrote its arm ONCE, which is what P7 bought and is
/// the reason this guard exists rather than a note in a document. When
/// the second copy exists, the cheap thing to do is write the arm into whichever
/// handler you were looking at -- which is how these two came to disagree about
/// tool timing, and how `/agent/chat/stream` came to stream reasoning it never
/// offered to its `ChatService`.
///
/// The "is there a second one" half is deliberately phrased as "none outside"
/// rather than "ten inside": a guard naming today's variants cannot see the one
/// added tomorrow, which is the assertion-window failure this programme keeps
/// re-learning.
///
/// It is not phrased as a search for the string `AgentStreamEvent::` ALONE,
/// which is what it used to be and which an import defeats:
/// `use ...::AgentStreamEvent as Ev;` and a `match &event { Ev::… }` fold
/// reintroduces the whole defect with this guard green. That mutation was run.
/// So the absence is checked three ways -- the qualified name, the `match` on
/// the event binding, and the variant names themselves, which no aliasing can
/// avoid spelling -- and, more importantly, the POSITIVE half is checked too: a
/// handler that stopped folding through the translator altogether was equally
/// invisible to a guard that only asserts an absence.
#[test]
fn the_engine_event_match_lives_in_exactly_one_place() {
    // The variants whose names cannot plausibly appear in a handler for an
    // unrelated reason. `Status`, `Text`, `Done` and `Error` are left out on
    // purpose: `anyhow::Error` in a handler is not a second match, and a guard
    // that says it is gets deleted rather than obeyed. Any second fold has to
    // name the six below -- exhaustively if it is a copy, and at least one of
    // them if it is a partial `match … _ => {}` bolted on beside the real one.
    const DISTINCTIVE_VARIANTS: [&str; 7] = [
        "Thinking",
        "ToolCall",
        "ToolResult",
        "ReviewStatus",
        "ReviewRevision",
        "TurnLimitReached",
        // PAI-6 P6. The newest variant is the one most likely to be handled
        // twice, because the arm is written while its producer is being built
        // and a handler is the obvious place to put it.
        "SubagentProgress",
    ];

    let translator = strip_line_comments(method_body(TRANSLATOR));
    let inside = translator.matches("AgentStreamEvent::").count();

    // Vacuity control. If the slicer or the name rots, `inside` is 0, every
    // "not in the handler" assertion below still passes, and this file starts
    // reporting that a match nobody can find has not been duplicated.
    assert!(
        inside >= 10,
        "`{TRANSLATOR}` matches only {inside} `AgentStreamEvent::` variants. The \
         enum has at least ten, so either this guard is pointing at the wrong \
         function or the translator has stopped being the translator -- and \
         every other assertion in this test is now vacuous"
    );
    assert!(
        translator.len() > 1_000 && translator.len() < ROUTES.len() / 10,
        "`{TRANSLATOR}` sliced to {} bytes of a {}-byte file, which is not the \
         shape of one method. Too small and the slicer stopped early; too large \
         and it ran past the end and is quoting somebody else's code as proof",
        translator.len(),
        ROUTES.len()
    );
    // Positive control for the identifier search, against real code rather than
    // a fixture: the six names below are known to be in the translator, so if
    // `mentions_identifier` were broken in the "finds nothing" direction this
    // fails here instead of quietly clearing every handler below.
    for variant in DISTINCTIVE_VARIANTS {
        assert!(
            mentions_identifier(&translator, variant),
            "`{TRANSLATOR}` no longer handles `{variant}`. Either the enum was \
             renamed -- in which case this list needs the new spelling before it \
             can detect a second copy again -- or the translator has stopped \
             being exhaustive"
        );
    }

    for handler in [CHAT, AGENT] {
        let body = strip_line_comments(handler_body(handler));

        // The positive half. Absence assertions cannot see a handler that
        // stopped folding at all, and a route that never reaches the translator
        // is a worse regression than a second copy of it.
        assert!(
            body.contains("turn.absorb(event)"),
            "{handler} no longer folds the engine's events through \
             `{TRANSLATOR}`. Every frame that route emits now comes from \
             somewhere this file cannot see, and the single-answer property P7 \
             bought is gone whether or not a second match is visible below"
        );

        assert!(
            !body.contains("AgentStreamEvent::"),
            "{handler} matches engine events itself instead of folding them \
             through `{TRANSLATOR}`. Two copies of the match is what PAI-5 P7 \
             removed: the next variant gets written into one of them, the other \
             silently drops it, and the two routes answer the same engine \
             differently"
        );
        // `match event_result {` is the handler's own Result match and is fine;
        // neither spelling below is a substring of it.
        for spelling in ["match &event", "match event {"] {
            assert!(
                !body.contains(spelling),
                "{handler} contains `{spelling}` -- it is matching the engine \
                 event itself. Importing the enum under another name hides it \
                 from the assertion above but not from this one"
            );
        }
        for variant in DISTINCTIVE_VARIANTS {
            assert!(
                !mentions_identifier(&body, variant),
                "{handler} names the engine variant `{variant}`, so it is \
                 destructuring engine events somewhere of its own. A second \
                 fold cannot avoid spelling the variants, whatever the enum \
                 itself is imported as"
            );
        }
    }

    // And nowhere else in the file either -- a third stream path would drift
    // from both.
    let everywhere = strip_line_comments(production())
        .matches("AgentStreamEvent::")
        .count();
    assert_eq!(
        everywhere, inside,
        "routes.rs matches `AgentStreamEvent::` in {everywhere} places but only \
         {inside} of them are in `{TRANSLATOR}`. Every frame shape belongs to \
         one function so that a new variant is written once"
    );
}

/// Both handlers scope the service from the turn's RESOLVED identity, not from a
/// default and not from a literal.
///
/// The input half of the gate. `with_profile_scope` taking the wrong argument
/// fails nothing else in the suite: the call is present, the handler compiles, and
/// the misattribution is silent.
#[test]
fn neither_handler_scopes_its_service_from_a_literal() {
    for handler in [CHAT, AGENT] {
        let body = handler_body(handler);
        assert!(
            body.contains(".with_profile_scope(turn_scope"),
            "{handler} does not scope its ChatService from the turn's resolved scope"
        );
        assert!(
            !body.contains(".with_profile_scope(ProfileScope::"),
            "{handler} scopes its ChatService from a literal, which cannot be the speaker"
        );
    }
}

/// `/chat/stream` sends its `done` AFTER it has persisted the turn.
///
/// This is why the translator hands back `TurnComplete` rather than emitting a
/// frame of its own: if `done` came out of the fold, this route would close the
/// stream before `persist_assistant_turn_with_extraction` had run, and a client
/// that reloads the session when the stream ends -- which the desktop does --
/// would read back a conversation with the answer it just watched arrive
/// missing from it.
///
/// It is the one half of the `done` contract that reads source, because it is
/// the one half a drained SSE body cannot show: collecting the frames says
/// nothing about what else had already happened when each was yielded. The other
/// half -- that `/agent/chat/stream` sends a `done` at all -- is driven for real
/// at the bottom of this file.
#[test]
fn chat_stream_persists_the_turn_before_it_closes_the_stream() {
    let body = handler_body(CHAT);
    let persisted = position(body, "persist_assistant_turn_with_extraction", CHAT);
    let done = position(body, "\"done\": true", CHAT);

    assert!(
        done > persisted,
        "{CHAT} yields its `done` frame at byte {done} of its body but only persists the \
         turn at byte {persisted}. A client that reloads the session when the stream closes \
         reads a conversation missing the answer it just watched arrive"
    );
    assert_eq!(
        body.matches("\"done\": true").count(),
        1,
        "{CHAT} builds more than one `done` frame, so the ordering asserted above is only \
         true of whichever one this guard happened to find first"
    );
}

// ── The other half: the route, driven ────────────────────────────────────────
//
// Everything above reads text. What follows runs `/agent/chat/stream` for real:
// through `build_router`, through the auth middleware, against a tempdir SQLite
// database, with the `MockAgent` standing in for `GooseAdapter`. `AppState` has
// no `Default`, so every integration test in this crate spells the fixture out
// and this one is no exception.

/// Onboarding always reports completed, so its gate never stands between the
/// request and the handler under test.
struct CompletedOnboarding;

#[async_trait::async_trait]
impl OnboardingRepository for CompletedOnboarding {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        Some(OnboardingStep::Completed)
    }
    async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reset(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn is_complete(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct StubDeviceRegistry;

#[async_trait::async_trait]
impl DeviceRegistry for StubDeviceRegistry {
    async fn register(&self, req: RegisterDeviceRequest) -> anyhow::Result<Device> {
        Ok(Device {
            id: "stub".to_string(),
            name: req.name,
            device_type: req.device_type,
            hostname: req.hostname,
            ip_address: None,
            capabilities: req.capabilities,
            registered_at: "2024-01-01 00:00:00".to_string(),
            last_seen: None,
            is_online: false,
            room: req.room,
        })
    }
    async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        Ok(vec![])
    }
    async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
        Ok(None)
    }
    async fn unregister(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn make_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let db = pond_infra::db::Database::init(tmp.path()).await.unwrap();
    let session_storage = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let mock_hs = MockHandshake::new();
    mock_hs.add_valid_token("test-token".to_string()).await;

    let state = Arc::new(AppState {
        db: Arc::new(db),
        onboarding_repo: Arc::new(CompletedOnboarding),
        handshake: Arc::new(mock_hs),
        whisper_url: "http://127.0.0.1:9000".to_string(),
        transcribe_audio: None,
        session_storage,
        http_client: ReqwestClient::new(),
        agent: Arc::new(MockAgent::new()),
        llm_provider: Arc::new(tokio::sync::RwLock::new(None)),
        llamafile_url: "http://127.0.0.1:8080".to_string(),
        tts: None,
        tts_control: None,
        settings_repo: Arc::new(MockSettingsRepository::new()),
        profile_repo: Arc::new(MockProfileRepository::new()),
        device_registry: Arc::new(StubDeviceRegistry),
        matter: None,
        memory_repo: Arc::new(MockMemoryRepository::new()),
        embedding_provider: None,
        vector_index: None,
        index_reindex: None,
        account_sync: None,
        sensor_storage: Arc::new(MockSensorStorage::new()),
        camera_storage: Arc::new(MockCameraStorage::new()),
        face_recognition: None,
        prompt_template_dir: None,
        model_repo: None,
        data_dir: None,
        skip_onboarding: true,
        scheduler: None,
        model_scheduler: None,
        mcp_memory: None,
        extension_manager: None,
        mcp_server_repo: None,
        tool_registry: None,
        marketplace: None,
        secret_repo: None,
        download_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        piper_http_port: None,
        model_catalog_provider: None,
        model_storage_dir: None,
        prompt_template_repo: None,
        prompt_extra_repo: None,
        skill_repo: None,
        recipe_repo: None,
        llamafile_manager: None,
        operational_log: None,
        event_bus: None,
        event_log: None,
        push_token_repo: None,
        notification_tx: tokio::sync::broadcast::channel(16).0,
        notification_queue: None,
        notification_sender: None,
        sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        notification_sse_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
        answer_reviewer: None,
        memory_extractor: None,
        memory_extraction_service: None,
        last_user_activity: Arc::new(tokio::sync::RwLock::new(std::time::Instant::now())),
        consolidation_cancel: Arc::new(tokio::sync::RwLock::new(None)),
        consolidation_event_tx: tokio::sync::broadcast::channel(16).0,
        consolidation_runner: None,
        inference_pool: None,
        schedule_result_tx: tokio::sync::broadcast::channel(1).0,
        telemetry: None,
        context_monitor: Arc::new(
            pond_core::models::services::context_monitor::ContextMonitor::new(),
        ),
        mcp_app_resources: std::collections::HashMap::new(),
        oauth_state: pond_api::oauth_callback::new_oauth_state(),
        oauth_outcomes: pond_api::oauth_callback::new_oauth_outcomes(),
        security_policy: None,
        tool_dispatcher: None,
        api_port: 4000,
        weather_provider: None,
        peer_directory: Arc::new(
            pond_core::mesh::mocks::mock_peer_directory::MockPeerDirectory::new(),
        ),
        credit_ledger: Arc::new(
            pond_core::mesh::mocks::mock_credit_ledger::MockCreditLedger::new(),
        ),
        usage_tally: Arc::new(pond_core::mesh::mocks::mock_usage_tally::MockUsageTally::new()),
        mesh_transport: None,
    });
    (
        build_router(state, std::path::PathBuf::from("pond-desktop/dist")),
        tmp,
    )
}

/// POST one turn to `/agent/chat/stream` and return the SSE frames it produced.
///
/// Drains the whole body, which is also what makes the handler run: persistence
/// lives inside the `async_stream` generator and only executes when polled.
async fn drive_agent_stream(app: &axum::Router, session_id: &str, message: &str) -> Vec<Value> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/agent/chat/stream")
                .header("content-type", "application/json")
                .header("Authorization", "Bearer test-token")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "session_id": session_id,
                        "message": message,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "/agent/chat/stream refused the turn, so nothing below is about the stream"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).expect("an SSE body is UTF-8");
    let frames: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| {
            serde_json::from_str(data).unwrap_or_else(|e| {
                panic!("a frame the browser has to parse is not JSON: {data} -- {e}")
            })
        })
        .collect();

    // Vacuity control. A handler that bailed out early still returns 200 with a
    // body full of frames -- one `error` frame -- and every "the turn did X"
    // assertion below would then be measuring a turn that never ran.
    assert!(
        !frames.iter().any(|f| f.get("error").is_some()),
        "the route reported an error instead of running the turn: {frames:?}"
    );
    frames
}

/// `/agent/chat/stream` ends the turn it opened.
///
/// The desktop's reader loop ends on `{"done": true}` and on nothing else. Before
/// this test existed, deleting the entire body of this route's
/// `StreamStep::TurnComplete` arm -- the arm PAI-5 P7 rewrote -- left
/// `cargo test -p pond-api` green in full, because no test in the workspace drove
/// the route at all. A refactor whose claim is "both routes now behave
/// identically" has to assert somewhere that the second route still behaves.
#[tokio::test]
async fn the_agent_route_ends_the_turn_it_opened() {
    let (app, _tmp) = make_app().await;
    let session_id = "agent-stream-terminates";

    let frames = drive_agent_stream(&app, session_id, "hello").await;

    let done: Vec<&Value> = frames.iter().filter(|f| f.get("done").is_some()).collect();
    assert_eq!(
        done.len(),
        1,
        "/agent/chat/stream must end its stream with exactly one `done` frame. None and a \
         browser reads forever; two and it closes a turn that is still arriving. Frames \
         were: {frames:?}"
    );
    assert_eq!(
        done[0]["done"],
        json!(true),
        "the terminating frame must say `done: true`, not merely carry the key: {:?}",
        done[0]
    );
    assert_eq!(
        done[0]["session_id"],
        json!(session_id),
        "the `done` frame carries the session the client reloads when the stream ends: {:?}",
        done[0]
    );

    // And the turn actually answered, so the assertion above is about a real
    // stream rather than a `done` sent over silence.
    let answer: String = frames
        .iter()
        .filter(|f| f["type"] == "text")
        .filter_map(|f| f["content"].as_str())
        .collect();
    assert_eq!(
        answer, "Echo: hello",
        "the answer the client assembles from the text frames is not the answer the engine \
         produced. Frames were: {frames:?}"
    );
}

/// The turn reaches `session_messages`, which is what `GET /sessions/{id}/messages`
/// serves and what the next turn's history is read back from.
///
/// `both_stream_handlers_extract_memory_from_the_turn` asserts this route calls
/// `persist_assistant_turn_with_extraction`; this asserts the call arrives
/// somewhere. A persistence call inside a generator nobody polls, or made against
/// a session row that was never created, is a call that reads as present and
/// stores nothing.
#[tokio::test]
async fn the_agent_route_persists_the_turn_it_streamed() {
    let (app, _tmp) = make_app().await;
    let session_id = "agent-stream-persists";

    drive_agent_stream(&app, session_id, "hello").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/sessions/{session_id}/messages"))
                .header("Authorization", "Bearer test-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    let messages = json["messages"].as_array().expect("messages array");

    assert_eq!(
        messages.len(),
        2,
        "expected the user's turn and the assistant's answer, got: {json}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "hello");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["content"], "Echo: hello",
        "the answer persisted is not the answer streamed, so the next turn's history is \
         not the conversation the user had"
    );
}
