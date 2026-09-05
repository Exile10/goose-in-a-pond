//! [`MatterRuntime`] — the reconciling owner of everything Matter. One reconciler task owns
//! the mutable state and converges it toward the last value sent through
//! [`MatterRuntimePort::apply`] over a `watch` channel, so rapid toggles coalesce and two
//! reconciles never interleave; setup runs in a `select!` so a disable cancels an install.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::NotificationSender;
use pond_core::shared::ports::event_bus::EventBus;
use pond_core::user_data::ports::device_commissioning::DeviceCommissioningPort;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceDescription, DeviceState,
};
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::matter_runtime::{
    MatterConfig, MatterRuntimePort, MatterState, MatterStatus,
};
use tokio::sync::{watch, RwLock};
use tokio::task::JoinHandle;

use crate::bridge::{run_matter_supervisor, SupervisorConfig};
use crate::client::{MatterClient, MatterEvent};
use crate::commissioning::MatterCommissioner;
use crate::control::MatterDeviceControl;
use crate::notify::MatterNotifier;
use crate::protocol::{describe, is_matter_device_id};
use crate::server_setup::{ensure_running, local_port_from_ws_url, SharedServerChild};

/// How long to wait for a freshly installed controller to start listening.
/// Matches the budget the startup path used before this became reconcilable.
const CONTROLLER_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// How long `shutdown` waits for the reconciler to tear down before giving up.
/// Teardown is local work (abort a task, kill a child), so this is generous;
/// the bound only exists so a wedged reconciler cannot hang process exit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for a killed controller to actually exit.
///
/// Shorter than [`SHUTDOWN_TIMEOUT`], which this runs inside, so one child cannot spend the
/// whole budget. The signal is SIGKILL: a process still alive after this is wedged.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);

/// The live [`MatterDeviceControl`], or `None` while Matter is off or
/// unreachable. Read by [`SwitchableDeviceControl`] on every call.
type ControlCell = Arc<RwLock<Option<Arc<MatterDeviceControl>>>>;

/// What the runtime has been asked to converge to.
///
/// `nonce` makes every [`MatterRuntimePort::apply`] a distinct value so the watch channel
/// always wakes the reconciler; whether that causes work is decided against what is running.
#[derive(Clone, PartialEq, Eq)]
struct Desired {
    url: String,
    /// Whether the controller should be asked for a Bluetooth transport. A
    /// change to it restarts the controller, because it is a spawn argument.
    ble: bool,
    shutdown: bool,
    nonce: u64,
}

/// Reconciles the Matter integration toward the requested state.
pub struct MatterRuntime {
    notifier: MatterNotifier,
    desired: watch::Sender<Desired>,
    status: Arc<RwLock<MatterStatus>>,
    commissioner: Arc<RwLock<Option<Arc<dyn DeviceCommissioningPort>>>>,
    control: ControlCell,
    /// Signalled by the reconciler once it has torn down and stopped.
    stopped: Arc<tokio::sync::Notify>,
    nonce: std::sync::atomic::AtomicU64,
}

impl MatterRuntime {
    /// Build the runtime and start its reconciler. Nothing happens until the
    /// first [`apply`](MatterRuntimePort::apply) — construction is inert, so
    /// wiring it up costs nothing when Matter is off.
    pub fn new(
        data_dir: PathBuf,
        registry: Arc<dyn DeviceRegistry + Send + Sync>,
        bus: Arc<dyn EventBus>,
    ) -> Arc<Self> {
        let (desired, desired_rx) = watch::channel(Desired {
            url: String::new(),
            ble: false,
            shutdown: false,
            nonce: 0,
        });

        let notifier = MatterNotifier::new();
        let runtime = Arc::new(Self {
            notifier: notifier.clone(),
            desired,
            status: Arc::new(RwLock::new(MatterStatus::disabled())),
            commissioner: Arc::new(RwLock::new(None)),
            control: Arc::new(RwLock::new(None)),
            stopped: Arc::new(tokio::sync::Notify::new()),
            nonce: std::sync::atomic::AtomicU64::new(0),
        });

        tokio::spawn(reconcile_loop(
            desired_rx,
            Reconciler {
                data_dir,
                registry,
                bus,
                notifier,
                status: runtime.status.clone(),
                commissioner: runtime.commissioner.clone(),
                control: runtime.control.clone(),
                stopped: runtime.stopped.clone(),
            },
        ));

        runtime
    }

