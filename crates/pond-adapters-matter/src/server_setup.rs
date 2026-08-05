//! Local matter-server lifecycle — GIAP installs and runs the controller itself.
//!
//! The Matter adapter talks to a [python-matter-server] over WebSocket, but that
//! controller is a Python process someone has to install. Requiring the user to
//! hand-build a Python stack before a single bulb works is the wrong first-run
//! experience for an appliance, so when Matter is enabled and nothing is serving
//! the configured port, GIAP sets one up:
//!
//! 1. probe the port — if a controller is already there (the user runs their
//!    own, or a previous Pond left one up), use it and change nothing;
//! 2. otherwise create a private venv under the data dir and `pip install` a
//!    **pinned** python-matter-server;
//! 3. spawn it with its storage inside the data dir, so the commissioned fabric
//!    (and every paired device) survives restarts and upgrades;
//! 4. wait for the port to accept connections before the adapter connects.
//!
//! Only loopback URLs are auto-started: a remote `matter_ws_url` is someone
//! else's server and GIAP must not try to manage it.
//!
//! [python-matter-server]: https://github.com/home-assistant-libs/python-matter-server

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

/// The controller GIAP started, if any. Shared rather than owned outright
/// because two places have to agree on which process is current: the reconciler
/// kills it on teardown, and the reconnect supervisor replaces it when it finds
/// the process dead. `None` means GIAP did not start one — the user runs their
/// own controller, or Matter is off.
pub type SharedServerChild = Arc<Mutex<Option<Child>>>;

/// Pinned: an unpinned install would let an upstream release change what runs
/// on the user's home network without review.
pub const MATTER_SERVER_SPEC: &str = "python-matter-server[server]==8.1.2";

/// python-matter-server requires Python >= 3.12.
pub const MIN_PYTHON: (u32, u32) = (3, 12);

/// Interpreters to try, newest first.
const PYTHON_CANDIDATES: &[&str] = &["python3.13", "python3.12", "python3"];

/// Parse `"Python 3.12.13"` into `(3, 12)`. Pure so the version gate is
/// testable without an interpreter.
pub fn parse_python_version(output: &str) -> Option<(u32, u32)> {
    let version = output.split_whitespace().nth(1)?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Tuple ordering gives the right comparison: (3,13) >= (3,12) >= MIN.
pub fn meets_min_python(version: (u32, u32)) -> bool {
    version >= MIN_PYTHON
}

/// The loopback port to auto-start for, or `None` when the URL points at
/// another host — GIAP only manages a controller it runs itself.
pub fn local_port_from_ws_url(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("ws://")
        .or_else(|| url.strip_prefix("wss://"))?;
    let authority = rest.split('/').next()?;
    let (host, port) = authority.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
        return None;
    }
    port.parse().ok()
}

/// Everything GIAP owns for the controller lives under one directory.
pub fn controller_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("matter-server")
}
pub fn venv_dir(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("venv")
}
pub fn venv_python(data_dir: &Path) -> PathBuf {
    venv_dir(data_dir).join("bin").join("python")
}
/// The fabric store — commissioned nodes live here, so it must be stable.
pub fn storage_dir(data_dir: &Path) -> PathBuf {
    controller_dir(data_dir).join("storage")
}

