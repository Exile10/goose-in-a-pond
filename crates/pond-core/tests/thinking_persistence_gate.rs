//! PAI-5 P6: the setting is the gate, and the gate is tested with its input.
//!
//! `thinking_is_never_replayed.rs` proves the READ has one caller. This proves
//! the WRITE obeys the user, which is the half a source scan cannot see.
//!
//! The failure this is shaped against is on this programme's record. PAI-5 P1
//! guarded `is_voice` as an identifier and never its composition; the token was
//! present, every test passed, and the feature leaked reasoning on every desktop
//! voice turn because the other input was hardcoded. So the assertions here are
//! about MEANING: the same call sequence, run twice with the setting on and off,
//! must produce different storage. Nothing here asserts that a symbol exists.
//!
//! Two specific mutations this must catch, both of which compile:
//!
//! * `with_thinking` ignoring its argument (`self.persist_thinking = true`).
//!   `writes_nothing_when_the_user_did_not_ask` fails.
//! * `record_thinking` dropping its gate, so the buffer fills and the drain
//!   writes it. Same test fails, because the drain is what reaches storage.
//!
//! And one that does not compile away: the blocks must be keyed to the
//! ASSISTANT row, whose id `ChatService` mints internally. A handler cannot
//! know that id, which is why the write lives here at all -- so
//! `blocks_are_keyed_to_the_assistant_row_chatservice_minted` reads the id back
//! out of what storage was handed rather than assuming one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use pond_core::models::domain::message::Role;
use pond_core::models::ports::agent::Agent;
use pond_core::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::shared::services::chat::ChatService;
use pond_core::user_data::domain::session::{Session, SessionMessage};
use pond_core::user_data::ports::session_storage::{SessionStorage, SessionStorageError};

const BLOCK_ONE: &str = "The user said 'it' -- probably the thermostat.";
const BLOCK_TWO: &str = "No, they meant the porch light. Answer about that.";

/// Records both halves so a test can ask what was written and against what key.
#[derive(Default)]
struct RecordingStorage {
    msgs: Mutex<Vec<SessionMessage>>,
    thinking: Mutex<Vec<(String, String, Vec<String>)>>,
}

impl RecordingStorage {
    fn messages(&self) -> Vec<SessionMessage> {
        self.msgs.lock().expect("poisoned").clone()
    }
    fn thinking(&self) -> Vec<(String, String, Vec<String>)> {
        self.thinking.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl SessionStorage for RecordingStorage {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError> {
        Ok(Session::new(session_id))
    }
    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        Ok(Session::new(session_id.to_string()))
    }
    async fn add_message(
        &self,
        _session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        self.msgs.lock().expect("poisoned").push(message.clone());
        Ok(message)
    }
    async fn get_messages(
        &self,
        _session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.messages())
    }
    async fn update_title(
        &self,
        _session_id: &str,
        _title: String,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }
    async fn delete_session(&self, _session_id: &str) -> Result<(), SessionStorageError> {
        Ok(())
    }
    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
        Ok(Vec::new())
    }
    async fn get_messages_paginated(
        &self,
        _session_id: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.messages())
    }
    async fn get_recent_messages(
        &self,
        _session_id: &str,
        _limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        Ok(self.messages())
    }
    async fn add_thinking(
        &self,
        session_id: &str,
        message_id: &str,
        blocks: &[String],
    ) -> Result<(), SessionStorageError> {
        self.thinking.lock().expect("poisoned").push((
            session_id.to_string(),
            message_id.to_string(),
            blocks.to_vec(),
        ));
        Ok(())
    }
    async fn get_thinking_for_session(
        &self,
        _session_id: &str,
    ) -> Result<HashMap<String, Vec<String>>, SessionStorageError> {
        Ok(HashMap::new())
    }
}

/// Never actually streamed here -- `persist_assistant_turn` is called directly,
/// exactly as both SSE handlers call it after their stream drains.
struct InertAgent;

#[async_trait]
impl Agent for InertAgent {
    async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
        unimplemented!("this guard drives persistence, not generation")
    }
    async fn chat_stream(
        &self,
        _request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        unimplemented!("this guard drives persistence, not generation")
    }
}

/// One turn, driven through the same two calls the SSE handlers make: offer the
/// thinking frames as they arrive, then persist the assistant turn.
async fn run_turn(enabled: bool) -> Arc<RecordingStorage> {
    let storage = Arc::new(RecordingStorage::default());
    let service = ChatService::new(
        Arc::new(InertAgent),
        "gate-session".to_string(),
        storage.clone(),
    )
    .with_thinking(enabled);

    service.persist_user_message("turn it on").await.unwrap();
    service.record_thinking(BLOCK_ONE);
    service.record_thinking(BLOCK_TWO);
    service
        .persist_assistant_turn(Vec::new(), "Porch light is on.", None, None)
        .await
        .unwrap();

    storage
}

