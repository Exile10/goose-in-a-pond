//! Manager for the terminal-voice child process.
//!
//! Architecture A of the terminal-voice-in-desktop contract: the Tauri shell
//! spawns and owns a `pond-server chat --input whisper --json-events
//! --session-id <uuid>` child that exclusively owns the microphone and speaker
//! (wake word, VAD, ASR, TTS, barge-in all in-child). The shell parses the
//! child's stdout NDJSON stream and re-emits it as Tauri events.
//!
//! Modeled closely on [`crate::process::ServerProcess`]:
//!   * `Mutex<Option<Child>>` for the spawned process handle
//!   * an async `lifecycle_lock` that serializes start/stop so concurrent
//!     invokes cannot race into a double-spawn or a spawn/kill interleave
//!   * `kill()` on `RunEvent::Exit`
//!   * `cleanup_orphaned_child()` startup pattern
//!   * binary resolution shared with `ServerProcess` (incl. `POND_SERVER_BIN`)
//!
//! The shell is built with `panic = "abort"` (see `Cargo.toml`). A poisoned
//! std `Mutex` would normally surface as an `unwrap` panic — which, under
//! `panic = "abort"`, would take down the whole app. To stay robust we never
//! `unwrap()` a `Mutex::lock()`; the [`lock`] helper recovers the guard from a
//! `PoisonError` instead, so a panic in one code path can never cascade into an
//! abort here.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

use crate::process::{resolve_binary_path, server_binary_name};

/// How long `stop` waits for the child to exit after its stdin is closed
/// before it resorts to `kill()`. The contract specifies ~3s.
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(3);
const GRACEFUL_STOP_POLL: Duration = Duration::from_millis(50);

/// Lock a std `Mutex` without ever panicking on poison.
///
/// Under `panic = "abort"` a poisoned mutex from an `unwrap()` would abort the
/// whole process. A poisoned lock here only means some other code path panicked
/// while holding it; the data behind it is still structurally valid for our
/// purposes (an `Option<Child>` / `Option<String>`), so recovering the guard is
/// the safe, non-cascading choice.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A single Tauri event to emit: the event name plus its JSON payload.
///
/// Kept as an owned pair (rather than emitting inline) so the NDJSON→event
/// mapping can be a pure function exercised directly by unit tests with the
/// golden lines from the contract.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceEvent {
    pub name: &'static str,
    pub payload: serde_json::Value,
}

/// Map one NDJSON line from the child's stdout to the Tauri event it should
/// produce, per section 2 of the contract.
///
/// Returns:
///   * `Ok(Some(event))` — a recognised event line
///   * `Ok(None)`        — a recognised line that produces no Tauri event
///     (empty lines and `exit`, which the reader loop handles as lifecycle)
///   * `Err(reason)`     — the line is not valid NDJSON or lacks an `event`
///     field; the caller logs a warning and continues (never crashes)
///
/// This is a pure function: no I/O, no shared state. All the contract's
/// snake_case field names and event-name mappings live here so a single test
/// suite pins them.
pub fn ndjson_line_to_event(line: &str) -> Result<Option<VoiceEvent>, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("not valid JSON: {e}"))?;

    let event = value
        .get("event")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing string `event` field".to_string())?;

    let string_field = |key: &str| -> String {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };

    let mapped = match event {
        "ready" => VoiceEvent {
            name: "voice-ready",
            payload: serde_json::json!({ "session_id": string_field("session_id") }),
        },
        "state" => VoiceEvent {
            // Contract: payload is the raw state string (wait/listen/thinking/speak).
            name: "voice-state",
            payload: serde_json::Value::String(string_field("state")),
        },
        "transcript" => VoiceEvent {
            name: "voice-transcript",
            payload: serde_json::json!({ "text": string_field("text") }),
        },
        "token" => VoiceEvent {
            name: "voice-token",
            payload: serde_json::json!({ "content": string_field("content") }),
        },
        "tool_call" => VoiceEvent {
            name: "voice-tool-call",
            payload: serde_json::json!({
                "tool": string_field("tool"),
                "id": string_field("id"),
            }),
        },
        "tool_result" => VoiceEvent {
            name: "voice-tool-result",
            payload: serde_json::json!({
                "tool": string_field("tool"),
                "id": string_field("id"),
                "content": string_field("content"),
            }),
        },
        "turn_complete" => VoiceEvent {
            name: "voice-done",
            payload: serde_json::json!({ "session_id": string_field("session_id") }),
        },
        "error" => VoiceEvent {
            name: "voice-error",
            payload: serde_json::json!({ "message": string_field("message") }),
        },
        // `exit` is not re-emitted directly; the reader loop emits
        // `voice-session-ended` once the child actually exits and is reaped.
        // We still parse it so its `reason` can be surfaced there.
        "exit" => return Ok(None),
        other => return Err(format!("unknown event kind `{other}`")),
    };

    Ok(Some(mapped))
}

