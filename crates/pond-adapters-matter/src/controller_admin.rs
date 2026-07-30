//! [`MatterController`] — the [`MatterControllerAdmin`] over a local
//! python-matter-server process.
//!
//! Identifying the controller is the whole difficulty. GIAP knows about a
//! controller it spawned this run, because it holds the child handle. It knows
//! nothing about one that was already listening — which is the case that goes
//! wrong, because a controller left over from an earlier Pond can be up for days
//! and lose its mDNS state while still answering its port.
//!
//! So the process is identified by the command it was started with: a
//! `matter_server.server` whose `--storage-path` is the storage directory under
//! *this* data dir is GIAP's own, whoever started it. Anything else on the port
//! — the user's own server, a container, a different Pond's data dir — is
//! external and left strictly alone. That is a stronger ownership test than the
//! loopback check alone, which cannot tell GIAP's controller from someone else's
//! that happens to be on localhost.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::matter_controller::{
    ControllerOrigin, ControllerStatus, MatterControllerAdmin,
};
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::server_setup::{ensure_running, is_running, storage_dir};

/// The controller child GIAP currently owns. Shared because a restart replaces
/// it, and shutdown must kill whichever one is live by then — `kill_on_drop`
/// does not fire on the signal path.
pub type SharedControllerChild = Arc<Mutex<Option<Child>>>;

/// How long to wait for the old controller to release the port before spawning
/// a replacement. Generous: the process flushes its fabric storage on the way
/// out, and binding a port still held would fail the restart for no good reason.
const PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a freshly spawned controller gets to start listening. The CHIP stack
/// is slow to initialise; this matches the startup path.
const RESTART_READY_TIMEOUT: Duration = Duration::from_secs(120);

pub struct MatterController {
    /// `None` when the configured URL is remote: not GIAP's to inspect or touch.
    port: Option<u16>,
    data_dir: PathBuf,
    child: SharedControllerChild,
}

impl MatterController {
    pub fn new(data_dir: PathBuf, port: Option<u16>, child: SharedControllerChild) -> Self {
        Self {
            port,
            data_dir,
            child,
        }
    }

    /// The child cell, so shutdown can kill the controller that is live at the
    /// time rather than the one GIAP happened to spawn first.
    pub fn child_handle(&self) -> SharedControllerChild {
        self.child.clone()
    }

    /// The local controller process belonging to this data dir, as
    /// `(pid, uptime_secs)`.
    ///
    /// Matched on the command line rather than the port, because the port only
    /// says "something is listening". Reading `--storage-path` is what proves
    /// the process is serving *this* Pond's fabric.
    fn find_process(data_dir: &Path) -> Option<(u32, u64)> {
        let wanted = storage_dir(data_dir);
        let sys = sysinfo::System::new_all();

        for (pid, proc_) in sys.processes() {
            let args: Vec<String> = proc_
                .cmd()
                .iter()
                .map(|a| a.to_string_lossy().to_string())
                .collect();
            if !args.iter().any(|a| a.contains("matter_server.server")) {
                continue;
            }
            // The storage path is the ownership proof. Compared as a path so a
            // trailing slash or a differently-spelled but equal path matches.
            let owns_our_storage = args
                .iter()
                .any(|a| Path::new(a.trim_end_matches('/')) == wanted.as_path());
            if !owns_our_storage {
                continue;
            }

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let uptime = now.saturating_sub(proc_.start_time());
            return Some((pid.as_u32(), uptime));
        }
        None
    }

    /// Stop the controller, whether GIAP holds its handle or only its pid.
    async fn stop(&self, pid: u32) -> Result<()> {
        // Prefer the handle: it reaps the child, so no zombie is left behind.
        if let Some(mut child) = self.child.lock().await.take() {
            if child.id() == Some(pid) {
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Ok(());
            }
            // Not the process we mean to stop — put it back rather than lose it.
            *self.child.lock().await = Some(child);
        }

        // Adopted from an earlier run: signal it by pid. TERM rather than KILL so
        // it can flush the fabric storage on the way out.
        let sys = sysinfo::System::new_all();
        let proc_ = sys
            .process(sysinfo::Pid::from_u32(pid))
            .ok_or_else(|| anyhow!("the controller process ({pid}) is already gone"))?;
        if !proc_.kill_with(sysinfo::Signal::Term).unwrap_or(false) {
            proc_.kill();
        }
        Ok(())
    }

