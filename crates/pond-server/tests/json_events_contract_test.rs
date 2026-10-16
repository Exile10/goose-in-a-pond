//! Spawns `pond-server chat --json-events --provider mock` (offline echo agent) and checks its
//! stdout is contract NDJSON ending in `exit` on stdin EOF. Not run in CI (`cargo check` only).

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Each test spawns a voice child, and the voice lock admits one per device.
static ONE_CHILD_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_child_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    ONE_CHILD_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Parsed stdout events up to `exit`, or until `deadline` elapses.
fn read_events_until_exit(stdout: std::process::ChildStdout, deadline: Duration) -> Vec<Value> {
    let start = Instant::now();
    let mut reader = BufReader::new(stdout);
    let mut events = Vec::new();
    let mut line = String::new();
    loop {
        if start.elapsed() > deadline {
            break;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if trimmed.is_empty() {
                    continue;
                }
                // EVERY line on stdout must be valid JSON — no banners/prompts.
                let value: Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
                    panic!("non-JSON line on stdout (contract violation): {trimmed:?} ({e})")
                });
                let is_exit = value.get("event").and_then(Value::as_str) == Some("exit");
                events.push(value);
                if is_exit {
                    break;
                }
            }
            Err(e) => panic!("error reading child stdout: {e}"),
        }
    }
    events
}

#[test]
fn json_events_stdin_turn_emits_contract_ndjson() {
    let _one = one_child_at_a_time();
    let tmp = std::env::temp_dir().join(format!(
        "giap-json-events-contract-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::create_dir_all(&tmp).expect("create temp data dir");

    let bin = env!("CARGO_BIN_EXE_pond-server");
    let mut child = Command::new(bin)
        .args([
            "chat",
            "--json-events",
            "--session-id",
            "test-contract",
            "--provider",
            "mock",
        ])
        .env("POND_DATA_DIR", &tmp)
        .env("SQLX_OFFLINE", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn pond-server binary");

    // One utterance then EOF: the first listen() runs the turn, the second sees EOF and exits.
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        writeln!(stdin, "what is the capital of france").expect("write to child stdin");
        stdin.flush().ok();
        // `stdin` drops here → pipe closes → child's next read_line() gets EOF.
    }

    let stdout = child.stdout.take().expect("child stdout");
    let events = read_events_until_exit(stdout, Duration::from_secs(60));

    // The child should have exited on its own; reap it (kill as a safety net).
    let _ = child.kill();
    let _ = child.wait();

    let names: Vec<&str> = events
        .iter()
        .map(|e| e.get("event").and_then(Value::as_str).unwrap_or("<none>"))
        .collect();

    assert_eq!(
        events
            .first()
            .and_then(|e| e.get("event"))
            .and_then(Value::as_str),
        Some("ready"),
        "first event must be ready; got sequence: {names:?}"
    );
    assert_eq!(
        events[0].get("session_id").and_then(Value::as_str),
        Some("test-contract"),
        "ready must carry the --session-id"
    );

    assert!(
        names.contains(&"transcript"),
        "a transcript event must appear; got: {names:?}"
    );
    assert!(
        names.contains(&"token"),
        "at least one token event must appear; got: {names:?}"
    );

    let state_values: Vec<&str> = events
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("state"))
        .filter_map(|e| e.get("state").and_then(Value::as_str))
        .collect();
    for needed in ["wait", "listen", "thinking", "speak"] {
        assert!(
            state_values.contains(&needed),
            "state '{needed}' must appear; got states: {state_values:?}"
        );
    }
    let idx = |s: &str| state_values.iter().position(|v| *v == s).unwrap();
    assert!(
        idx("wait") < idx("listen")
            && idx("listen") < idx("thinking")
            && idx("thinking") < idx("speak"),
        "states must progress wait→listen→thinking→speak; got: {state_values:?}"
    );

    let turn_complete_count = names.iter().filter(|n| **n == "turn_complete").count();
    assert_eq!(
        turn_complete_count, 1,
        "exactly one turn_complete on completion; got: {names:?}"
    );
    let tc = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("turn_complete"))
        .unwrap();
    assert_eq!(
        tc.get("session_id").and_then(Value::as_str),
        Some("test-contract")
    );

    let last = events.last().expect("at least one event");
    assert_eq!(
        last.get("event").and_then(Value::as_str),
        Some("exit"),
        "last event must be exit; got: {names:?}"
    );
    assert_eq!(
        last.get("reason").and_then(Value::as_str),
        Some("stdin_eof"),
        "exit reason must be stdin_eof after closing stdin"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

/// The child's stdout, parsed line by line on a reader thread so waits can time out.
struct EventStream {
    rx: std::sync::mpsc::Receiver<Value>,
    seen: Vec<Value>,
}

impl EventStream {
    fn new(stdout: std::process::ChildStdout) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(&line).unwrap_or_else(|e| {
                    panic!("non-JSON line on stdout (contract violation): {line:?} ({e})")
                });
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            rx,
            seen: Vec::new(),
        }
    }

    /// Reads until an event named `name`, returning it; panics after `deadline`.
    fn until(&mut self, name: &str, deadline: Duration) -> Value {
        let end = Instant::now() + deadline;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(value) => {
                    self.seen.push(value.clone());
                    if value.get("event").and_then(Value::as_str) == Some(name) {
                        return value;
                    }
                }
                Err(_) => panic!("no `{name}` event within {deadline:?}; saw {:?}", self.seen),
            }
        }
    }

    fn names(&self) -> Vec<&str> {
        self.seen
            .iter()
            .map(|e| e.get("event").and_then(Value::as_str).unwrap_or("<none>"))
            .collect()
    }
}