/// Extract the `reason` field from an `exit` NDJSON line, if the line is one.
/// Used by the reader loop to label `voice-session-ended`.
fn exit_reason(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("event").and_then(|v| v.as_str()) != Some("exit") {
        return None;
    }
    Some(
        value
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or(end_reason::STDIN_EOF)
            .to_string(),
    )
}

/// Reasons a voice child session ended, carried in `voice-session-ended`.
mod end_reason {
    /// Child saw stdin EOF and exited cleanly — the normal stop path.
    pub const STDIN_EOF: &str = "stdin_eof";
    /// The child exited but produced no `exit` line (crash / abort).
    pub const CRASHED: &str = "crashed";
}

/// Handle to the (optionally) spawned terminal-voice child process.
///
/// Exactly one child may be alive at a time. `active` is the `VoiceChildActive`
/// flag that gates the shell's own mic paths (`record_with_vad`,
/// `run_voice_pipeline`, wake-listener restart) while a child owns the audio
/// devices.
///
/// The fields the reader thread must touch (`child`, `session_id`, `active`)
/// are stored behind `Arc` so the reader can reap the process and clear state
/// on EOF while the command handlers hold their own handles.
pub struct VoiceChatProcess {
    /// The spawned child. `None` when no session is active. Shared with the
    /// reader thread, which reaps it on stdout EOF.
    child: Arc<Mutex<Option<Child>>>,
    /// The child's stdin, held open for the lifetime of the session. Dropping
    /// it closes the pipe, which the child treats as EOF and exits cleanly.
    /// Only the command handlers touch this (to close it in `stop`).
    stdin: Mutex<Option<ChildStdin>>,
    /// The generated session uuid for the live child. `None` when inactive.
    /// Shared with the reader thread so `voice-done` correlation is stable.
    session_id: Arc<Mutex<Option<String>>>,
    /// Serializes start/stop so two invokes cannot race into a double-spawn or
    /// a spawn/kill interleave. Mirrors `ServerProcess::recovery_lock`.
    lifecycle_lock: AsyncMutex<()>,
    /// `VoiceChildActive` flag — true while a child owns the audio devices.
    /// Shared with the reader thread, which clears it on child exit.
    active: Arc<AtomicBool>,
    /// Whether the shell's wake listener was running when the session started,
    /// so `stop` can restore it. Only meaningful while `active` is true.
    wake_was_running: AtomicBool,
}