    /// Wait for the port to be free, so the replacement can bind it.
    async fn await_port_release(port: u16) -> Result<()> {
        let deadline = tokio::time::Instant::now() + PORT_RELEASE_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if !is_running(port).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(anyhow!(
            "the old controller is still holding port {port} after {PORT_RELEASE_TIMEOUT:?}"
        ))
    }
}

#[async_trait]
impl MatterControllerAdmin for MatterController {
    async fn status(&self) -> ControllerStatus {
        let Some(_port) = self.port else {
            // A remote URL: someone else's server, by definition.
            return ControllerStatus::external();
        };

        let data_dir = self.data_dir.clone();
        // `new_all` walks the process table, so keep it off the async worker.
        let found = tokio::task::spawn_blocking(move || Self::find_process(&data_dir))
            .await
            .unwrap_or(None);

        let Some((pid, uptime)) = found else {
            // Listening, but not a controller serving this data dir.
            return ControllerStatus::external();
        };

        // Ours-this-run only if it is the very child we are holding.
        let started_by_us = self
            .child
            .lock()
            .await
            .as_ref()
            .and_then(Child::id)
            .is_some_and(|held| held == pid);
        let origin = if started_by_us {
            ControllerOrigin::StartedByPond
        } else {
            ControllerOrigin::AdoptedFromEarlierRun
        };
        ControllerStatus::owned(origin, pid, uptime)
    }

    async fn restart(&self) -> Result<()> {
        let status = self.status().await;
        if !status.restartable {
            return Err(anyhow!(
                "this Matter controller is {} , so GIAP will not restart it. \
                 Restart it wherever you started it.",
                status.origin.describe()
            ));
        }
        let port = self
            .port
            .ok_or_else(|| anyhow!("no local controller port is configured"))?;
        let pid = status
            .pid
            .ok_or_else(|| anyhow!("the controller process could not be identified"))?;

        tracing::info!(pid, port, "matter: restarting the controller");
        self.stop(pid).await?;
        Self::await_port_release(port).await?;

        // The fabric is in the storage directory, not the process, so the fresh
        // controller comes back with every commissioned node intact.
        let child = ensure_running(&self.data_dir, port, RESTART_READY_TIMEOUT).await?;
        *self.child.lock().await = child;

        // No reconnect needed here: dropping the socket wakes the supervisor,
        // which reconnects with backoff and swaps the shared client that both
        // device control and commissioning read through.
        tracing::info!(port, "matter: controller restarted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin(port: Option<u16>) -> MatterController {
        MatterController::new(
            PathBuf::from("/nonexistent/giap-test-data-dir"),
            port,
            Arc::new(Mutex::new(None)),
        )
    }

    #[tokio::test]
    async fn a_remote_url_is_external_and_refused() {
        let a = admin(None);
        let s = a.status().await;
        assert_eq!(s.origin, ControllerOrigin::External);
        assert!(!s.restartable);

        let err = a.restart().await.unwrap_err().to_string();
        // The refusal has to say whose controller it is, not just "no".
        assert!(err.contains("not managed by this Pond"), "{err}");
    }

    #[tokio::test]
    async fn a_loopback_port_with_no_matching_process_is_external_not_owned() {
        // Nothing is serving this data dir, so there is nothing GIAP may claim —
        // a controller on the port belongs to someone else.
        let s = admin(Some(5580)).status().await;
        assert_eq!(s.origin, ControllerOrigin::External);
        assert!(!s.restartable);
    }

    #[tokio::test]
    async fn the_process_match_requires_our_own_storage_path() {
        // A controller started with a different data dir must not be adopted,
        // which is what stops one Pond restarting another Pond's controller.
        assert_eq!(
            MatterController::find_process(Path::new("/nonexistent/other-pond")),
            None
        );
    }

    #[tokio::test]
    async fn waiting_on_a_free_port_returns_at_once() {
        let free = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        assert!(MatterController::await_port_release(free).await.is_ok());
    }
}