/// Is something accepting connections on the controller port?
pub async fn is_running(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

/// First interpreter on PATH meeting [`MIN_PYTHON`].
async fn find_python() -> Result<PathBuf> {
    for candidate in PYTHON_CANDIDATES {
        let Ok(out) = Command::new(candidate).arg("--version").output().await else {
            continue;
        };
        // Older interpreters print the version on stderr.
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr)
        } else {
            String::from_utf8_lossy(&out.stdout)
        };
        if parse_python_version(&text).is_some_and(meets_min_python) {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err(anyhow!(
        "no Python {}.{}+ found on PATH — python-matter-server needs it. \
         Install one (macOS: `brew install python@3.12`; Debian/Jetson: \
         `apt install python3.12 python3.12-venv`) and restart, or run your own \
         matter-server and point matter_ws_url at it.",
        MIN_PYTHON.0,
        MIN_PYTHON.1
    ))
}

/// Create the venv and install the pinned controller. Idempotent: if the module
/// already imports in the venv, this is a no-op.
async fn ensure_installed(data_dir: &Path) -> Result<()> {
    let py = venv_python(data_dir);
    if py.exists() {
        if let Ok(out) = Command::new(&py)
            .args(["-c", "import matter_server"])
            .output()
            .await
        {
            if out.status.success() {
                return Ok(());
            }
        }
    }

    let system_python = find_python().await?;
    let venv = venv_dir(data_dir);
    tracing::info!(path = %venv.display(), "matter: creating controller venv");
    std::fs::create_dir_all(controller_dir(data_dir))
        .with_context(|| format!("creating {}", controller_dir(data_dir).display()))?;

    let status = Command::new(&system_python)
        .arg("-m")
        .arg("venv")
        .arg(&venv)
        .status()
        .await
        .context("running python -m venv")?;
    if !status.success() {
        return Err(anyhow!("python -m venv failed with status {status}"));
    }

    tracing::info!(spec = MATTER_SERVER_SPEC, "matter: installing controller");
    let status = Command::new(&py)
        .args(["-m", "pip", "install", "--disable-pip-version-check"])
        .arg(MATTER_SERVER_SPEC)
        .status()
        .await
        .context("running pip install")?;
    if !status.success() {
        return Err(anyhow!(
            "pip install {MATTER_SERVER_SPEC} failed with status {status}"
        ));
    }
    Ok(())
}

/// Spawn the controller. The child is `kill_on_drop`, so holding the handle ties
/// its lifetime to pond-server: drop it and the controller goes away too.
fn spawn_server(data_dir: &Path, port: u16) -> Result<Child> {
    let storage = storage_dir(data_dir);
    std::fs::create_dir_all(&storage).with_context(|| format!("creating {}", storage.display()))?;

    // Keep the controller's own output for diagnosis; a silent failure here is
    // otherwise very hard to debug from the Pond side.
    let log_path = controller_dir(data_dir).join("matter-server.log");
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("creating {}", log_path.display()))?;
    let log_err = log.try_clone().context("cloning log handle")?;

    Command::new(venv_python(data_dir))
        .arg("-m")
        .arg("matter_server.server")
        .args(["--port", &port.to_string()])
        .arg("--storage-path")
        .arg(&storage)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true)
        .spawn()
        .context("spawning matter_server.server")
}

/// Ensure a controller is reachable on `port`, installing and starting one if
/// needed. Returns the child handle when GIAP started it (the caller must keep
/// it alive), or `None` when an existing server was reused.
pub async fn ensure_running(
    data_dir: &Path,
    port: u16,
    ready_timeout: Duration,
) -> Result<Option<Child>> {
    if is_running(port).await {
        tracing::info!(port, "matter: controller already running; reusing it");
        return Ok(None);
    }

    tracing::info!(port, "matter: no controller found; setting one up");
    ensure_installed(data_dir).await?;
    let child = spawn_server(data_dir, port)?;

    let deadline = Instant::now() + ready_timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if is_running(port).await {
            tracing::info!(port, "matter: controller ready");
            return Ok(Some(child));
        }
    }
    Err(anyhow!(
        "matter-server did not start listening on port {port} within {:?} — see {}",
        ready_timeout,
        controller_dir(data_dir).join("matter-server.log").display()
    ))
}

/// What a revival attempt actually did, so the caller can tell "the controller
/// was dead and is back" from "the controller is fine, the fault is elsewhere".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revival {
    /// The URL points at another host: someone else's controller, which GIAP
    /// must never install for or spawn.
    NotLocal,
    /// A controller was already listening; nothing was installed or spawned.
    Reused,
    /// The controller was gone and a fresh one is now accepting connections.
    Restarted,
}