impl VoiceChatProcess {
    pub fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            stdin: Mutex::new(None),
            session_id: Arc::new(Mutex::new(None)),
            lifecycle_lock: AsyncMutex::new(()),
            active: Arc::new(AtomicBool::new(false)),
            wake_was_running: AtomicBool::new(false),
        }
    }

    /// True while a voice child is active (contract: `voice_session_active`).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Record whether the wake listener was running before this session so
    /// `stop` can restore it. Called by `start_voice_session` after it has
    /// stopped the listener.
    pub fn set_wake_was_running(&self, was_running: bool) {
        self.wake_was_running.store(was_running, Ordering::SeqCst);
    }

    pub fn wake_was_running(&self) -> bool {
        self.wake_was_running.load(Ordering::SeqCst)
    }

    /// Acquire the lifecycle lock so callers (the command handlers) can perform
    /// a start or stop without racing another one.
    pub async fn lifecycle_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lifecycle_lock.lock().await
    }

    /// Spawn the voice child and start the stdout reader thread.
    ///
    /// Callers MUST hold [`Self::lifecycle_guard`] and MUST have already
    /// verified the child is not active. Generates and returns the session
    /// uuid. Reuses the same binary-resolution logic as `ServerProcess`
    /// (including the `POND_SERVER_BIN` override).
    pub fn spawn(&self, app: &AppHandle, resource_dir: &std::path::Path) -> Result<String, String> {
        // Never spawn a second child.
        if self.is_active() {
            return Err("voice session already active".to_string());
        }

        // Reap any dead handle lingering from a previous session.
        self.cleanup_orphaned_child();

        let binary_name = server_binary_name();
        let binary_path = resolve_binary_path(resource_dir, binary_name).ok_or_else(|| {
            format!(
                "No pond-server binary found. Place it at \
                 src-tauri/binaries/{binary_name}, bundle it with the app, \
                 or set POND_SERVER_BIN."
            )
        })?;

        let session_id = uuid::Uuid::new_v4().to_string();

        tracing::info!(
            "Spawning voice child: {} chat --input whisper --json-events --session-id {}",
            binary_path.display(),
            session_id
        );

        let mut child = Command::new(&binary_path)
            .arg("chat")
            .arg("--input")
            .arg("whisper")
            .arg("--json-events")
            .arg("--session-id")
            .arg(&session_id)
            // stdin held open: closing it later is the clean-exit signal.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn voice child: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "voice child produced no stdin pipe".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "voice child produced no stdout pipe".to_string())?;
        let stderr = child.stderr.take();

        // Publish state before starting the reader so events dispatch against a
        // consistent view.
        lock(&self.child).replace(child);
        lock(&self.stdin).replace(stdin);
        lock(&self.session_id).replace(session_id.clone());
        self.active.store(true, Ordering::SeqCst);

        // stderr → tracing at debug level (contract: human diagnostics only).
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            std::thread::spawn(move || {
                for line in reader.lines() {
                    match line {
                        Ok(l) if !l.trim().is_empty() => {
                            tracing::debug!(target: "voice_child_stderr", "{l}");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::debug!("voice child stderr read error: {e}");
                            break;
                        }
                    }
                }
            });
        }

        // stdout NDJSON reader → Tauri events. On EOF it reaps the child and
        // emits `voice-session-ended`, then clears manager state.
        let app_reader = app.clone();
        let child_slot = self.child.clone();
        let session_slot = self.session_id.clone();
        let active_flag = self.active.clone();
        let reader = BufReader::new(stdout);
        std::thread::spawn(move || {
            run_stdout_reader(app_reader, reader, child_slot, session_slot, active_flag);
        });

        Ok(session_id)
    }

    /// Close the child's stdin. This is the clean-exit signal: the child sees
    /// EOF on stdin and breaks out of its run loop, closing stdout, which the
    /// reader thread observes to reap the process and clear state.
    ///
    /// Idempotent: a no-op when stdin is already closed. Callers MUST hold
    /// [`Self::lifecycle_guard`], then poll [`Self::is_active`] and escalate to
    /// [`Self::kill`] if the child does not exit within the grace period. The
    /// timed wait lives in the async command so it can yield the runtime rather
    /// than block a worker thread.
    pub fn close_stdin(&self) {
        // Dropping stdin closes the pipe → child sees EOF → run_loop breaks.
        drop(lock(&self.stdin).take());
    }

    /// The grace period a caller should wait after [`Self::close_stdin`] before
    /// escalating to [`Self::kill`].
    pub const fn graceful_stop_timeout() -> Duration {
        GRACEFUL_STOP_TIMEOUT
    }

    /// The interval a caller should poll [`Self::is_active`] at while waiting
    /// for a clean exit.
    pub const fn graceful_stop_poll() -> Duration {
        GRACEFUL_STOP_POLL
    }

    /// Kill the child immediately and reap it. Used on `RunEvent::Exit` and as
    /// the escalation path from `stop`. Always clears state; never panics.
    pub fn kill(&self) {
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("voice child killed");
        }
        drop(lock(&self.stdin).take());
        lock(&self.session_id).take();
        self.active.store(false, Ordering::SeqCst);
    }

    /// Reap a dead child handle left over from a previous session. Mirrors
    /// `ServerProcess::cleanup_orphaned_child`. Safe to call at startup and
    /// before every spawn.
    pub fn cleanup_orphaned_child(&self) {
        {
            let mut guard = lock(&self.child);
            if let Some(child) = guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        tracing::warn!("orphaned voice child already exited with status: {status}");
                    }
                    Ok(None) => {
                        tracing::warn!("orphaned voice child still running at startup; killing");
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    Err(e) => {
                        tracing::warn!("failed to inspect voice child status: {e}");
                    }
                }
                *guard = None;
            }
        }
        // Reset derived state so a leftover flag can never gate the mic paths
        // after the child is gone.
        drop(lock(&self.stdin).take());
        lock(&self.session_id).take();
        self.active.store(false, Ordering::SeqCst);
    }
}

impl Default for VoiceChatProcess {
    fn default() -> Self {
        Self::new()
    }
}