#[tokio::test]
async fn writes_nothing_when_the_user_did_not_ask() {
    let storage = run_turn(false).await;

    // The default. `persist_thinking` is false out of the box, so this is the
    // state every existing pond is in and the one that must be airtight.
    assert!(
        storage.thinking().is_empty(),
        "reasoning text was written to storage with `persist_thinking = false`: \
         {:?}\n  This is the whole privacy claim of P6. Check that \
         `record_thinking` still returns early on the gate AND that \
         `with_thinking` still stores its argument rather than a literal -- \
         either mutation produces exactly this failure and neither is a \
         compile error.",
        storage.thinking()
    );

    // Vacuity control: the turn itself must have happened, or "nothing was
    // written" is a statement about a turn that never ran.
    assert_eq!(
        storage.messages().len(),
        2,
        "expected the user row and the assistant row to be persisted regardless \
         of the reasoning setting; got {:?}",
        storage
            .messages()
            .iter()
            .map(|m| m.message.role.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn blocks_are_keyed_to_the_assistant_row_chatservice_minted() {
    let storage = run_turn(true).await;
    let written = storage.thinking();

    assert_eq!(
        written.len(),
        1,
        "expected exactly one `add_thinking` call carrying the turn's passages \
         together; got {written:?}. One call per block would key them \
         correctly but make the block_index the adapter assigns meaningless."
    );
    let (session_id, message_id, blocks) = &written[0];

    assert_eq!(
        blocks.as_slice(),
        &[BLOCK_ONE.to_string(), BLOCK_TWO.to_string()],
        "the passages must arrive in emission order and unaltered"
    );
    assert_eq!(session_id, "gate-session");

    // THE POINT OF PUTTING THE WRITE INSIDE ChatService. The assistant row's id
    // is minted by `persist_assistant_turn` (`Uuid::new_v4()`); no handler can
    // know it. If this ever keyed to the SESSION id, or to the user row, the
    // history read would attach a turn's reasoning to the wrong answer -- or to
    // every answer.
    let msgs = storage.messages();
    let assistant = msgs
        .iter()
        .find(|m| m.message.role == Role::Assistant)
        .expect("an assistant row must have been persisted");
    assert_eq!(
        message_id, &assistant.id,
        "reasoning was keyed to `{message_id}` but the assistant row this turn \
         wrote is `{}`. The blocks belong to the reply they produced; keyed to \
         anything else the UI will show one turn's reasoning under another's \
         answer.",
        assistant.id
    );
    assert_ne!(
        message_id, "gate-session",
        "reasoning was keyed to the SESSION id, not a message id"
    );
}

#[tokio::test]
async fn one_turns_reasoning_never_lands_on_the_next_turns_answer() {
    let storage = Arc::new(RecordingStorage::default());
    let service = ChatService::new(
        Arc::new(InertAgent),
        "gate-session".to_string(),
        storage.clone(),
    )
    .with_thinking(true);

    service.record_thinking(BLOCK_ONE);
    service
        .persist_assistant_turn(Vec::new(), "first answer", None, None)
        .await
        .unwrap();
    // Second turn records nothing at all.
    service
        .persist_assistant_turn(Vec::new(), "second answer", None, None)
        .await
        .unwrap();

    // The terminal voice loop keeps ONE ChatService for the life of the
    // process, so an undrained buffer is not a hypothetical: it would re-attach
    // the first turn's reasoning to every answer that followed, forever.
    let written = storage.thinking();
    assert_eq!(
        written.len(),
        1,
        "the buffer was not drained by the first turn -- turn two wrote \
         reasoning it never produced: {written:?}"
    );
    assert_eq!(written[0].2, vec![BLOCK_ONE.to_string()]);
}

#[tokio::test]
async fn empty_and_whitespace_passages_are_not_stored() {
    let storage = Arc::new(RecordingStorage::default());
    let service = ChatService::new(
        Arc::new(InertAgent),
        "gate-session".to_string(),
        storage.clone(),
    )
    .with_thinking(true);

    service.record_thinking("");
    service.record_thinking("   \n  ");
    service
        .persist_assistant_turn(Vec::new(), "answer", None, None)
        .await
        .unwrap();

    // A turn that emitted no real reasoning must leave no rows, not a row
    // containing "". The history read distinguishes absent from empty, and a
    // blank block would render an empty thinking panel on a turn that did not
    // think.
    assert!(
        storage.thinking().is_empty(),
        "blank reasoning passages were persisted: {:?}",
        storage.thinking()
    );
}