/// Re-run [`ensure_running`] for a controller GIAP manages itself, parking any
/// freshly spawned child in `child` so teardown still kills the process that is
/// actually running.
///
/// The reconnect supervisor calls this once reconnecting alone has stopped
/// working: a controller whose process has exited will never answer a
/// reconnect, no matter how long the loop runs. Idempotent by construction —
/// [`ensure_running`] reuses a live port — so it is safe to call repeatedly,
/// and it never puts a second controller onto a fabric that already has one.
pub async fn revive_local_controller(
    data_dir: &Path,
    url: &str,
    child: &SharedServerChild,
    ready_timeout: Duration,
) -> Result<Revival> {
    let Some(port) = local_port_from_ws_url(url) else {
        return Ok(Revival::NotLocal);
    };

    match ensure_running(data_dir, port, ready_timeout).await? {
        // Storing the new handle drops the dead one, which is harmless:
        // `kill_on_drop` against an already-exited process is a no-op, and
        // teardown now kills the controller that is really running.
        Some(fresh) => {
            *child.lock().await = Some(fresh);
            Ok(Revival::Restarted)
        }
        // Something is serving the port — leave the stored handle alone rather
        // than claiming ownership of a process GIAP did not start.
        None => Ok(Revival::Reused),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A remote controller is another machine's process. Revival runs on every
    /// failing URL, so this is the guard that stops GIAP installing a Python
    /// stack and spawning a controller for a server it does not own.
    #[tokio::test]
    async fn revival_never_touches_a_remote_controller() {
        let child: SharedServerChild = Arc::new(Mutex::new(None));

        let outcome = revive_local_controller(
            // Unreachable on purpose: nothing here may be read or written.
            Path::new("/nonexistent"),
            "ws://192.168.1.50:5580/ws",
            &child,
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(outcome, Revival::NotLocal);
        assert!(child.lock().await.is_none(), "no child for a remote server");
    }

    /// A controller that is still listening is reused, never restarted. The
    /// supervisor retries revival for as long as reconnects keep failing, and a
    /// second controller on a live fabric would be worse than the outage it was
    /// trying to fix.
    #[tokio::test]
    async fn revival_reuses_a_controller_that_is_still_listening() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let child: SharedServerChild = Arc::new(Mutex::new(None));

        let outcome = revive_local_controller(
            // The port answers, so setup returns before the data dir is used.
            Path::new("/nonexistent"),
            &format!("ws://127.0.0.1:{port}/ws"),
            &child,
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(outcome, Revival::Reused);
        assert!(
            child.lock().await.is_none(),
            "reusing a live controller must not claim ownership of it"
        );
    }

    #[test]
    fn parses_python_versions_and_gates_on_3_12() {
        assert_eq!(parse_python_version("Python 3.12.13"), Some((3, 12)));
        assert_eq!(parse_python_version("Python 3.9.6"), Some((3, 9)));
        assert_eq!(parse_python_version("Python 3.13.0rc1"), Some((3, 13)));
        assert_eq!(parse_python_version("not a version"), None);
        assert_eq!(parse_python_version(""), None);

        assert!(meets_min_python((3, 12)));
        assert!(meets_min_python((3, 13)));
        assert!(meets_min_python((4, 0)));
        // The macOS system Python is 3.9 — it must be rejected, not used.
        assert!(!meets_min_python((3, 9)));
        assert!(!meets_min_python((2, 7)));
    }

    #[test]
    fn only_loopback_urls_are_auto_started() {
        assert_eq!(local_port_from_ws_url("ws://127.0.0.1:5580/ws"), Some(5580));
        assert_eq!(local_port_from_ws_url("ws://localhost:5580/ws"), Some(5580));
        assert_eq!(local_port_from_ws_url("ws://127.0.0.1:6000"), Some(6000));

        // Someone else's controller: never auto-managed.
        assert_eq!(local_port_from_ws_url("ws://192.168.1.50:5580/ws"), None);
        assert_eq!(local_port_from_ws_url("ws://matter.local:5580/ws"), None);
        // Malformed / portless.
        assert_eq!(local_port_from_ws_url("http://127.0.0.1:5580"), None);
        assert_eq!(local_port_from_ws_url("ws://127.0.0.1/ws"), None);
    }

    #[test]
    fn controller_paths_are_nested_under_the_data_dir() {
        let data = Path::new("/var/lib/giap");
        assert_eq!(
            venv_dir(data),
            Path::new("/var/lib/giap/matter-server/venv")
        );
        assert_eq!(
            storage_dir(data),
            Path::new("/var/lib/giap/matter-server/storage")
        );
        // Storage must live under the data dir so the commissioned fabric
        // survives restarts.
        assert!(storage_dir(data).starts_with(data));
    }

    #[tokio::test]
    async fn is_running_detects_a_live_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_running(port).await, "a bound port must read as running");

        drop(listener);
        // A port with nothing on it must not read as running (that is what
        // decides whether GIAP spawns its own controller).
        let free = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        assert!(!is_running(free).await);
    }
}
