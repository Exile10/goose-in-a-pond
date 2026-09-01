//! Manager for the terminal-voice child process.
//!
//! Architecture A of the terminal-voice-in-desktop contract: the Tauri shell
//! spawns and owns a `pond-server chat --voice --json-events
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

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

use crate::process::{dev_repo_root, resolve_binary_path, server_binary_name};

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

/// The classification of a single NDJSON stdout line, derived from ONE parse.
///
/// The reader loop parses each line exactly once (a hot path — every token
/// delta is a line) and branches on this instead of parsing the same bytes in
/// both `ndjson_line_to_event` and a separate exit-reason extractor.
#[derive(Debug, Clone, PartialEq)]
pub enum LineClass {
    /// A recognised event line that maps to a Tauri event.
    Event(VoiceEvent),
    /// The child's `exit` line, carrying its (stable) reason string. The reader
    /// loop remembers this to label the eventual `voice-session-ended`.
    Exit(String),
    /// A recognised line that produces no Tauri event and is not an exit
    /// (e.g. an empty line).
    Skip,
}

/// Classify one NDJSON line from the child's stdout in a single parse, per
/// section 2 of the contract.
///
/// Returns:
///   * `Ok(LineClass::Event(event))` — a recognised event line
///   * `Ok(LineClass::Exit(reason))` — the child's `exit` lifecycle line
///   * `Ok(LineClass::Skip)`         — an empty line (no event, no exit)
///   * `Err(reason)`                 — the line is not valid NDJSON or lacks an
///     `event` field; the caller logs a warning and continues (never crashes)
///
/// This is a pure function: no I/O, no shared state. All the contract's
/// snake_case field names and event-name mappings live here so a single test
/// suite pins them.
pub fn classify_line(line: &str) -> Result<LineClass, String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(LineClass::Skip);
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
        // Prefix warm-up progress at session start: `warming` while the model
        // loads and the prompt prefix prefills, then ready/skipped/failed. The
        // child also SPEAKS these transitions; this event lets the UI label
        // the stretch where the mic is not yet listening.
        "warmup" => VoiceEvent {
            name: "voice-warmup",
            payload: serde_json::Value::String(string_field("state")),
        },
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
        // Distinct from the legacy, unrelated "audio-level" event (audio.rs /
        // audio_cmd.rs) — this one belongs exclusively to the child-process
        // voice-* event family (see useVoiceSession.ts's EVENT OWNERSHIP RULE).
        "audio_level" => VoiceEvent {
            name: "voice-audio-level",
            payload: serde_json::json!({
                "rms": value.get("rms").and_then(|v| v.as_f64()).unwrap_or(0.0)
            }),
        },
        // `exit` is not re-emitted directly; the reader loop emits
        // `voice-session-ended` once the child actually exits and is reaped.
        // We surface its `reason` here so the eventual ended event is labelled.
        "exit" => {
            let reason = value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or(end_reason::STDIN_EOF)
                .to_string();
            return Ok(LineClass::Exit(reason));
        }
        other => return Err(format!("unknown event kind `{other}`")),
    };

    Ok(LineClass::Event(mapped))
}

/// Reasons a voice child session ended, carried in `voice-session-ended`.
///
/// These strings are part of the frontend contract — the UI classifies an
/// ended session by `reason`. Keep the existing values stable
/// (`stdin_eof` / `dismissed` / `error` / `crashed`); new reasons are additive.
mod end_reason {
    /// Child saw stdin EOF and exited cleanly — the normal stop path.
    pub const STDIN_EOF: &str = "stdin_eof";
    /// The child exited but produced no `exit` line — a crash, abort, or signal
    /// termination (SIGKILL/SIGSEGV/…), all of which are abnormal from the
    /// child's perspective. A user-initiated stop that escalates to kill bumps
    /// the generation first, so that reader returns early and never labels an
    /// end reason here; this const therefore covers only genuine abnormal exits.
    pub const CRASHED: &str = "crashed";
    /// The child died during startup — it never emitted `ready`, so no session
    /// existed to crash. Distinct from [`CRASHED`] because the causes are
    /// different in kind (a binary that cannot run against this machine's
    /// state: stale sidecar, failed DB migration, missing dylib) and because
    /// the child's own explanation is on stderr, not in any NDJSON line. This
    /// is the only end reason whose payload carries `detail`.
    pub const FAILED_TO_START: &str = "failed_to_start";
}

