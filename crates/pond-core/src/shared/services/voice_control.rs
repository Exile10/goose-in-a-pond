//! The desktop shell's control channel into the `--json-events` voice child: one JSON command per
//! stdin line. When no speech model loaded, the same stdin also carries typed turns.

use crate::models::ports::voice_input::VoiceInput;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use std::io::BufRead;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// A command line from the shell, e.g. `{"cmd":"interrupt"}`. Extra fields are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Stop speaking and end the turn in flight, persisting nothing. Ignored outside a turn.
    Interrupt,
    /// End the session: abandon any turn in flight, release the microphone, exit 0.
    Stop,
}

/// What one stdin line turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinLine {
    Blank,
    Command(ControlCommand),
    /// Typed input: anything that does not open a JSON object.
    Text(String),
    /// A JSON object that is not a known command. Never treated as typed input.
    Malformed(String),
}

pub fn parse_stdin_line(line: &str) -> StdinLine {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return StdinLine::Blank;
    }
    if !trimmed.starts_with('{') {
        return StdinLine::Text(trimmed.to_string());
    }
    match serde_json::from_str::<ControlCommand>(trimmed) {
        Ok(cmd) => StdinLine::Command(cmd),
        Err(e) => StdinLine::Malformed(e.to_string()),
    }
}

/// Why the session is ending. The first reason recorded wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// `{"cmd":"stop"}`.
    Requested,
    /// Stdin closed while it carried only commands.
    StdinEof,
}

impl StopReason {
    /// The NDJSON `exit` reason.
    pub fn as_exit_reason(self) -> &'static str {
        match self {
            Self::Requested => "stopped",
            Self::StdinEof => "stdin_eof",
        }
    }
}

/// Shared between the stdin reader and the voice loop. Stop is latched; interrupt is an edge.
#[derive(Clone)]
pub struct VoiceControl {
    inner: Arc<ControlInner>,
}

struct ControlInner {
    /// Bumped per interrupt; a turn waits for it to move past the value it started with.
    interrupts: watch::Sender<u64>,
    stop: watch::Sender<Option<StopReason>>,
}

impl Default for VoiceControl {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceControl {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ControlInner {
                interrupts: watch::channel(0).0,
                stop: watch::channel(None).0,
            }),
        }
    }

    pub fn apply(&self, command: ControlCommand) {
        match command {
            ControlCommand::Interrupt => self.interrupt(),
            ControlCommand::Stop => self.stop(StopReason::Requested),
        }
    }

    pub fn interrupt(&self) {
        self.inner
            .interrupts
            .send_modify(|n| *n = n.wrapping_add(1));
    }

    pub fn stop(&self, reason: StopReason) {
        self.inner.stop.send_if_modified(|current| {
            if current.is_some() {
                return false;
            }
            *current = Some(reason);
            true
        });
    }

    pub fn stop_requested(&self) -> Option<StopReason> {
        *self.inner.stop.borrow()
    }

    /// Resolves once a stop is recorded, at once if one already was.
    pub async fn stopped(&self) -> StopReason {
        let mut rx = self.inner.stop.subscribe();
        if let Ok(reason) = rx.wait_for(Option::is_some).await {
            if let Some(reason) = *reason {
                return reason;
            }
        }
        // The sender lives in `self`, so the channel cannot close under this borrow.
        std::future::pending().await
    }

    /// Resolves at the first interrupt after this call, so one sent while idle is not applied to
    /// the next turn.
    pub fn next_interrupt(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.inner.interrupts.subscribe();
        async move {
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Reads `reader` to EOF, applying commands. With `typed`, other lines are turns and EOF is the
/// end of input (queued turns still run); without it they are dropped and EOF stops the session.
pub fn pump_stdin<R: BufRead>(
    mut reader: R,
    control: &VoiceControl,
    typed: Option<mpsc::UnboundedSender<String>>,
) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("voice control: stdin read failed, treating it as closed: {e}");
                break;
            }
        }
        match parse_stdin_line(&String::from_utf8_lossy(&buf)) {
            StdinLine::Blank => {}
            StdinLine::Command(command) => {
                tracing::debug!(?command, "voice control command");
                control.apply(command);
            }
            StdinLine::Text(text) => match &typed {
                Some(tx) => {
                    let _ = tx.send(text);
                }
                None => tracing::warn!("voice control: ignoring a non-command stdin line"),
            },
            StdinLine::Malformed(error) => {
                tracing::warn!("voice control: ignoring an unknown command line ({error})");
            }
        }
    }
    if typed.is_none() {
        control.stop(StopReason::StdinEof);
    }
}

/// Typed turns forwarded by [`pump_stdin`]; `None` once stdin closes and the queue is drained.
pub struct TypedLineInput {
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
}

impl TypedLineInput {
    pub fn channel() -> (mpsc::UnboundedSender<String>, Self) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            tx,
            Self {
                rx: tokio::sync::Mutex::new(rx),
            },
        )
    }
}