fn spawn_mock_child(session_id: &str, data_dir: &std::path::Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_pond-server"))
        .args([
            "chat",
            "--json-events",
            "--session-id",
            session_id,
            "--provider",
            "mock",
        ])
        .env("POND_DATA_DIR", data_dir)
        .env("SQLX_OFFLINE", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn pond-server binary")
}

fn temp_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "giap-json-events-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp data dir");
    dir
}

/// The desktop shell's grace period before it escalates a stop to SIGKILL.
const SHELL_GRACE: Duration = Duration::from_secs(3);

#[test]
fn a_stop_command_ends_the_session_with_exit_zero_inside_the_shell_grace() {
    let _one = one_child_at_a_time();
    let tmp = temp_data_dir("stop");
    let mut child = spawn_mock_child("test-stop", &tmp);
    let mut stdin = child.stdin.take().expect("child stdin");
    let mut events = EventStream::new(child.stdout.take().expect("child stdout"));

    events.until("ready", Duration::from_secs(60));
    // Listening on stdin for a typed turn: the state a stop most often arrives in.
    events.until("state", Duration::from_secs(10));

    // Stdin stays open: the command alone must end the session.
    let sent = Instant::now();
    writeln!(stdin, r#"{{"cmd":"stop"}}"#).expect("write stop");
    stdin.flush().ok();

    let exit = events.until("exit", SHELL_GRACE);
    assert_eq!(
        exit.get("reason").and_then(Value::as_str),
        Some("stopped"),
        "a stop command reports `stopped`; got {:?}",
        events.names()
    );

    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if sent.elapsed() > SHELL_GRACE {
            let _ = child.kill();
            panic!("the child outlived the shell's {SHELL_GRACE:?} grace after a stop");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "a stopped session exits 0; got {status}");
    drop(stdin);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_command_line_on_a_typed_stdin_is_never_a_turn() {
    let _one = one_child_at_a_time();
    let tmp = temp_data_dir("typed-commands");
    let mut child = spawn_mock_child("test-typed-commands", &tmp);
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        writeln!(stdin, r#"{{"cmd":"interrupt"}}"#).unwrap();
        writeln!(stdin, r#"{{"cmd":"unknown"}}"#).unwrap();
        writeln!(stdin, "what is the capital of france").unwrap();
        stdin.flush().ok();
        // EOF on a typed stdin ends input after the queued turn, as before commands existed.
    }
    let mut events = EventStream::new(child.stdout.take().expect("child stdout"));
    let exit = events.until("exit", Duration::from_secs(60));
    let _ = child.kill();
    let _ = child.wait();

    let transcripts: Vec<&str> = events
        .seen
        .iter()
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("transcript"))
        .filter_map(|e| e.get("text").and_then(Value::as_str))
        .collect();
    assert_eq!(
        transcripts,
        vec!["what is the capital of france"],
        "commands must not be heard as turns; got {:?}",
        events.names()
    );
    assert_eq!(
        events
            .names()
            .iter()
            .filter(|n| **n == "turn_complete")
            .count(),
        1
    );
    assert_eq!(
        exit.get("reason").and_then(Value::as_str),
        Some("stdin_eof")
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