/// How many trailing stderr lines to retain for a startup failure's `detail`.
///
/// The child's stderr is a full tracing stream; only the tail matters, and the
/// fatal line is almost always last. Bounded so a chatty session cannot grow
/// this without limit over its lifetime.
const STDERR_TAIL_LINES: usize = 20;

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
    /// reader thread, which reaps it on stdout EOF — but only if its generation
    /// still matches (see `generation`).
    child: Arc<Mutex<Option<Child>>>,
    /// The child's stdin, held open for the lifetime of the session. Dropping
    /// it closes the pipe, which the child treats as EOF and exits cleanly.
    /// Only the command handlers touch this (to close it in `stop`).
    stdin: Mutex<Option<ChildStdin>>,
    /// The generated session uuid for the live child. `None` when inactive.
    /// Shared with the reader thread, which reads it to label the
    /// `voice-session-ended` payload so the frontend can drop stale events.
    session_id: Arc<Mutex<Option<String>>>,
    /// Monotonically increasing session generation, bumped on every `spawn`.
    /// Each reader thread captures the generation it was spawned for and only
    /// reaps the child / clears state / emits `voice-session-ended` if the live
    /// generation still matches. This stops a slow, backlogged reader from a
    /// killed session from reaping a *newer* child that has since taken the
    /// shared slot during a rapid stop/start cycle.
    generation: Arc<AtomicU64>,
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
            generation: Arc::new(AtomicU64::new(0)),
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

    /// The session id of the currently-active child, if any. Lets a redundant
    /// `start_voice_session` be idempotent — returning the live session id —
    /// instead of erroring when a fast remount (React StrictMode's dev
    /// double-invoke) races the teardown and finds the child still active.
    pub fn current_session_id(&self) -> Option<String> {
        lock(&self.session_id).clone()
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
                 or set POND_SERVER_BIN. (For a real bundle run \
                 `npm run stage:server`; `npm run stage:server:stub` stages a \
                 compile-only placeholder that is NOT a working server.)"
            )
        })?;

        let session_id = uuid::Uuid::new_v4().to_string();

        tracing::info!(
            "Spawning voice child: {} chat --voice --json-events --session-id {}",
            binary_path.display(),
            session_id
        );

        // `--voice` replaced `--input whisper`. A staged sidecar older than
        // that rename dies on clap's "unexpected argument" before it emits a
        // single NDJSON line — the same symptom as a sidecar older than the
        // database's migrations, and the same fix: re-run
        // `scripts/stage-server-sidecar.sh`. The stderr tail attached to
        // `voice-session-ended` carries clap's message, which names the flag.
        let mut cmd = Command::new(&binary_path);
        cmd.arg("chat")
            .arg("--voice")
            .arg("--json-events")
            .arg("--session-id")
            .arg(&session_id)
            // stdin held open: closing it later is the clean-exit signal.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // In dev builds, ground the child's cwd at the repo root so the
        // GooseAdapter inside the voice child resolves extension/music paths
        // (e.g. `extensions/music/src/server.ts`) the same way the serve child
        // does (see `ServerProcess::ensure_running`). No-op in production
        // bundles, where the compile-time repo path does not exist on the
        // user's machine and resource-relative paths are absolute.
        if let Some(root) = dev_repo_root() {
            cmd.current_dir(root);
        }
        let mut child = cmd
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

        // Record the child's pid to a well-known pidfile so a future launch can
        // reap this process if the shell itself is killed (kill -9) before it
        // can run its `RunEvent::Exit` handler. Removed again when the child is
        // reaped (kill / reader EOF / cleanup).
        write_pidfile(child.id());

        // Bump the generation for this session. The reader thread captures it
        // and refuses to reap/tear-down a slot whose generation has moved on.
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        // Publish state before starting the reader so events dispatch against a
        // consistent view.
        lock(&self.child).replace(child);
        lock(&self.stdin).replace(stdin);
        lock(&self.session_id).replace(session_id.clone());
        self.active.store(true, Ordering::SeqCst);

        // stderr → tracing at debug level (contract: human diagnostics only),
        // AND into a bounded ring buffer.
        //
        // The ring is what makes a startup failure explicable. A child that dies
        // before `ready` writes its reason ONLY here — `out!` is compiled to a
        // no-op under `--json-events`, so stdout carries nothing at all. Before
        // this, that reason existed solely in a `debug!` record nobody had
        // enabled, and the UI could say no more than "exited (code 1)". The
        // lines are still logged at debug for a live tail; the ring exists so
        // the reader can attach the tail to `voice-session-ended`.
        let stderr_tail: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        if let Some(stderr) = stderr {
            let reader = BufReader::new(stderr);
            let tail = stderr_tail.clone();
            std::thread::spawn(move || {
                for line in reader.lines() {
                    match line {
                        Ok(l) if !l.trim().is_empty() => {
                            tracing::debug!(target: "voice_child_stderr", "{l}");
                            let mut ring = lock(&tail);
                            if ring.len() == STDERR_TAIL_LINES {
                                ring.pop_front();
                            }
                            ring.push_back(l);
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
        // emits `voice-session-ended`, then clears manager state — but only if
        // its generation still matches the live one.
        let app_reader = app.clone();
        let child_slot = self.child.clone();
        let session_slot = self.session_id.clone();
        let active_flag = self.active.clone();
        let generation_slot = self.generation.clone();
        let reader = BufReader::new(stdout);
        std::thread::spawn(move || {
            run_stdout_reader(ReaderContext {
                app: app_reader,
                reader,
                child: child_slot,
                session_id: session_slot,
                active: active_flag,
                generation: generation_slot,
                my_generation: generation,
                stderr_tail,
            });
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
    ///
    /// Bumps the generation so the (now-stale) reader thread cannot reap a later
    /// child or tear down a newer session's state when it finally observes EOF.
    pub fn kill(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(mut child) = lock(&self.child).take() {
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("voice child killed");
        }
        drop(lock(&self.stdin).take());
        lock(&self.session_id).take();
        self.active.store(false, Ordering::SeqCst);
        remove_pidfile();
    }

    /// Reap a leftover voice child from a previous run.
    ///
    /// Two sources of orphan are handled:
    ///   1. An in-memory `Child` handle still in our slot (the ordinary
    ///      before-spawn hygiene case).
    ///   2. A `pond-server chat` process orphaned because the shell itself was
    ///      killed (`kill -9`, crash) before it could run its `RunEvent::Exit`
    ///      handler. A freshly-constructed `VoiceChatProcess` has an empty slot,
    ///      so the in-memory check alone can never see this — we recover it from
    ///      the pidfile written at spawn.
    ///
    /// The pidfile pid is only killed after verifying the process is BOTH alive
    /// AND its command line still matches a `pond-server chat` invocation, so a
    /// pid that was reused by an unrelated process is never touched. Safe to
    /// call at startup and before every spawn.
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

        // Recover a process orphaned by a hard kill of the shell (no in-memory
        // handle survives that), validating identity before killing.
        reap_pidfile_orphan();
    }
}

impl Default for VoiceChatProcess {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything the stdout reader thread needs, carried as one struct so the
/// thread's identity (the generation it was spawned for) travels alongside the
/// shared slots it may reap.
struct ReaderContext {
    app: AppHandle,
    reader: BufReader<std::process::ChildStdout>,
    child: Arc<Mutex<Option<Child>>>,
    session_id: Arc<Mutex<Option<String>>>,
    active: Arc<AtomicBool>,
    /// The live session generation, bumped by `spawn`/`kill`.
    generation: Arc<AtomicU64>,
    /// The generation this reader was spawned for. It only reaps/tears down the
    /// shared slots while this still equals the live generation.
    my_generation: u64,
    /// Trailing stderr lines, for explaining a startup failure. Shared with the
    /// stderr thread.
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

/// Decide the end reason for a child that closed stdout, and what to say about
/// it. Split out of [`run_stdout_reader`] so it is testable without an
/// `AppHandle` or a real process.
///
/// * `clean_reason` — the reason from the child's own `exit` line, if it sent
///   one. Its presence IS the definition of a clean shutdown.
/// * `saw_ready` — whether `ready` ever arrived. Distinguishes a session that
///   ran and then died from one that never started.
///
/// `detail` is populated only for a startup failure, and only from stderr:
/// after `ready` the child reports its own troubles as NDJSON `error` events,
/// so a stderr dump there would be noise duplicating a better signal.
fn classify_end(
    clean_reason: Option<&str>,
    saw_ready: bool,
    stderr_tail: &VecDeque<String>,
) -> (String, Option<String>) {
    if let Some(reason) = clean_reason {
        return (reason.to_string(), None);
    }
    if saw_ready {
        return (end_reason::CRASHED.to_string(), None);
    }
    let joined = stderr_tail
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    let detail = (!joined.trim().is_empty()).then_some(joined);
    (end_reason::FAILED_TO_START.to_string(), detail)
}

/// Drive the child's stdout: parse each NDJSON line, emit the mapped Tauri
/// event, and on EOF reap the child and emit `voice-session-ended`.
///
/// Runs on its own OS thread. Robust to non-JSON lines (logs a warning and
/// continues) and to a poisoned lock (recovered via [`lock`]). Each line is
/// parsed exactly once via [`classify_line`] — the per-token hot path never
/// re-parses the same bytes.
///
/// On EOF the reader only reaps the child / clears state / emits
/// `voice-session-ended` if its `my_generation` still matches the live
/// generation. A stale reader from a killed session (whose generation has since
/// moved on) therefore cannot reap a newer child or tear down a live session.
fn run_stdout_reader(ctx: ReaderContext) {
    let ReaderContext {
        app,
        reader,
        child,
        session_id,
        active,
        generation,
        my_generation,
        stderr_tail,
    } = ctx;

    // The child's last `exit` line, if any, carries the clean reason.
    let mut clean_reason: Option<String> = None;
    // Whether the child ever announced itself. Everything before `ready` is
    // startup; dying in that window is a different failure from a session that
    // ran and then crashed, and only the former can be explained by stderr.
    let mut saw_ready = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("voice child stdout read error: {e}");
                break;
            }
        };

        // Single parse per line: derive both the Tauri event mapping and the
        // exit reason from one `classify_line` call.
        match classify_line(&line) {
            Ok(LineClass::Event(event)) => {
                if event.name == "voice-ready" {
                    saw_ready = true;
                }
                if let Err(e) = app.emit(event.name, event.payload) {
                    tracing::warn!("failed to emit {}: {e}", event.name);
                }
            }
            Ok(LineClass::Exit(reason)) => {
                clean_reason = Some(reason);
            }
            Ok(LineClass::Skip) => {}
            Err(reason) => {
                // Non-JSON / unknown line — contract says warn, do not crash.
                tracing::warn!("ignoring non-contract voice child line ({reason}): {line}");
            }
        }
    }

    // stdout closed → the child is exiting. If a newer session has already taken
    // over (our generation is stale), do NOT touch the shared slots — the newer
    // spawn/kill owns them now. This is the rapid stop/start race guard.
    if generation.load(Ordering::SeqCst) != my_generation {
        tracing::debug!(
            "stale voice reader (gen {my_generation}) observed EOF after a newer session took over; not reaping"
        );
        return;
    }

    // Capture the session id for the ended payload before we clear it, so the
    // frontend can correlate the event with the session it is watching.
    let ended_session_id = lock(&session_id).clone();

    // Reap our child to avoid a zombie and read its exit code.
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

    // A missing `exit` line means the child died without a clean shutdown.
    // Signal termination (SIGKILL/SIGSEGV/…) reports no exit code on Unix, so an
    // absent code cannot distinguish an intentional kill from a genuine crash —
    // and a user-initiated stop that escalated to kill has already bumped the
    // generation, so its reader returned early above and never reaches here.
    // Everything that lands here is therefore an abnormal exit: report "crashed"
    // (matching the frontend, which expects `crashed` for a code=null child).
    let (reason, detail) = {
        let ring = lock(&stderr_tail);
        classify_end(clean_reason.as_deref(), saw_ready, &ring)
    };
    if let Some(detail) = &detail {
        // At warn, not debug: this is the whole explanation for a voice mode
        // that will not start, and the debug-level stream is what hid it.
        tracing::warn!("voice child failed to start; child stderr tail:\n{detail}");
    }

    // Clear derived state and drop the pidfile now that the child is reaped.
    lock(&session_id).take();
    active.store(false, Ordering::SeqCst);
    remove_pidfile();

    if let Err(e) = app.emit(
        "voice-session-ended",
        serde_json::json!({
            "code": code,
            "reason": reason,
            "session_id": ended_session_id,
            "detail": detail,
        }),
    ) {
        tracing::warn!("failed to emit voice-session-ended: {e}");
    }
}

// ── Pidfile: recover a voice child orphaned by a hard kill of the shell ──────
//
// The shell is `panic = "abort"` and `std::process::Child` is not killed when
// its parent dies, so a `kill -9` of the shell (or a crash before `RunEvent::
// Exit`) leaves the `pond-server chat` child running, holding the mic. We write
// the child's pid to a well-known per-user file at spawn and reap it on the next
// launch — but only after confirming the pid is alive AND still a
// `pond-server chat` process, so a reused pid is never killed.

/// Path to the voice-child pidfile, scoped per user under the OS temp dir.
///
/// `std::env::temp_dir()` is already user-private on most platforms, but the
/// per-user token in the filename makes the isolation explicit so two users on
/// one shared host never fight over one file. We only need per-user
/// *separation*, not the real uid, so a stable token derived from the account
/// name / home path is sufficient (and needs no libc dependency).
fn pidfile_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("giap-voice-child-{}.pid", per_user_token()))
}

/// A stable, per-user token used only to keep pidfiles from colliding across
/// accounts on a shared host. Not a real uid — just needs to differ per user.
fn per_user_token() -> String {
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            return sanitize_token(&user);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let hash = home
            .bytes()
            .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
        return hash.to_string();
    }
    "default".to_string()
}

/// Keep a token filesystem-safe: alphanumerics pass through, everything else
/// becomes `_`.
fn sanitize_token(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Write the live child's pid to the pidfile. Best effort — a failure only
/// means the hard-kill-orphan recovery is unavailable, not that the session is
/// broken.
fn write_pidfile(pid: u32) {
    let path = pidfile_path();
    if let Err(e) = std::fs::write(&path, pid.to_string()) {
        tracing::debug!("could not write voice pidfile {}: {e}", path.display());
    }
}

/// Remove the pidfile once the child is reaped. Best effort.
fn remove_pidfile() {
    let path = pidfile_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::debug!("could not remove voice pidfile {}: {e}", path.display()),
    }
}

/// Read the pidfile and, if it names a live `pond-server chat` process, kill and
/// reap it, then remove the file. A pid that is dead, unreadable, or belongs to
/// an unrelated (pid-reused) process is left untouched — we only remove the
/// stale file.
fn reap_pidfile_orphan() {
    let path = pidfile_path();
    let pid = match read_pidfile(&path) {
        Some(pid) => pid,
        None => return,
    };

    match pid_is_voice_child(pid) {
        Some(true) => {
            tracing::warn!("reaping orphaned voice child pid {pid} from a previous run");
            kill_pid(pid);
            let _ = std::fs::remove_file(&path);
        }
        Some(false) => {
            // Confirmed dead, or a live-but-unrelated (pid-reused) process — the
            // record is stale, so clear it.
            tracing::debug!("voice pidfile pid {pid} is not a live pond-server chat process; clearing stale file");
            let _ = std::fs::remove_file(&path);
        }
        None => {
            // We could not determine liveness (e.g. `ps` failed to spawn under
            // process-table/memory pressure — plausible on the 8GB Jetson). Do
            // NOT delete the pidfile: a real orphan may still be holding the mic,
            // and this is the only record of it. Keep it so a later launch can
            // retry recovery rather than permanently orphaning the child.
            tracing::warn!("could not determine status of voice pidfile pid {pid}; keeping the pidfile so a later launch can retry recovery");
        }
    }
}

/// Parse a pid out of the pidfile contents. `None` if missing/empty/malformed.
fn read_pidfile(path: &std::path::Path) -> Option<u32> {
    let contents = std::fs::read_to_string(path).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Classify the pidfile's `pid`, reading its command line via `ps` (portable
/// across macOS/Linux):
///   * `Some(true)`  — a live process whose command line is `pond-server chat`
///     (ours; safe to kill).
///   * `Some(false)` — `ps` ran and reported either no such process (dead) or a
///     live-but-unrelated (pid-reused) command line; never kill, clear the file.
///   * `None`        — `ps` could not be spawned, so liveness is INDETERMINATE;
///     the caller must not treat this as dead (that would destroy the orphan
///     record for a child that may still be holding the mic).
fn pid_is_voice_child(pid: u32) -> Option<bool> {
    let output = Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("command=")
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let cmdline = String::from_utf8_lossy(&out.stdout);
            let cmdline = cmdline.trim();
            Some(!cmdline.is_empty() && cmdline_is_voice_child(cmdline))
        }
        // `ps` ran and exited non-zero → no such pid → the process is gone.
        Ok(_) => Some(false),
        // `ps` itself could not be spawned → we cannot tell; stay indeterminate.
        Err(e) => {
            tracing::debug!("could not run `ps` to classify voice pid {pid}: {e}");
            None
        }
    }
}

/// Pure predicate: does a process command line identify our voice child?
/// Requires both the `pond-server` binary token and the `chat` subcommand so a
/// bare `pond-server serve` (the dashboard server) is not mistaken for it.
fn cmdline_is_voice_child(cmdline: &str) -> bool {
    cmdline.contains("pond-server") && cmdline.split_whitespace().any(|tok| tok == "chat")
}

/// Send SIGKILL to `pid` (best effort). Uses `kill` so we avoid a libc dep.
fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/F")
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Helper: classify a line and assert it produced a mappable event.
    fn event(line: &str) -> VoiceEvent {
        match classify_line(line).unwrap() {
            LineClass::Event(ev) => ev,
            other => panic!("expected an event, got {other:?} for line: {line}"),
        }
    }

    // ── Golden lines from the contract (section 1) ─────────────────────────

    #[test]
    fn ready_line_maps_to_voice_ready() {
        let ev = event(r#"{"event":"ready","session_id":"abc-123"}"#);
        assert_eq!(ev.name, "voice-ready");
        assert_eq!(ev.payload, json!({ "session_id": "abc-123" }));
    }

    #[test]
    fn state_line_payload_is_raw_string() {
        for s in ["wait", "listen", "thinking", "speak"] {
            let line = format!(r#"{{"event":"state","state":"{s}"}}"#);
            let ev = event(&line);
            assert_eq!(ev.name, "voice-state");
            // Contract: the voice-state payload is the raw string, not an object.
            assert_eq!(ev.payload, json!(s));
        }
    }

    #[test]
    fn transcript_line_maps_to_voice_transcript() {
        let ev = event(r#"{"event":"transcript","text":"turn on the lights"}"#);
        assert_eq!(ev.name, "voice-transcript");
        assert_eq!(ev.payload, json!({ "text": "turn on the lights" }));
    }

    #[test]
    fn token_line_maps_to_voice_token() {
        let ev = event(r#"{"event":"token","content":"Sure"}"#);
        assert_eq!(ev.name, "voice-token");
        assert_eq!(ev.payload, json!({ "content": "Sure" }));
    }

    #[test]
    fn tool_call_line_maps_to_voice_tool_call() {
        let ev = event(r#"{"event":"tool_call","tool":"giap__lights","id":"t1"}"#);
        assert_eq!(ev.name, "voice-tool-call");
        assert_eq!(ev.payload, json!({ "tool": "giap__lights", "id": "t1" }));
    }

    #[test]
    fn tool_result_line_maps_to_voice_tool_result() {
        let ev = event(
            r#"{"event":"tool_result","tool":"giap__lights","id":"t1","content":"ok, done"}"#,
        );
        assert_eq!(ev.name, "voice-tool-result");
        assert_eq!(
            ev.payload,
            json!({ "tool": "giap__lights", "id": "t1", "content": "ok, done" })
        );
    }

    #[test]
    fn turn_complete_line_maps_to_voice_done() {
        let ev = event(r#"{"event":"turn_complete","session_id":"abc-123"}"#);
        assert_eq!(ev.name, "voice-done");
        assert_eq!(ev.payload, json!({ "session_id": "abc-123" }));
    }

    #[test]
    fn error_line_maps_to_voice_error() {
        let ev = event(r#"{"event":"error","message":"mic busy"}"#);
        assert_eq!(ev.name, "voice-error");
        assert_eq!(ev.payload, json!({ "message": "mic busy" }));
    }

    #[test]
    fn audio_level_line_maps_to_voice_audio_level() {
        let ev = event(r#"{"event":"audio_level","rms":0.42}"#);
        assert_eq!(ev.name, "voice-audio-level");
        assert_eq!(ev.payload, json!({ "rms": 0.42 }));
    }

    // ── exit / lifecycle lines classify as Exit, never a Tauri event ───────

    #[test]
    fn exit_line_classifies_as_exit_with_reason() {
        // The reader loop turns `exit` into `voice-session-ended`; the single
        // parse yields the stable reason string directly.
        assert_eq!(
            classify_line(r#"{"event":"exit","reason":"stdin_eof"}"#).unwrap(),
            LineClass::Exit("stdin_eof".to_string())
        );
        assert_eq!(
            classify_line(r#"{"event":"exit","reason":"dismissed"}"#).unwrap(),
            LineClass::Exit("dismissed".to_string())
        );
        assert_eq!(
            classify_line(r#"{"event":"exit","reason":"error"}"#).unwrap(),
            LineClass::Exit("error".to_string())
        );
    }

    #[test]
    fn exit_line_without_reason_defaults_to_stdin_eof() {
        assert_eq!(
            classify_line(r#"{"event":"exit"}"#).unwrap(),
            LineClass::Exit("stdin_eof".to_string())
        );
    }

    // ── End classification: a child that never started must say why ────────
    //
    // The bug these cover: a sidecar staged weeks earlier was rejected by a
    // newer database ("migration 29 was previously applied but is missing in
    // the resolved migrations"), exited 1, and emitted ZERO NDJSON lines. The
    // shell reported "crashed" with an exit code and dropped the one line that
    // explained it, because `out!` is a no-op under `--json-events` and the
    // stderr stream was logged at debug.

    fn tail(lines: &[&str]) -> VecDeque<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_child_that_never_readied_failed_to_start_and_carries_stderr() {
        let (reason, detail) = classify_end(
            None,
            false,
            &tail(&[
                "  Goose in a Pond 0.1.0 — voice",
                "Error: migration 29 was previously applied but is missing in the resolved migrations",
            ]),
        );
        assert_eq!(reason, end_reason::FAILED_TO_START);
        let detail = detail.expect("a startup failure must carry the child's own explanation");
        assert!(
            detail.contains("migration 29"),
            "the cause must survive into the payload, got: {detail}"
        );
    }

    #[test]
    fn a_readied_session_that_dies_is_crashed_and_carries_no_stderr() {
        // After `ready` the child reports troubles as NDJSON `error` events, so
        // attaching stderr here would duplicate a better signal with noise.
        let (reason, detail) = classify_end(None, true, &tail(&["some later log line"]));
        assert_eq!(reason, end_reason::CRASHED);
        assert_eq!(detail, None);
    }

    #[test]
    fn a_clean_exit_line_wins_over_both() {
        // Sending `exit` IS the definition of a clean shutdown; a child that
        // says so before ever readying still exited cleanly, not fatally.
        let (reason, detail) = classify_end(Some("stdin_eof"), false, &tail(&["noise"]));
        assert_eq!(reason, "stdin_eof");
        assert_eq!(detail, None);
    }

    #[test]
    fn a_silent_startup_failure_reports_no_detail_rather_than_empty_string() {
        // An empty/blank tail must not become `detail: ""` — the frontend
        // branches on absence to choose its "without reporting a reason"
        // wording, and "" would render as a message with nothing after it.
        let (reason, detail) = classify_end(None, false, &tail(&["   ", ""]));
        assert_eq!(reason, end_reason::FAILED_TO_START);
        assert_eq!(detail, None);
    }

    // ── Robustness: bad lines never crash, they error for the caller to log ─

    #[test]
    fn empty_line_is_skipped() {
        assert_eq!(classify_line("").unwrap(), LineClass::Skip);
        assert_eq!(classify_line("   ").unwrap(), LineClass::Skip);
    }

    #[test]
    fn non_json_line_is_an_error_not_a_panic() {
        let err = classify_line("this is a banner line, not JSON").unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    #[test]
    fn json_without_event_field_is_an_error() {
        let err = classify_line(r#"{"state":"wait"}"#).unwrap_err();
        assert!(err.contains("missing string `event`"), "got: {err}");
    }

    #[test]
    fn unknown_event_kind_is_an_error() {
        let err = classify_line(r#"{"event":"heartbeat"}"#).unwrap_err();
        assert!(err.contains("unknown event kind"), "got: {err}");
    }

    #[test]
    fn missing_string_fields_default_to_empty() {
        // A `transcript` with no `text` field still maps, with an empty string,
        // rather than erroring — the reader stays resilient.
        let ev = event(r#"{"event":"transcript"}"#);
        assert_eq!(ev.payload, json!({ "text": "" }));
    }

    #[test]
    fn leading_and_trailing_whitespace_is_tolerated() {
        let ev = event("  {\"event\":\"state\",\"state\":\"speak\"}  \n");
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

    // ── Pidfile identity validation (kill -9 orphan recovery, finding 53) ──

    #[test]
    fn cmdline_matches_only_a_pond_server_chat_process() {
        // The exact production invocation must match.
        assert!(cmdline_is_voice_child(
            "/opt/app/pond-server chat --voice --json-events --session-id abc"
        ));
        // A bare macOS bundle sidecar path with the chat subcommand matches.
        assert!(cmdline_is_voice_child(
            "/Applications/Goose In A Pond.app/Contents/MacOS/pond-server chat --voice"
        ));
        // And the pre-rename invocation still matches, because orphan recovery
        // has to reap a child spawned by the shell that was running before an
        // upgrade. The matcher keys on the binary and the subcommand, never on
        // the flags, which is what makes that survivable.
        assert!(cmdline_is_voice_child(
            "/opt/app/pond-server chat --input whisper --json-events --session-id abc"
        ));
    }

    #[test]
    fn cmdline_does_not_match_the_serve_process_or_unrelated_pids() {
        // The dashboard server (serve) must NOT be reaped as a voice child.
        assert!(!cmdline_is_voice_child(
            "/opt/app/pond-server serve --port 4000"
        ));
        // A `chat` substring inside another token is not the chat subcommand.
        assert!(!cmdline_is_voice_child(
            "/usr/bin/pond-server-chatterbox serve"
        ));
        // Unrelated processes never match.
        assert!(!cmdline_is_voice_child("/usr/bin/node /some/other/app.js"));
        assert!(!cmdline_is_voice_child(""));
        // `chat` alone (no pond-server binary token) must not match a reused pid
        // running some other program that happens to take a `chat` argument.
        assert!(!cmdline_is_voice_child("/usr/bin/irc-client chat"));
    }

    #[test]
    fn read_pidfile_parses_a_valid_pid_and_rejects_junk() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("giap-pidfile-test-{}.pid", std::process::id()));

        // Valid pid (with surrounding whitespace).
        std::fs::write(&path, "  12345\n").unwrap();
        assert_eq!(read_pidfile(&path), Some(12345));

        // Malformed contents → None (never a wild kill target).
        std::fs::write(&path, "not-a-pid").unwrap();
        assert_eq!(read_pidfile(&path), None);

        // Empty file → None.
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_pidfile(&path), None);

        std::fs::remove_file(&path).ok();

        // Missing file → None.
        assert_eq!(read_pidfile(&path), None);
    }

    #[test]
    fn per_user_token_is_filesystem_safe() {
        // Whatever the environment yields, the token must be a safe filename
        // component (no path separators or spaces).
        let token = per_user_token();
        assert!(!token.is_empty());
        assert!(
            token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "token not filesystem-safe: {token:?}"
        );
    }

    #[test]
    fn sanitize_token_replaces_unsafe_chars() {
        assert_eq!(sanitize_token("jerry"), "jerry");
        assert_eq!(sanitize_token("a b/c.d"), "a_b_c_d");
        assert_eq!(sanitize_token("Ünïcode"), "_n_code");
    }

    #[test]
    fn pidfile_write_read_remove_round_trips() {
        // The manager writes the pidfile at spawn and removes it on reap; prove
        // the primitives are consistent against the real per-user path.
        remove_pidfile(); // start clean
        write_pidfile(4242);
        assert_eq!(read_pidfile(&pidfile_path()), Some(4242));
        remove_pidfile();
        assert_eq!(read_pidfile(&pidfile_path()), None);
        // Idempotent: a second remove is a no-op, never an error/panic.
        remove_pidfile();
    }

    #[test]
    fn a_dead_pid_is_not_treated_as_a_voice_child() {
        // Pid 0 is never a live pond-server chat process, so identity validation
        // must refuse it — a reused/dead pid is never a kill target. `None`
        // (indeterminate, e.g. `ps` unavailable) is acceptable; only a
        // confirmed `Some(true)` match would be a bug.
        assert_ne!(pid_is_voice_child(0), Some(true));
    }
}