    /// Wait for the reconciler to reach a settled state, or for `timeout`, so the Matter
    /// startup lines land before the "listening" banner. `apply` is deliberately
    /// non-blocking because a first-run install takes minutes, so this is the bounded
    /// compromise: settle quickly, or give up and let the install continue in background.
    pub async fn settle(&self, timeout: Duration) -> MatterStatus {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.status().await;
            // `enabled` first: `apply` only SENDS to the watch channel, so for a
            // moment after it returns the reconciler has not woken and the status
            // is still the initial `Disabled`. Waiting for that acknowledgement is
            // what makes this a wait rather than a race.
            if status.enabled && !matches!(status.state, MatterState::Connecting) {
                return status;
            }
            if tokio::time::Instant::now() >= deadline {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Route the runtime's user-facing notifications through `sender`. Separate from
    /// construction because the notification stack is built after the runtime; see
    /// [`MatterNotifier`]. Call it before the first [`apply`](MatterRuntimePort::apply), or
    /// a first-run install finishes without the user ever being told it started.
    pub async fn attach_notifications(&self, sender: Arc<dyn NotificationSender>) {
        self.notifier.attach(sender).await;
    }

    /// A [`DeviceControlPort`] that follows this runtime: Matter while
    /// connected, `fallback` otherwise. Built once and cloned everywhere, so
    /// the agent and MCP wiring never need rebuilding when Matter is toggled.
    pub fn device_control(
        &self,
        fallback: Arc<dyn DeviceControlPort>,
    ) -> Arc<SwitchableDeviceControl> {
        Arc::new(SwitchableDeviceControl {
            matter: self.control.clone(),
            status: self.status.clone(),
            fallback,
        })
    }
}

#[async_trait]
impl MatterRuntimePort for MatterRuntime {
    fn apply(&self, config: MatterConfig) {
        let nonce = self
            .nonce
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        // A closed channel means the reconciler is gone (shutdown); dropping
        // the request is correct — there is nothing left to converge.
        let _ = self.desired.send(Desired {
            url: config.url,
            ble: config.ble,
            shutdown: false,
            nonce,
        });
    }

    async fn status(&self) -> MatterStatus {
        self.status.read().await.clone()
    }

    async fn commissioner(&self) -> Option<Arc<dyn DeviceCommissioningPort>> {
        self.commissioner.read().await.clone()
    }

    async fn shutdown(&self) {
        let nonce = self
            .nonce
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        let notified = self.stopped.notified();
        if self
            .desired
            .send(Desired {
                url: String::new(),
                ble: false,
                shutdown: true,
                nonce,
            })
            .is_err()
        {
            return; // reconciler already stopped
        }
        // Bounded: process exit must not hang on a wedged reconciler, even
        // though the cost of giving up is an orphaned controller.
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, notified)
            .await
            .is_err()
        {
            tracing::warn!("matter: runtime did not stop within the shutdown timeout");
        }
    }
}

/// The cells and dependencies the reconciler writes to. Split out so the loop
/// owns its mutable state (`Running`) separately from the shared handles.
struct Reconciler {
    data_dir: PathBuf,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    bus: Arc<dyn EventBus>,
    notifier: MatterNotifier,
    status: Arc<RwLock<MatterStatus>>,
    commissioner: Arc<RwLock<Option<Arc<dyn DeviceCommissioningPort>>>>,
    control: ControlCell,
    stopped: Arc<tokio::sync::Notify>,
}

/// What is currently running, owned by the reconciler task.
#[derive(Default)]
struct Running {
    /// The controller GIAP started, if any. Killed explicitly on teardown: `kill_on_drop`
    /// does not fire on the signal path, where the process exits without unwinding, so the
    /// controller would outlive the Pond. Shared with the supervisor, which parks a
    /// respawned handle here, so teardown kills the controller actually running.
    child: SharedServerChild,
    supervisor: Option<JoinHandle<()>>,
}

/// Converge toward the requested state until asked to shut down.
async fn reconcile_loop(mut rx: watch::Receiver<Desired>, r: Reconciler) {
    // What the running state was built from — compared against the request to
    // decide whether anything needs to change.
    let mut current = Desired {
        url: String::new(),
        ble: false,
        shutdown: false,
        nonce: 0,
    };
    let mut running = Running::default();

    loop {
        let want = rx.borrow_and_update().clone();

        if want.shutdown {
            teardown(&mut running, &r).await;
            r.stopped.notify_waiters();
            return;
        }

        // Nothing has been asked for yet. `nonce` is zero only in the channel's initial
        // value and every `apply` increments it, so this is the one reliable way to tell
        // "no request" from "a request that looks like the default". Without it the first
        // pass ran with an empty url and settled in `Unreachable` before anyone asked.
        if want.nonce == 0 {
            if rx.changed().await.is_err() {
                teardown(&mut running, &r).await;
                r.stopped.notify_waiters();
                return;
            }
            continue;
        }

        // Idempotent by comparison, not by flag: identical values while
        // connected are a no-op, but the same values while unreachable are a
        // retry — which is what the UI's retry affordance sends.
        let healthy = r.status.read().await.state.is_connected();
        // BLE too, not just the URL: it is an argument to the controller's own
        // process, so turning it on has no effect at all until that process is
        // replaced. Comparing the URL alone made the setting look like it saved and
        // did nothing until the next restart.
        let changed = want.url != current.url || want.ble != current.ble;
        if changed || !healthy {
            teardown(&mut running, &r).await;
            current = want.clone();

            {
                *r.status.write().await = MatterStatus {
                    enabled: true,
                    url: want.url.clone(),
                    state: MatterState::Connecting,
                };

                // Cancellable: a disable (or a URL edit) must not wait out a
                // controller install, which can take minutes. The half-built
                // child is dropped with it, and `kill_on_drop` reaps it.
                tokio::select! {
                    biased;
                    _ = rx.changed() => continue,
                    result = connect(&r, &want.url, want.ble) => match result {
                        Ok(connected) => {
                            running.child = connected.child;
                            running.supervisor = Some(connected.supervisor);
                            *r.commissioner.write().await = Some(connected.commissioner);
                            *r.control.write().await = Some(connected.control);
                            *r.status.write().await = MatterStatus {
                                enabled: true,
                                url: want.url.clone(),
                                state: MatterState::Connected,
                            };
                            tracing::info!(
                                target: "giap::trace",
                                kind = "matter_state_changed",
                                url = %want.url,
                                to = "connected",
                                "matter: controller connected"
                            );
                        }
                        Err(e) => {
                            // `describe` and not `to_string`: this is the only
                            // account of the failure the user gets, and Display
                            // shows the outermost context with the reason thrown
                            // away. Served by `GET /api/v1/matter/status`, so redacted.
                            let error = describe(&e);
                            tracing::warn!(
                                target: "giap::trace",
                                kind = "matter_state_changed",
                                url = %want.url,
                                to = "unreachable",
                                error = %error,
                                "matter: could not start or reach the controller"
                            );
                            *r.status.write().await = MatterStatus {
                                enabled: true,
                                url: want.url.clone(),
                                state: MatterState::Unreachable { error },
                            };
                        }
                    },
                }
            }
        }

        if rx.changed().await.is_err() {
            // Every sender dropped: the runtime handle is gone, so nothing can
            // ask for Matter again. Leave nothing running behind.
            teardown(&mut running, &r).await;
            r.stopped.notify_waiters();
            return;
        }
    }
}

/// Everything a successful connect produced.
struct Connected {
    child: SharedServerChild,
    supervisor: JoinHandle<()>,
    commissioner: Arc<dyn DeviceCommissioningPort>,
    control: Arc<MatterDeviceControl>,
}

/// Start a controller if this URL is ours to manage, connect to it, and put the
/// bridge under supervision.
async fn connect(r: &Reconciler, url: &str, ble: bool) -> Result<Connected> {
    // An install predating the Matter section could have been enabled with no
    // address. Named plainly rather than left to surface as an opaque WebSocket
    // parse failure — the fix is to fill the field in.
    if url.trim().is_empty() {
        anyhow::bail!("no Matter controller address is set");
    }

    // Only a loopback URL is GIAP's to install and run; anything else is
    // someone else's controller and is used as-is.
    let started = match local_port_from_ws_url(url) {
        Some(port) => {
            ensure_running(
                &r.data_dir,
                port,
                CONTROLLER_READY_TIMEOUT,
                &r.notifier,
                url,
                ble,
            )
            .await?
        }
        None => None,
    };
    let started_here = started.is_some();
    // One cell, two writers: this connect puts the first handle in, and the
    // supervisor replaces it if it ever has to restart the process.
    let child: SharedServerChild = Arc::new(tokio::sync::Mutex::new(started));

    // `started` is Some only when GIAP spawned the controller, which is exactly
    // when its stderr is being piped and relayed — so its log EVENTS would be a
    // duplicate of every line.
    let (client, events): (Arc<MatterClient>, tokio::sync::mpsc::Receiver<MatterEvent>) =
        if started_here {
            MatterClient::connect_to_managed(url).await?
        } else {
            MatterClient::connect(url).await?
        };

    let control = Arc::new(MatterDeviceControl::new(client.clone()));
    let commissioner: Arc<dyn DeviceCommissioningPort> =
        Arc::new(MatterCommissioner::new(client.clone(), r.notifier.clone()));

    // Supervised: on connection loss it reconnects with backoff and swaps the
    // fresh client into the control's handle, so a controller restart no longer
    // needs a pond-server restart. It also restarts the controller itself when
    // reconnecting alone stops being enough.
    let supervisor = tokio::spawn(run_matter_supervisor(
        SupervisorConfig {
            url: url.to_string(),
            data_dir: r.data_dir.clone(),
            child: child.clone(),
            ble,
        },
        control.client_handle(),
        client,
        events,
        r.registry.clone(),
        r.bus.clone(),
        r.notifier.clone(),
    ));

    Ok(Connected {
        child,
        supervisor,
        commissioner,
        control,
    })
}

/// Stop whatever is running and clear the cells the rest of the app reads.
/// Cells first: no caller should reach a controller that is being killed.
async fn teardown(running: &mut Running, r: &Reconciler) {
    *r.commissioner.write().await = None;
    *r.control.write().await = None;

    if let Some(supervisor) = running.supervisor.take() {
        // The supervisor reconnects forever by design, so it is aborted rather
        // than awaited — otherwise disabling Matter would leave a task racing
        // to re-open the connection just torn down. Aborting it first also
        // means it cannot revive a controller between here and the kill below.
        supervisor.abort();
    }
    stop_controller(&running.child, &r.data_dir).await;
}

/// Kill the controller GIAP started, whichever process that currently is.
///
/// Reads the handle out of the shared cell rather than taking a `Child` by value: after a
/// respawn the `connect` handle names a dead process. Empty cell means GIAP started none.
pub(crate) async fn stop_controller(child: &SharedServerChild, data_dir: &std::path::Path) {
    if let Some(mut running) = child.lock().await.take() {
        tracing::info!("matter: stopping the controller GIAP started");
        let _ = running.start_kill();

        // AWAITED, not merely signalled: `start_kill` returns before the process
        // releases the port, and a restart probing that port finds the dying
        // controller, classifies it `Occupant::Foreign` and refuses to start.
        // Bounded because shutdown must not hang; SIGKILL itself cannot be refused.
        if tokio::time::timeout(KILL_TIMEOUT, running.wait())
            .await
            .is_err()
        {
            tracing::warn!(
                target: "giap::trace",
                kind = "matter_controller_kill_timeout",
                timeout_secs = KILL_TIMEOUT.as_secs(),
                "matter: the controller did not exit after being killed; its port may still be held"
            );
        }

        // Cleared on the way out so the next start has nothing stale to
        // classify. Losing this file is harmless — the pid check would find the
        // process gone — but leaving it costs a `ps` on every start.
        crate::server_setup::clear_pidfile(data_dir);
    }
}

/// A [`DeviceControlPort`] that follows the runtime: Matter while connected, a fallback
/// (the logging stub) otherwise. Cloned into the agent, the MCP server and the tool wiring
/// at startup, so it must be one `Arc` for the whole process; the decision moves behind it,
/// into a cell the reconciler writes.
pub struct SwitchableDeviceControl {
    matter: ControlCell,
    status: Arc<RwLock<MatterStatus>>,
    fallback: Arc<dyn DeviceControlPort>,
}

impl SwitchableDeviceControl {
    /// The backend for `device_id`, or the reason there is none. A Matter device never
    /// falls back: the stub answers every verb with success and would report the device
    /// switched on when nothing was sent. The test is the PREFIX check
    /// `is_matter_device_id`, so an unreadable Matter id reaches the error, not the stub.
    async fn backend_for(&self, device_id: &str) -> Result<Arc<dyn DeviceControlPort>> {
        if let Some(matter) = self.matter.read().await.clone() {
            return Ok(matter);
        }
        if !is_matter_device_id(device_id) {
            return Ok(self.fallback.clone());
        }
        // Name the actual state: "off" and "the controller is unreachable" need
        // different actions from the user, and the agent relays whichever it is.
        Err(match self.status.read().await.state.clone() {
            MatterState::Disabled => anyhow::anyhow!(
                "Matter is off, so '{device_id}' cannot be controlled — turn Matter on in the \
                 Devices tab"
            ),
            MatterState::Connecting => anyhow::anyhow!(
                "the Matter controller is still starting, so '{device_id}' cannot be controlled yet"
            ),
            MatterState::Unreachable { error } => anyhow::anyhow!(
                "the Matter controller is unreachable ({error}), so '{device_id}' cannot be \
                 controlled"
            ),
            // Connected with no control cell is a momentary gap during teardown.
            MatterState::Connected => anyhow::anyhow!(
                "the Matter controller is restarting, so '{device_id}' cannot be controlled yet"
            ),
        })
    }
}

// Every verb forwards, including the optional ones: defaulting those to
// "unsupported" would quietly drop colour, fan, and covering control the
// moment they went through this facade.
#[async_trait]
impl DeviceControlPort for SwitchableDeviceControl {
    async fn describe(&self, device_id: &str) -> Result<DeviceDescription> {
        self.backend_for(device_id).await?.describe(device_id).await
    }

