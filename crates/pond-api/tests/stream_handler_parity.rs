//! The two chat stream handlers agree about persistence, extraction and scope.
//!
//! PAI-5 P7. `/chat/stream` and `/agent/chat/stream` both run the live
//! `GooseAdapter`, both write to `session_messages`, and both are supposed to
//! hand the turn to `ChatService`. They drifted: `agent_chat_stream` persisted
//! its turns and never extracted memory from them, so an entire conversation
//! held on that route contributed nothing, and nothing in the suite objected.
//!
//! # Why this reads source
//!
//! The construction being guarded is inside an `async_stream::stream!` body in a
//! handler that needs a live engine, a live `AppState` and a provider. The same
//! judgement as `pond-adapters-goose`'s
//! `the_turn_authority_is_built_from_the_published_allow_set`: where the thing
//! worth asserting cannot be reached without standing up the world, a source
//! guard is the honest instrument.
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
//! rather than each carrying its own exhaustive match. That is guarded here as
//! an ABSENCE -- no handler matches `AgentStreamEvent` itself -- because the
//! defect it prevents is a new variant being written into one copy and not the
//! other. The frames that come out of the translator are guarded behaviourally,
//! in `routes.rs`'s own test module, where real `AgentStreamEvent` values go in
//! and the JSON a browser receives comes out. Neither test is worth much without
//! the other: this one proves there is a single answer, that one proves the
//! answer is right.

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
/// standing promise to write every new variant twice. PAI-6 P6 adds one. When
/// the second copy exists, the cheap thing to do is write the arm into whichever
/// handler you were looking at -- which is how these two came to disagree about
/// tool timing, and how `/agent/chat/stream` came to stream reasoning it never
/// offered to its `ChatService`.
///
/// Deliberately phrased as "none outside" rather than "ten inside": a guard
/// naming today's variants cannot see the one added tomorrow, which is the
/// assertion-window failure this programme keeps re-learning.
#[test]
fn the_engine_event_match_lives_in_exactly_one_place() {
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

    for handler in [CHAT, AGENT] {
        let body = strip_line_comments(handler_body(handler));
        assert!(
            !body.contains("AgentStreamEvent::"),
            "{handler} matches engine events itself instead of folding them \
             through `{TRANSLATOR}`. Two copies of the match is what PAI-5 P7 \
             removed: the next variant gets written into one of them, the other \
             silently drops it, and the two routes answer the same engine \
             differently"
        );
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