#[async_trait]
impl VoiceInput for TypedLineInput {
    async fn listen(&self) -> Result<Option<String>> {
        Ok(self.rx.lock().await.recv().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn parses_the_two_commands() {
        assert_eq!(
            parse_stdin_line(r#"{"cmd":"interrupt"}"#),
            StdinLine::Command(ControlCommand::Interrupt)
        );
        assert_eq!(
            parse_stdin_line("  {\"cmd\":\"stop\"}\r\n"),
            StdinLine::Command(ControlCommand::Stop)
        );
    }

    #[test]
    fn ignores_extra_fields_on_a_command() {
        assert_eq!(
            parse_stdin_line(r#"{"cmd":"stop","sent_at":12}"#),
            StdinLine::Command(ControlCommand::Stop)
        );
    }

    #[test]
    fn blank_lines_are_nothing() {
        assert_eq!(parse_stdin_line(""), StdinLine::Blank);
        assert_eq!(parse_stdin_line("  \r\n"), StdinLine::Blank);
    }

    #[test]
    fn plain_text_is_typed_input() {
        assert_eq!(
            parse_stdin_line("what is the capital of france\n"),
            StdinLine::Text("what is the capital of france".to_string())
        );
    }

    #[test]
    fn an_unknown_or_broken_command_is_never_typed_input() {
        for line in [
            r#"{"cmd":"pause"}"#,
            r#"{"cmd":"STOP"}"#,
            r#"{"command":"stop"}"#,
            r#"{"cmd":"stop""#,
            "{",
        ] {
            assert!(
                matches!(parse_stdin_line(line), StdinLine::Malformed(_)),
                "{line:?} must be malformed"
            );
        }
    }

    #[test]
    fn the_first_stop_reason_wins() {
        let control = VoiceControl::new();
        assert_eq!(control.stop_requested(), None);
        control.stop(StopReason::Requested);
        control.stop(StopReason::StdinEof);
        assert_eq!(control.stop_requested(), Some(StopReason::Requested));
    }

    #[test]
    fn exit_reasons_match_the_ndjson_contract() {
        assert_eq!(StopReason::Requested.as_exit_reason(), "stopped");
        assert_eq!(StopReason::StdinEof.as_exit_reason(), "stdin_eof");
    }

    #[tokio::test]
    async fn stopped_resolves_for_a_stop_recorded_earlier() {
        let control = VoiceControl::new();
        control.stop(StopReason::StdinEof);
        let reason = tokio::time::timeout(Duration::from_secs(1), control.stopped())
            .await
            .expect("an earlier stop must not be missed");
        assert_eq!(reason, StopReason::StdinEof);
    }

    #[tokio::test]
    async fn stopped_wakes_on_a_later_stop() {
        let control = VoiceControl::new();
        let waiter = tokio::spawn({
            let control = control.clone();
            async move { control.stopped().await }
        });
        tokio::task::yield_now().await;
        control.apply(ControlCommand::Stop);
        let reason = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("stop must wake the waiter")
            .unwrap();
        assert_eq!(reason, StopReason::Requested);
    }

    #[tokio::test]
    async fn an_interrupt_before_the_turn_began_is_not_applied_to_it() {
        let control = VoiceControl::new();
        control.interrupt();
        let next = control.next_interrupt();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), next)
                .await
                .is_err(),
            "an interrupt sent while idle must not end the next turn"
        );
    }

    #[tokio::test]
    async fn an_interrupt_during_the_turn_resolves_it() {
        let control = VoiceControl::new();
        let next = control.next_interrupt();
        control.apply(ControlCommand::Interrupt);
        tokio::time::timeout(Duration::from_secs(1), next)
            .await
            .expect("the interrupt must reach the turn");
    }

    #[test]
    fn eof_on_a_command_only_stdin_stops_the_session() {
        let control = VoiceControl::new();
        pump_stdin(Cursor::new("{\"cmd\":\"interrupt\"}\n"), &control, None);
        assert_eq!(control.stop_requested(), Some(StopReason::StdinEof));
    }

    #[test]
    fn a_stop_command_is_recorded_before_the_eof_that_follows_it() {
        let control = VoiceControl::new();
        pump_stdin(Cursor::new("{\"cmd\":\"stop\"}\n"), &control, None);
        assert_eq!(control.stop_requested(), Some(StopReason::Requested));
    }

    #[test]
    fn text_on_a_command_only_stdin_is_dropped() {
        let control = VoiceControl::new();
        pump_stdin(Cursor::new("hello\nnot json either\n"), &control, None);
        assert_eq!(control.stop_requested(), Some(StopReason::StdinEof));
    }

    #[tokio::test]
    async fn typed_lines_queue_as_turns_and_eof_ends_input_without_a_stop() {
        let control = VoiceControl::new();
        let (tx, input) = TypedLineInput::channel();
        pump_stdin(
            Cursor::new("first turn\n\n{\"cmd\":\"interrupt\"}\nsecond turn"),
            &control,
            Some(tx),
        );

        assert_eq!(input.listen().await.unwrap().as_deref(), Some("first turn"));
        assert_eq!(
            input.listen().await.unwrap().as_deref(),
            Some("second turn")
        );
        assert_eq!(input.listen().await.unwrap(), None, "EOF ends typed input");
        assert_eq!(
            control.stop_requested(),
            None,
            "EOF must not abandon queued typed turns"
        );
    }

    #[tokio::test]
    async fn a_stop_on_a_typed_stdin_is_still_a_stop() {
        let control = VoiceControl::new();
        let (tx, input) = TypedLineInput::channel();
        pump_stdin(Cursor::new("{\"cmd\":\"stop\"}\n"), &control, Some(tx));
        assert_eq!(control.stop_requested(), Some(StopReason::Requested));
        assert_eq!(
            input.listen().await.unwrap(),
            None,
            "a command is never a turn"
        );
    }

    #[test]
    fn invalid_utf8_does_not_end_the_reader() {
        let control = VoiceControl::new();
        let mut bytes = vec![0xff, 0xfe, b'\n'];
        bytes.extend_from_slice(b"{\"cmd\":\"stop\"}\n");
        pump_stdin(Cursor::new(bytes), &control, None);
        assert_eq!(control.stop_requested(), Some(StopReason::Requested));
    }
}