    async fn state(&self, device_id: &str) -> Result<DeviceState> {
        self.backend_for(device_id).await?.state(device_id).await
    }

    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_power(device_id, on)
            .await
    }

    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_brightness(device_id, percent)
            .await
    }

    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_target_temp(device_id, celsius)
            .await
    }

    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_locked(device_id, locked)
            .await
    }

    async fn set_color(
        &self,
        device_id: &str,
        hue_degrees: u16,
        saturation_percent: u8,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_color(device_id, hue_degrees, saturation_percent)
            .await
    }

    async fn set_volume(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_volume(device_id, percent)
            .await
    }

    async fn set_color_temp(&self, device_id: &str, kelvin: u32) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_color_temp(device_id, kelvin)
            .await
    }

    async fn set_fan_speed(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_fan_speed(device_id, percent)
            .await
    }

    async fn set_mode(
        &self,
        device_id: &str,
        setting: &str,
        value: &str,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_mode(device_id, setting, value)
            .await
    }

    async fn set_operation(
        &self,
        device_id: &str,
        operation: &str,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_operation(device_id, operation)
            .await
    }

    async fn set_tilt(&self, device_id: &str, percent_open: u8) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_tilt(device_id, percent_open)
            .await
    }

    async fn set_valve(&self, device_id: &str, open: bool) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_valve(device_id, open)
            .await
    }

    async fn set_position(
        &self,
        device_id: &str,
        percent_open: u8,
    ) -> Result<DeviceControlOutcome> {
        self.backend_for(device_id)
            .await?
            .set_position(device_id, percent_open)
            .await
    }
}