/// Drive the child's stdout: parse each NDJSON line, emit the mapped Tauri
/// event, and on EOF reap the child and emit `voice-session-ended`.
///
/// Runs on its own OS thread. Robust to non-JSON lines (logs a warning and
/// continues) and to a poisoned lock (recovered via [`lock`]).
fn run_stdout_reader(
    app: AppHandle,
    reader: BufReader<std::process::ChildStdout>,
    child: Arc<Mutex<Option<Child>>>,
    session_id: Arc<Mutex<Option<String>>>,
    active: Arc<AtomicBool>,
) {
    // The child's last `exit` line, if any, carries the clean reason.
    let mut clean_reason: Option<String> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("voice child stdout read error: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        // Capture an `exit` line's reason before mapping drops it.
        if let Some(reason) = exit_reason(&line) {
            clean_reason = Some(reason);
            continue;
        }

        match ndjson_line_to_event(&line) {
            Ok(Some(event)) => {
                if let Err(e) = app.emit(event.name, event.payload) {
                    tracing::warn!("failed to emit {}: {e}", event.name);
                }
            }
            Ok(None) => {}
            Err(reason) => {
                // Non-JSON / unknown line — contract says warn, do not crash.
                tracing::warn!("ignoring non-contract voice child line ({reason}): {line}");
            }
        }
    }

    // stdout closed → the child is exiting. Reap it to avoid a zombie and read
    // its exit code.
    let code = {
        let mut guard = lock(&child);
        match guard.take() {
            Some(mut c) => {
                let status = c.wait();
                match status {
                    Ok(s) => s.code(),
                    Err(e) => {
                        tracing::warn!("failed to wait on voice child: {e}");
                        None
                    }
                }
            }
            None => None,
        }
    };

    // A missing `exit` line means the child died without a clean shutdown
    // (crash / kill). If it was killed by us, `active` is already false and the
    // exit code is typically absent (signal termination).
    let reason = clean_reason.unwrap_or_else(|| end_reason::CRASHED.to_string());

    // Clear derived state.
    lock(&session_id).take();
    active.store(false, Ordering::SeqCst);

    if let Err(e) = app.emit(
        "voice-session-ended",
        serde_json::json!({ "code": code, "reason": reason }),
    ) {
        tracing::warn!("failed to emit voice-session-ended: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Golden lines from the contract (section 1) ─────────────────────────

    #[test]
    fn ready_line_maps_to_voice_ready() {
        let ev = ndjson_line_to_event(r#"{"event":"ready","session_id":"abc-123"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-ready");
        assert_eq!(ev.payload, json!({ "session_id": "abc-123" }));
    }

    #[test]
    fn state_line_payload_is_raw_string() {
        for s in ["wait", "listen", "thinking", "speak"] {
            let line = format!(r#"{{"event":"state","state":"{s}"}}"#);
            let ev = ndjson_line_to_event(&line).unwrap().unwrap();
            assert_eq!(ev.name, "voice-state");
            // Contract: the voice-state payload is the raw string, not an object.
            assert_eq!(ev.payload, json!(s));
        }
    }

    #[test]
    fn transcript_line_maps_to_voice_transcript() {
        let ev = ndjson_line_to_event(r#"{"event":"transcript","text":"turn on the lights"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-transcript");
        assert_eq!(ev.payload, json!({ "text": "turn on the lights" }));
    }

    #[test]
    fn token_line_maps_to_voice_token() {
        let ev = ndjson_line_to_event(r#"{"event":"token","content":"Sure"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-token");
        assert_eq!(ev.payload, json!({ "content": "Sure" }));
    }

    #[test]
    fn tool_call_line_maps_to_voice_tool_call() {
        let ev = ndjson_line_to_event(r#"{"event":"tool_call","tool":"giap__lights","id":"t1"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-tool-call");
        assert_eq!(ev.payload, json!({ "tool": "giap__lights", "id": "t1" }));
    }

    #[test]
    fn tool_result_line_maps_to_voice_tool_result() {
        let line =
            r#"{"event":"tool_result","tool":"giap__lights","id":"t1","content":"ok, done"}"#;
        let ev = ndjson_line_to_event(line).unwrap().unwrap();
        assert_eq!(ev.name, "voice-tool-result");
        assert_eq!(
            ev.payload,
            json!({ "tool": "giap__lights", "id": "t1", "content": "ok, done" })
        );
    }

    #[test]
    fn turn_complete_line_maps_to_voice_done() {
        let ev = ndjson_line_to_event(r#"{"event":"turn_complete","session_id":"abc-123"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-done");
        assert_eq!(ev.payload, json!({ "session_id": "abc-123" }));
    }

    #[test]
    fn error_line_maps_to_voice_error() {
        let ev = ndjson_line_to_event(r#"{"event":"error","message":"mic busy"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-error");
        assert_eq!(ev.payload, json!({ "message": "mic busy" }));
    }

    // ── exit / lifecycle lines produce no direct Tauri event ───────────────

    #[test]
    fn exit_line_produces_no_direct_event() {
        // The reader loop turns `exit` into `voice-session-ended`, so the pure
        // mapper returns None for it.
        let mapped = ndjson_line_to_event(r#"{"event":"exit","reason":"stdin_eof"}"#).unwrap();
        assert!(mapped.is_none());
    }

    #[test]
    fn exit_reason_extracts_stdin_eof() {
        assert_eq!(
            exit_reason(r#"{"event":"exit","reason":"stdin_eof"}"#),
            Some("stdin_eof".to_string())
        );
        assert_eq!(
            exit_reason(r#"{"event":"exit","reason":"dismissed"}"#),
            Some("dismissed".to_string())
        );
        // Non-exit lines are not exit reasons.
        assert_eq!(exit_reason(r#"{"event":"state","state":"wait"}"#), None);
        // Malformed lines are not exit reasons.
        assert_eq!(exit_reason("not json"), None);
    }

    #[test]
    fn exit_line_without_reason_defaults_to_stdin_eof() {
        assert_eq!(
            exit_reason(r#"{"event":"exit"}"#),
            Some("stdin_eof".to_string())
        );
    }

    // ── Robustness: bad lines never crash, they error for the caller to log ─

    #[test]
    fn empty_line_is_ignored() {
        assert!(ndjson_line_to_event("").unwrap().is_none());
        assert!(ndjson_line_to_event("   ").unwrap().is_none());
    }

    #[test]
    fn non_json_line_is_an_error_not_a_panic() {
        let err = ndjson_line_to_event("this is a banner line, not JSON").unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn json_without_event_field_is_an_error() {
        let err = ndjson_line_to_event(r#"{"state":"wait"}"#).unwrap_err();
        assert!(err.contains("missing string `event`"), "got: {err}");
    }

    #[test]
    fn unknown_event_kind_is_an_error() {
        let err = ndjson_line_to_event(r#"{"event":"heartbeat"}"#).unwrap_err();
        assert!(err.contains("unknown event kind"), "got: {err}");
    }

    #[test]
    fn missing_string_fields_default_to_empty() {
        // A `transcript` with no `text` field still maps, with an empty string,
        // rather than erroring — the reader stays resilient.
        let ev = ndjson_line_to_event(r#"{"event":"transcript"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.payload, json!({ "text": "" }));
    }

    #[test]
    fn leading_and_trailing_whitespace_is_tolerated() {
        let ev = ndjson_line_to_event("  {\"event\":\"state\",\"state\":\"speak\"}  \n")
            .unwrap()
            .unwrap();
        assert_eq!(ev.name, "voice-state");
        assert_eq!(ev.payload, json!("speak"));
    }

    // ── Manager state invariants (no child spawned) ────────────────────────

    #[test]
    fn new_manager_is_inactive() {
        let mgr = VoiceChatProcess::new();
        assert!(!mgr.is_active());
        assert!(!mgr.wake_was_running());
    }

    #[test]
    fn wake_was_running_round_trips() {
        let mgr = VoiceChatProcess::new();
        mgr.set_wake_was_running(true);
        assert!(mgr.wake_was_running());
        mgr.set_wake_was_running(false);
        assert!(!mgr.wake_was_running());
    }

    #[test]
    fn cleanup_on_empty_manager_is_a_noop() {
        let mgr = VoiceChatProcess::new();
        mgr.cleanup_orphaned_child();
        assert!(!mgr.is_active());
    }

    #[test]
    fn close_stdin_on_empty_manager_is_a_noop() {
        let mgr = VoiceChatProcess::new();
        mgr.close_stdin();
        assert!(!mgr.is_active());
    }

    #[test]
    fn kill_on_empty_manager_is_a_noop() {
        let mgr = VoiceChatProcess::new();
        mgr.kill();
        assert!(!mgr.is_active());
    }

    #[test]
    fn graceful_stop_constants_are_sane() {
        // The contract specifies ~3s; the poll must be well under that.
        assert_eq!(
            VoiceChatProcess::graceful_stop_timeout(),
            std::time::Duration::from_secs(3)
        );
        assert!(VoiceChatProcess::graceful_stop_poll() < VoiceChatProcess::graceful_stop_timeout());
    }
}