#[cfg(test)]
mod controller_lifetime_tests {
    use super::*;

    /// Is a pid still known to the OS? `kill -0` is POSIX and, crucially,
    /// SUCCEEDS on a zombie — a child that was signalled but never reaped —
    /// which is exactly the state this test exists to rule out.
    fn pid_exists(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// The controller is GONE when `stop_controller` returns, not merely signalled.
    ///
    /// `start_kill` only sends the signal, so a restart probing the Matter port finds the
    /// dying controller, classifies it `Occupant::Foreign` and refuses to start.
    #[tokio::test]
    async fn stopping_the_controller_waits_for_it_to_actually_exit() {
        let spawned = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("sleep must be available");
        let pid = spawned.id().expect("a freshly spawned child has a pid");
        assert!(pid_exists(pid), "the child must be running to begin with");

        let child: SharedServerChild = Arc::new(tokio::sync::Mutex::new(Some(spawned)));
        stop_controller(&child, std::path::Path::new("/nonexistent")).await;

        assert!(
            !pid_exists(pid),
            "pid {pid} still exists after stop_controller returned — it was signalled \
             but not reaped, so its port is still held"
        );
        assert!(
            child.lock().await.is_none(),
            "the handle must be taken, so nothing kills a pid that has been reused"
        );
    }
}
