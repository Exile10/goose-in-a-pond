//! Bundled userspace networking lifecycle and its private companion boundary.
//! The public listeners never accept the embedded peer header.

use anyhow::{bail, ensure, Context, Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use pond_api::network::{is_tailnet, EmbeddedAddress};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::Mutex,
};

mod recovery;
mod revocation;

const PEER_HEADER: &str = "x-pond-embedded-peer";

/// Hosted coordinator for a household that names none. Applied on enable, never on read: an
/// empty control URL is what marks a household local-only.
pub const DEFAULT_CONTROL_URL: &str = "https://controlpond.jarida.io";
pub const DEFAULT_ENROLLMENT_URL: &str = "https://enrollpond.jarida.io";

/// Default both origins only when both are empty; validation rejects a half-configured pair.
fn with_default_coordinator(mut config: Config) -> Config {
    if config.control_url.is_empty() && config.enrollment_url.is_empty() {
        config.control_url = DEFAULT_CONTROL_URL.to_string();
        config.enrollment_url = DEFAULT_ENROLLMENT_URL.to_string();
    }
    config
}

/// Where the Pond records the coordination service its household is registered with.
///
/// An invite or a device certificate is needed only for a household's first registration, so the
/// dashboard asks for an invite only while this names no service, or another one than configured.
const REGISTERED_FILE: &str = "registered.json";

/// The contents of [`REGISTERED_FILE`]: the enrollment origin the household registered with.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registered {
    enrollment: String,
}

/// Non-secret enrollment settings. No auth key can be stored in this file.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Whether to restore the embedded node on startup.
    pub enabled: bool,
    /// Explicit Headscale HTTPS origin; an empty value never selects a hosted service.
    #[serde(default)]
    pub control_url: String,
    /// Enrollment service origin, configured locally by the operator.
    #[serde(default)]
    pub enrollment_url: String,
}

/// Private, loopback-only state. Enrollment URLs must never be logged.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Backend state, localized by the caller.
    pub state: String,
    /// Tailnet addresses allocated to this application instance.
    pub addresses: Vec<String>,
    /// One-time browser authorization URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
    /// Public key of the pending network registration.
    #[serde(default)]
    pub node_key: String,
    /// Public machine identity available before coordinator authorization.
    #[serde(default)]
    pub machine_key: String,
}

struct Process {
    child: Child,
    _stdin: ChildStdin,
}

/// Owns an isolated local socket and the bundled Go networking process.
pub struct Runtime {
    directory: PathBuf,
    identity: PathBuf,
    socket: PathBuf,
    _socket_dir: tempfile::TempDir,
    port: u16,
    /// The bundled `pondnet` helper, resolved once at startup.
    helper: PathBuf,
    status: RwLock<Status>,
    process: Mutex<Option<Process>>,
    generation: AtomicU64,
    /// Held only across a read-modify-write of the revocation queue or enrollment record,
    /// never across a helper call.
    files: std::sync::Mutex<()>,
    /// Serialises helper calls about one device, so enrolling and revoking it cannot
    /// interleave, without making every other device wait for a slow coordinator.
    device_locks: std::sync::Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    /// When each device's sighting was last written, to throttle presence writes.
    sightings_written: std::sync::Mutex<BTreeMap<String, std::time::Instant>>,
    recovery: recovery::Queue,
    /// Address published by system information and pairing QR producers.
    pub address: EmbeddedAddress,
    /// Wake certificate renewal as soon as the node obtains an address.
    pub changed: tokio::sync::Notify,
}

impl Runtime {
    /// Create the private bridge; the short random socket path fits macOS's sockaddr_un limit.
    pub fn new(data: &Path, port: u16) -> Result<(Arc<Self>, tokio::net::UnixListener)> {
        // The helper holds the household's network identity, so which binary runs is not
        // something the environment decides in a release build.
        #[cfg(debug_assertions)]
        if let Some(helper) = std::env::var_os("POND_NETWORK_BINARY") {
            return Self::with_helper(data, port, PathBuf::from(helper));
        }
        Self::with_helper(
            data,
            port,
            std::env::current_exe()?.with_file_name("pondnet"),
        )
    }

    fn with_helper(
        data: &Path,
        port: u16,
        helper: PathBuf,
    ) -> Result<(Arc<Self>, tokio::net::UnixListener)> {
        let directory = data.join("embedded-network");
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        let metadata = std::fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o077 == 0,
            "embedded directory must be private and not a symlink"
        );
        let socket_dir = tempfile::Builder::new().prefix("pond-net-").tempdir()?;
        std::fs::set_permissions(socket_dir.path(), std::fs::Permissions::from_mode(0o700))?;
        let socket = socket_dir.path().join("api.sock");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        Ok((
            Arc::new(Self {
                directory,
                identity: data.join("tls/identity.json"),
                socket,
                _socket_dir: socket_dir,
                port,
                helper,
                status: RwLock::new(Status {
                    state: "Stopped".into(),
                    addresses: vec![],
                    auth_url: None,
                    node_key: String::new(),
                    machine_key: String::new(),
                }),
                process: Mutex::new(None),
                generation: AtomicU64::new(0),
                files: std::sync::Mutex::new(()),
                device_locks: std::sync::Mutex::new(BTreeMap::new()),
                sightings_written: std::sync::Mutex::new(BTreeMap::new()),
                recovery: recovery::Queue::default(),
                address: EmbeddedAddress::default(),
                changed: tokio::sync::Notify::new(),
            }),
            listener,
        ))
    }

    /// Load explicit settings without silently replacing corrupt configuration.
    pub fn config(&self) -> Result<Config> {
        let path = self.directory.join("config.json");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Ok(meta) => ensure!(
                meta.is_file() && !meta.file_type().is_symlink() && meta.len() < 4096,
                "invalid embedded configuration file"
            ),
            Err(e) => return Err(e.into()),
        }
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    fn persist(&self, config: &Config) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&serde_json::to_vec(config)?)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("config.json"))?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    fn publish(&self, status: Status) {
        let previous = self
            .status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .state
            .clone();
        if previous != status.state {
            tracing::info!(
                previous,
                state = status.state,
                "embedded networking state changed"
            );
            // A Pond's node is enrolled only after its household registered, so reaching Running
            // proves it. This is what records a household that registered before the record existed.
            if status.state == "Running" {
                self.record_registered();
            }
        }
        let address = status
            .addresses
            .iter()
            .find_map(|s| s.parse::<std::net::Ipv4Addr>().ok())
            .filter(|ip| is_tailnet((*ip).into()))
            .map(|ip| ip.to_string());
        if address.is_none() {
            *self.address.0.write().unwrap_or_else(|p| p.into_inner()) = None;
        }
        *self.status.write().unwrap_or_else(|p| p.into_inner()) = status;
        self.changed.notify_one();
    }

    /// Record that the household is registered with the configured coordination service.
    /// Failing to record it is logged and costs only an invite field the dashboard shows again.
    fn record_registered(&self) {
        let enrollment = match self.config() {
            Ok(config) => with_default_coordinator(config).enrollment_url,
            Err(error) => {
                tracing::warn!(%error, "could not read the configuration to record the household's registration");
                return;
            }
        };
        if self.registered_with().as_deref() == Some(enrollment.as_str()) {
            return;
        }
        match revocation::write_private_json(
            &self.directory,
            REGISTERED_FILE,
            &Registered {
                enrollment: enrollment.clone(),
            },
        ) {
            Ok(()) => tracing::info!(
                target: "giap::trace",
                kind = "household_registration_recorded",
                %enrollment,
                "remote access: recorded that this household is registered"
            ),
            Err(error) => tracing::warn!(
                %error,
                %enrollment,
                "remote access: could not record that this household is registered"
            ),
        }
    }

    /// Whether the household is registered with the coordination service now configured.
    fn registered(&self) -> bool {
        match self.config() {
            Ok(config) => {
                let enrollment = with_default_coordinator(config).enrollment_url;
                self.registered_with().as_deref() == Some(enrollment.as_str())
            }
            Err(error) => {
                tracing::warn!(%error, "could not read the configuration to report the household's registration");
                false
            }
        }
    }

    /// The enrollment origin [`REGISTERED_FILE`] names, if it exists and can be read.
    fn registered_with(&self) -> Option<String> {
        match revocation::read_private_json::<Registered>(
            &self.directory.join(REGISTERED_FILE),
            4096,
        ) {
            Ok(registered) => registered.map(|registered| registered.enrollment),
            Err(error) => {
                tracing::warn!(%error, "the household registration record is unreadable");
                None
            }
        }
    }

    /// Publish only addresses covered by the certificate installed on the listener.
    pub fn publish_ready(&self, certificate_names: &[String]) {
        let status = self.status.read().unwrap_or_else(|p| p.into_inner());
        let address = status.addresses.iter().find(|value| {
            certificate_names.contains(value) && value.parse::<std::net::Ipv4Addr>().is_ok()
        });
        *self.address.0.write().unwrap_or_else(|p| p.into_inner()) = if status.state == "Running" {
            address.cloned()
        } else {
            None
        };
    }

    /// The serial of the device certificate this Pond was imaged with, or `None` when it was
    /// never provisioned. A certificate file that is a symlink, too large, readable by others
    /// or unreadable counts as absent and is logged, so the dashboard falls back to asking for
    /// an invite rather than claiming a provisioning it cannot show.
    fn provisioned_serial(&self) -> Option<String> {
        #[derive(Deserialize)]
        struct Certificate {
            payload: String,
        }
        #[derive(Deserialize)]
        struct Payload {
            serial: String,
        }
        let path = self.directory.join("device").join("certificate.json");
        // Opened once and checked on the open handle, so a file swapped in after a check is
        // never read: a symlink, a loose mode or more than 8 KiB counts as no certificate.
        let certificate = match revocation::read_private_json::<Certificate>(&path, 8192) {
            Ok(Some(certificate)) => certificate,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(error = %error, "device certificate unreadable; ignoring it");
                return None;
            }
        };
        use base64::Engine;
        let serial = base64::engine::general_purpose::STANDARD
            .decode(certificate.payload)
            .ok()
            .and_then(|payload| serde_json::from_slice::<Payload>(&payload).ok())
            .map(|payload| payload.serial)
            .filter(|serial| serial.len() == 32 && serial.bytes().all(|b| b.is_ascii_hexdigit()));
        if serial.is_none() {
            tracing::warn!("device certificate carries no serial; ignoring it");
        }
        serial
    }

    async fn authority(
        &self,
        action: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        use tokio::io::AsyncWriteExt;
        let config = self.config()?;
        let mut child = self
            .helper_command()?
            .arg("--authority-action")
            .arg(action)
            .arg("--state")
            .arg(self.directory.join("authority"))
            .arg("--enrollment")
            .arg(config.enrollment_url)
            .arg("--port")
            .arg(self.port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(&serde_json::to_vec(&payload)?).await?;
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(25), child.wait_with_output())
                .await??;
        // Exit 3: the coordinator refused the request, as opposed to being unreachable.
        if output.status.code() == Some(3) {
            #[derive(Deserialize)]
            struct Refusal {
                refused: String,
            }
            let reason =
                serde_json::from_slice::<Refusal>(&output.stdout[..output.stdout.len().min(1024)])
                    .ok()
                    .map(|refusal| refusal.refused);
            bail!(RefusedByCoordinator {
                complaint: helper_complaint(&output.stderr),
                reason,
            });
        }
        ensure!(
            output.status.success(),
            "the network helper exited {} during {action}: {}",
            output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "on a signal".to_string()),
            helper_complaint(&output.stderr),
        );
        ensure!(
            output.stdout.len() < 8192,
            "the network helper answered {action} with {} bytes, more than the 8192 allowed",
            output.stdout.len(),
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    async fn enroll(
        &self,
        device: &str,
        role: &str,
        registration: &Registration,
    ) -> Result<serde_json::Value> {
        let device = if role == "phone" {
            network_device(device)?
        } else {
            device.to_owned()
        };
        let lock = self.device_lock(&device);
        let _held = lock.lock().await;
        ensure!(
            !self.pending_revocations()?.contains_key(&device),
            "device revocation is pending"
        );
        let payload = self.registration_payload(&device, role, registration)?;
        let enrolled = self.authority("enroll", payload).await?;
        self.ensure_not_revoked_meanwhile(&device)?;
        Ok(enrolled)
    }

    /// A command for the helper, refusing one another account could have replaced, and
    /// passing it only the environment it needs. An inherited `TS_AUTHKEY` could join the
    /// node to another tailnet and an inherited `HTTPS_PROXY` could route its coordinator
    /// traffic, so nothing else is passed through.
    fn helper_command(&self) -> Result<Command> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&self.helper).with_context(|| {
            format!(
                "bundled pondnet helper is missing at {}; build native/pondnet first",
                self.helper.display()
            )
        })?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        ensure!(
            metadata.is_file()
                && (metadata.uid() == euid || metadata.uid() == 0)
                && metadata.mode() & 0o022 == 0,
            "refusing the pondnet helper at {}: it must be a file owned by this account or root \
             and writable by no one else",
            self.helper.display()
        );
        let mut command = Command::new(&self.helper);
        command.env_clear();
        for name in ["HOME", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        Ok(command)
    }

    /// The lock that serialises helper calls about `device`.
    fn device_lock(&self, device: &str) -> Arc<Mutex<()>> {
        self.device_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(device.to_owned())
            .or_default()
            .clone()
    }

    /// A revocation queued while a helper call was in flight wins: the call must not report a
    /// success that the queued revocation is about to undo.
    fn ensure_not_revoked_meanwhile(&self, device: &str) -> Result<()> {
        ensure!(
            !self.pending_revocations()?.contains_key(device),
            "a revocation was queued for this device while it was being enrolled"
        );
        Ok(())
    }

    /// Note that `device` holds an enrollment, so a later revocation is sent without a check.
    fn record_enrolled(&self, device: &str) -> Result<()> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let mut record = revocation::load_enrolled(&self.directory)?;
        if record.devices.insert(device.to_owned()) {
            ensure!(
                record.devices.len() <= revocation::MAX_DEVICES,
                "enrollment record is full"
            );
            revocation::write_private_json(&self.directory, revocation::ENROLLED_FILE, &record)?;
        }
        Ok(())
    }

    /// Begin a complete record for a household created just now; an existing one is kept.
    fn start_enrollment_record(&self) -> Result<()> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        if !self.directory.join(revocation::ENROLLED_FILE).exists() {
            revocation::write_private_json(
                &self.directory,
                revocation::ENROLLED_FILE,
                &revocation::Enrolled {
                    complete: true,
                    devices: Default::default(),
                },
            )?;
            tracing::info!("remote access: keeping a complete record of enrolled phones");
        }
        Ok(())
    }

    fn forget_enrolled(&self, device: &str) -> Result<()> {
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let mut record = revocation::load_enrolled(&self.directory)?;
        if record.devices.remove(device) {
            revocation::write_private_json(&self.directory, revocation::ENROLLED_FILE, &record)?;
        }
        Ok(())
    }

    fn registration_payload(
        &self,
        device: &str,
        role: &str,
        registration: &Registration,
    ) -> Result<serde_json::Value> {
        let config = self.config()?;
        ensure!(config.enabled, "remote access needs local approval");
        let expected = url::Url::parse(&config.control_url)?;
        let supplied = url::Url::parse(&registration.auth_url)?;
        ensure!(
            expected.origin() == supplied.origin()
                && supplied.username().is_empty()
                && supplied.password().is_none()
                && supplied.query().is_none()
                && supplied.fragment().is_none(),
            "invalid enrollment origin"
        );
        let auth_id = supplied
            .path()
            .strip_prefix("/register/")
            .context("invalid registration path")?;
        ensure!(
            !auth_id.contains('/') && (16..=256).contains(&auth_id.len()),
            "invalid pending registration"
        );
        // The coordinator checks the same shapes; checked here too so a malformed key is
        // refused before anything is signed for it, and so an empty machine key can never
        // compare equal to an enrollment that lacks one.
        ensure!(
            hex_key(&registration.machine_key, "mkey:")
                && (registration.node_key.is_empty()
                    || hex_key(&registration.node_key, "nodekey:")),
            "invalid node or machine key"
        );
        Ok(
            serde_json::json!({"household":"", "device":device,"role":role,"action":"enroll","authId":auth_id,"nodeKey":registration.node_key,"machineKey":registration.machine_key,"nonce":"","expires":0}),
        )
    }

    fn pending_revocations(&self) -> Result<revocation::Queue> {
        revocation::load_queue(&self.directory, Utc::now())
    }

    fn save_revocations(&self, pending: &revocation::Queue) -> Result<()> {
        revocation::write_private_json(&self.directory, revocation::QUEUE_FILE, pending)
    }

    /// Retry durable revocations at a bounded rate, including while networking is disabled.
    /// Never returns: a fault is logged and retried, because ending this future would end
    /// `serve()` and take the dashboard down with it.
    pub async fn reconcile_revocations(&self) -> std::convert::Infallible {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            // Absent-device revocations are queued, so they survive an offline coordinator.
            if let Err(error) = self.sweep_absent_devices().await {
                tracing::warn!(%error, "could not check which devices have been away too long");
            }
            if let Err(error) = self.reconcile_due().await {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "remote_revocation_store_unavailable",
                    %error,
                    "the remote revocation queue could not be read or written; retrying"
                );
            }
        }
    }

    /// Send the revocations that are due, backing off each failure on its own.
    async fn reconcile_due(&self) -> Result<()> {
        let due = revocation::due(&self.pending_revocations()?, Utc::now());
        for (device, entry) in due {
            let outcome = self.send_revocation(&device, entry.verify).await;
            let unavailable = self.settle(&device, outcome)?;
            // Unreachable is unreachable for every device; a refusal is about this one.
            if unavailable {
                break;
            }
        }
        Ok(())
    }

    /// Record what became of one attempt; true when the coordinator could not be reached.
    fn settle(&self, device: &str, outcome: Sent) -> Result<bool> {
        let files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let mut queue = self.pending_revocations()?;
        let (reason, unavailable) = match outcome {
            Sent::Revoked | Sent::NeverEnrolled => {
                queue.remove(device);
                self.save_revocations(&queue)?;
                drop(files);
                self.forget_enrolled(device)?;
                if matches!(outcome, Sent::Revoked) {
                    tracing::info!(%device, "queued remote network revocation completed");
                } else {
                    tracing::info!(
                        %device,
                        "queued remote revocation dropped: the coordinator has no enrollment for this device"
                    );
                }
                return Ok(false);
            }
            Sent::Refused(reason) => (reason, false),
            Sent::Unavailable(reason) => (reason, true),
        };
        if let Some(entry) = queue.get_mut(device) {
            entry.back_off(Utc::now());
            tracing::warn!(
                %device,
                attempts = entry.attempts,
                next_attempt = %entry.next_attempt,
                unavailable,
                %reason,
                "remote network revocation remains queued"
            );
        }
        self.save_revocations(&queue)?;
        Ok(unavailable)
    }

    async fn send_revocation(&self, device: &str, verify: bool) -> Sent {
        let lock = self.device_lock(device);
        let _held = lock.lock().await;
        let classify = |error: anyhow::Error| match error.downcast_ref::<RefusedByCoordinator>() {
            Some(refused) if refused.reason.as_deref() == Some("enrollment_missing") => {
                Sent::NeverEnrolled
            }
            Some(refused) => Sent::Refused(refused.to_string()),
            None => Sent::Unavailable(format!("{error:#}")),
        };
        if verify {
            if let Err(error) = self
                .authority(
                    "inspect",
                    serde_json::json!({"device": device, "role": "phone"}),
                )
                .await
            {
                return classify(error);
            }
        }
        match self
            .authority(
                "revoke",
                serde_json::json!({
                    "household":"", "device":device, "role":"phone", "action":"revoke",
                    "authId":"", "nodeKey":"", "nonce":"", "expires":0
                }),
            )
            .await
        {
            Ok(_) => Sent::Revoked,
            Err(error) => classify(error),
        }
    }

    /// Current native addresses for certificate SAN renewal.
    pub fn addresses(&self) -> Vec<String> {
        self.status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .addresses
            .clone()
    }

    /// Start once, keeping stdin open so a normal shutdown stops the helper.
    pub async fn start(self: &Arc<Self>, config: Config) -> Result<()> {
        ensure!(config.enabled, "remote access must be explicitly enabled");
        let config = with_default_coordinator(config);
        ensure!(
            !config.enrollment_url.is_empty(),
            "enrollment service is required"
        );
        let enrollment = url::Url::parse(&config.enrollment_url)?;
        ensure!(
            enrollment.scheme() == "https"
                && enrollment.host_str().is_some()
                && enrollment.username().is_empty()
                && enrollment.password().is_none()
                && enrollment.query().is_none()
                && enrollment.fragment().is_none()
                && matches!(enrollment.path(), "" | "/"),
            "invalid enrollment origin"
        );
        ensure!(
            !config.control_url.is_empty(),
            "Headscale control server is required"
        );
        {
            let url = url::Url::parse(&config.control_url)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none()
                    && matches!(url.path(), "" | "/"),
                "control server must be an HTTPS origin"
            );
        }
        let mut process = self.process.lock().await;
        if let Some(current) = process.as_mut() {
            if current.child.try_wait()?.is_none() {
                ensure!(
                    self.config()?.control_url == config.control_url,
                    "stop remote access before changing the control server"
                );
                return Ok(());
            }
            *process = None;
        }
        let mut child = self
            .helper_command()?
            .arg("--state")
            .arg(self.directory.join("node"))
            .arg("--hostname")
            .arg("goose-in-a-pond")
            .arg("--control")
            .arg(&config.control_url)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--identity")
            .arg(&self.identity)
            .arg("--port")
            .arg(self.port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("start embedded network helper")?;
        let input = child.stdin.take().context("embedded stdin unavailable")?;
        let output = child.stdout.take().context("embedded status unavailable")?;
        self.persist(&config)?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *process = Some(Process {
            child,
            _stdin: input,
        });
        self.publish(Status {
            state: "Starting".into(),
            addresses: vec![],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        });
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(output);
            loop {
                let mut line = Vec::new();
                match (&mut reader).take(16385).read_until(b'\n', &mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => (),
                }
                if generation != runtime.generation.load(Ordering::SeqCst) {
                    return;
                }
                if line.len() > 16384 {
                    break;
                }
                let Ok(mut status) = serde_json::from_slice::<Status>(&line) else {
                    break;
                };
                if status.state.len() > 64
                    || status.addresses.len() > 8
                    || status.node_key.len() > 80
                    || status.machine_key.len() > 80
                {
                    break;
                }
                status
                    .addresses
                    .retain(|s| s.parse::<std::net::IpAddr>().is_ok_and(is_tailnet));
                status.auth_url = status.auth_url.filter(|s| {
                    url::Url::parse(s).is_ok_and(|u| {
                        u.scheme() == "https" && u.username().is_empty() && u.password().is_none()
                    })
                });
                runtime.publish(status);
            }
            let mut process = runtime.process.lock().await;
            if generation == runtime.generation.load(Ordering::SeqCst) {
                if let Some(mut failed) = process.take() {
                    let _ = failed.child.kill().await;
                }
                tracing::error!("embedded networking helper stopped unexpectedly");
                runtime.publish(Status {
                    state: "Unavailable".into(),
                    addresses: vec![],
                    auth_url: None,
                    node_key: String::new(),
                    machine_key: String::new(),
                });
            }
        });
        Ok(())
    }

    /// Stop and persist disablement without deleting the registered node identity.
    pub async fn disable(&self) -> Result<()> {
        let mut config = self.config()?;
        config.enabled = false;
        self.persist(&config)?;
        self.shutdown().await
    }

    /// Stop the helper when the Pond exits, retaining the startup preference.
    pub async fn shutdown(&self) -> Result<()> {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.recovery.clear();
        if let Some(mut process) = self.process.lock().await.take() {
            drop(process._stdin);
            if tokio::time::timeout(std::time::Duration::from_secs(10), process.child.wait())
                .await
                .is_err()
            {
                process.child.kill().await?;
            }
        }
        self.publish(Status {
            state: "Stopped".into(),
            addresses: vec![],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        });
        Ok(())
    }
}

fn network_device(id: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    ensure!(
        !id.is_empty() && id.len() <= 256,
        "invalid paired device identity"
    );
    let mut digest = Sha256::new();
    digest.update(b"goose-enrollment-device-v1\0");
    digest.update(id.as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

/// `prefix` followed by 64 lowercase hex digits, as the coordinator writes keys.
fn hex_key(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn valid_device(id: &str) -> bool {
    (16..=80).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// When each device was last seen at home; kept with the remote-access files, not in `devices`.
/// Keyed by network-hashed id, like the queue and the enrollment record.
const PRESENCE_FILE: &str = "presence.json";
/// Bounds on the presence file; a household has far fewer devices.
const PRESENCE_MAX_BYTES: u64 = 256 * 1024;
const PRESENCE_MAX_DEVICES: usize = 1024;
/// A sighting is written at most this often per device: every authenticated request is one.
const PRESENCE_WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl Runtime {
    /// Sightings by network id. A corrupt file must not stop devices lapsing, which is how it
    /// failed open before: it is moved aside, and every enrolled phone is treated as last seen
    /// when the file was last written, so each still lapses within the window.
    fn presence(&self) -> Result<BTreeMap<String, String>> {
        let path = self.directory.join(PRESENCE_FILE);
        match revocation::read_private_json::<BTreeMap<String, String>>(&path, PRESENCE_MAX_BYTES) {
            Ok(None) => Ok(BTreeMap::new()),
            Ok(Some(seen)) if seen.len() <= PRESENCE_MAX_DEVICES => Ok(seen
                .into_iter()
                .map(|(device, at)| {
                    // Written before sightings were hashed.
                    let device = if valid_device(&device) && device.len() == 64 {
                        device
                    } else {
                        network_device(&device).unwrap_or(device)
                    };
                    (device, at)
                })
                .collect()),
            outcome => {
                let reason = match outcome {
                    Err(error) => format!("{error:#}"),
                    _ => "too many devices".to_owned(),
                };
                let written = std::fs::symlink_metadata(&path)
                    .and_then(|m| m.modified())
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now());
                let aside = self.directory.join(format!(
                    "{PRESENCE_FILE}.corrupt-{}",
                    Utc::now().timestamp()
                ));
                std::fs::rename(&path, &aside)?;
                let seen: BTreeMap<String, String> = revocation::load_enrolled(&self.directory)?
                    .devices
                    .into_iter()
                    .map(|device| (device, written.to_rfc3339()))
                    .collect();
                tracing::warn!(
                    target: "giap::trace",
                    kind = "presence_record_replaced",
                    %reason,
                    aside = %aside.display(),
                    devices = seen.len(),
                    "the presence record was unreadable; enrolled phones now lapse from its last write"
                );
                self.save_presence(&seen)?;
                Ok(seen)
            }
        }
    }

    fn save_presence(&self, seen: &BTreeMap<String, String>) -> Result<()> {
        revocation::write_private_json(&self.directory, PRESENCE_FILE, seen)
    }

    /// Queue revocation for every device not home within the window; the retry loop sends them.
    async fn sweep_absent_devices(&self) -> Result<()> {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        // A local-only household must not contact coordination.
        if self.config()?.enrollment_url.is_empty() {
            return Ok(());
        }
        let cutoff = Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS);
        for device in self.absent_since(cutoff).await? {
            tracing::warn!(
                target: "giap::trace",
                kind = "remote_access_lapsed",
                %device,
                window_days = LAN_PRESENCE_WINDOW_DAYS,
                "remote access lapsed: this device has not been on the household network inside \
                 the window, so its remote access is being revoked. Local pairing is untouched, \
                 and bringing it home restores it."
            );
            // Also forgets the sighting, or every sweep would re-queue this revocation.
            self.queue_network(&device)?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl pond_core::security::ports::remote_access::DevicePresence for Runtime {
    async fn seen_on_lan(&self, device_id: &str) {
        let Ok(device) = network_device(device_id) else {
            return;
        };
        {
            let mut written = self
                .sightings_written
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if written
                .get(&device)
                .is_some_and(|at| at.elapsed() < PRESENCE_WRITE_INTERVAL)
            {
                return;
            }
            written.insert(device.clone(), std::time::Instant::now());
        }
        let write = || -> Result<()> {
            let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
            let mut seen = self.presence()?;
            if !seen.contains_key(&device) && seen.len() >= PRESENCE_MAX_DEVICES {
                bail!("the presence record is full");
            }
            seen.insert(device.clone(), Utc::now().to_rfc3339());
            self.save_presence(&seen)
        };
        if let Err(error) = write() {
            // Never fails the request; a lost sighting only brings the device's reminder forward.
            self.sightings_written
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&device);
            tracing::warn!(%error, %device, "could not record a LAN sighting");
        }
    }

    async fn absent_since(&self, cutoff: DateTime<Utc>) -> Result<Vec<String>> {
        Ok(self
            .presence()?
            .into_iter()
            .filter(|(_, seen)| {
                DateTime::parse_from_rfc3339(seen)
                    .map(|at| at.with_timezone(&Utc) < cutoff)
                    // An unreadable timestamp is not evidence of absence.
                    .unwrap_or(false)
            })
            .map(|(device, _)| device)
            .collect())
    }

    async fn lapses_at(&self, device_id: &str) -> Result<Option<DateTime<Utc>>> {
        use pond_core::security::ports::remote_access::LAN_PRESENCE_WINDOW_DAYS;
        let device = network_device(device_id)?;
        Ok(self.presence()?.get(&device).and_then(|seen| {
            DateTime::parse_from_rfc3339(seen)
                .ok()
                .map(|at| at.with_timezone(&Utc) + chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS))
        }))
    }
}

#[async_trait::async_trait]
impl pond_core::security::ports::remote_access::RemoteRevocation for Runtime {
    async fn queue(&self, device_id: &str) -> Result<()> {
        self.queue_network(&network_device(device_id)?)
    }
}

impl Runtime {
    /// Queue the revocation of a network-hashed device, and forget its sightings.
    fn queue_network(&self, device: &str) -> Result<()> {
        // A local-only household has never registered a node and must not contact coordination.
        if self.config()?.enrollment_url.is_empty() {
            return Ok(());
        }
        let device = device.to_owned();
        self.recovery.revoke(&device);
        let _files = self.files.lock().unwrap_or_else(|p| p.into_inner());
        let mut seen = self.presence()?;
        if seen.remove(&device).is_some() {
            self.save_presence(&seen)?;
        }
        let verify = match revocation::plan(&revocation::load_enrolled(&self.directory)?, &device) {
            revocation::Plan::Revoke => false,
            revocation::Plan::VerifyThenRevoke => true,
            revocation::Plan::NothingToRevoke => {
                tracing::debug!(%device, "no remote revocation needed: this device was never enrolled");
                return Ok(());
            }
        };
        let mut pending = self.pending_revocations()?;
        ensure!(
            pending.contains_key(&device) || pending.len() < revocation::MAX_DEVICES,
            "remote revocation queue is full"
        );
        let entry = pending
            .entry(device.clone())
            .or_insert_with(|| revocation::Pending::new(verify, Utc::now()));
        // A device learned to be enrolled since it was queued needs no check any more.
        entry.verify &= verify;
        self.save_revocations(&pending)?;
        tracing::info!(%device, verify, "remote network revocation queued durably");
        Ok(())
    }
}

fn local(
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<(), StatusCode> {
    if peer.is_ok_and(|p| p.0.ip().is_loopback()) {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn status(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<Status>, StatusCode> {
    local(peer)?;
    Ok(Json(
        runtime
            .status
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone(),
    ))
}

async fn enable(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    Json(config): Json<Config>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    runtime.start(config).await.map_err(|error| {
        tracing::warn!(%error, "could not enable embedded networking");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok(Json(serde_json::json!({"accepted":true})))
}

async fn disable(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    runtime.disable().await.map_err(|error| {
        tracing::warn!(%error, "could not disable embedded networking");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok(Json(serde_json::json!({"accepted":true})))
}

/// Refuse management without the host credential: these routes can re-point the Pond and
/// every paired phone at another coordinator, and a bearer token proves only that its holder
/// once had a pairing code.
async fn host_only(
    State(credential): State<pond_api::host_guard::HostCredential>,
    request: Request,
    next: Next,
) -> Response {
    match pond_api::host_guard::require_credential(
        Some(&credential),
        request.headers(),
        "remote_access_management",
    ) {
        Ok(()) => next.run(request).await,
        Err(refusal) => refusal.into_response(),
    }
}

/// Management is available only on the loopback listener, guarded by socket peer and the
/// host credential.
pub fn management(
    runtime: Arc<Runtime>,
    credential: pond_api::host_guard::HostCredential,
) -> Router {
    Router::new()
        .route(
            "/api/v1/remote-access",
            get(status).post(enable).delete(disable),
        )
        .route(
            "/api/v1/remote-access/identity",
            axum::routing::post(authority_identity),
        )
        .route(
            "/api/v1/remote-access/register",
            axum::routing::post(register_pond),
        )
        .route("/api/v1/remote-access/device", get(device_status))
        .merge(recovery::local_routes())
        .with_state(runtime)
        .route_layer(middleware::from_fn_with_state(credential, host_only))
        .layer(middleware::from_fn(pond_api::middleware::log_requests))
        // The desktop renderer calls these cross-origin, like every other dashboard route.
        .layer(pond_api::cors_layer())
}

async fn trusted_peer(mut request: Request, next: Next) -> Result<Response, StatusCode> {
    let peer = request
        .headers_mut()
        .remove(PEER_HEADER)
        .and_then(|v| v.to_str().ok().and_then(|s| s.parse::<SocketAddr>().ok()))
        .filter(|p| p.port() != 0 && is_tailnet(p.ip()))
        .ok_or(StatusCode::FORBIDDEN)?;
    request.extensions_mut().insert(ConnectInfo(peer));
    Ok(next.run(request).await)
}

/// Only use on the private Unix socket. Public listeners ignore peer headers.
pub fn private_companion(router: Router) -> Router {
    router.layer(middleware::from_fn(trusted_peer))
}

/// Pending registration, with no authority or administration secret.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Registration {
    auth_url: String,
    node_key: String,
    machine_key: String,
}

/// A coordinator refusal rather than a fault, typed so the handler can pick a distinct status.
#[derive(Debug)]
pub struct RefusedByCoordinator {
    /// The helper's stderr, bounded and redacted.
    pub complaint: String,
    /// The coordinator's own error identifier, e.g. `enrollment_missing`.
    pub reason: Option<String>,
}

impl std::fmt::Display for RefusedByCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.complaint)
    }
}

/// What became of one attempt to send a queued revocation.
enum Sent {
    Revoked,
    /// The coordinator has no such enrollment: nothing to revoke, and no tombstone written.
    NeverEnrolled,
    Refused(String),
    Unavailable(String),
}

impl std::error::Error for RefusedByCoordinator {}

/// The helper's stderr, fit to log: bounded, one line, and with `/register/` URLs redacted
/// (a bearer capability to join the household's tailnet).
fn helper_complaint(stderr: &[u8]) -> String {
    const KEEP: usize = 400;
    let text = String::from_utf8_lossy(stderr);
    let redacted: Vec<&str> = text
        .split_whitespace()
        .map(|word| {
            if word.contains("/register/") {
                "<redacted enrolment URL>"
            } else {
                word
            }
        })
        .collect();
    let line = redacted.join(" ");
    if line.is_empty() {
        return "and said nothing".to_string();
    }
    match line.char_indices().nth_back(KEEP) {
        // Keep the tail: the helper prints its reason last.
        Some((at, _)) => format!("...{}", &line[at..]),
        None => line,
    }
}

async fn authority_identity(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    // No authority yet means no phone was ever enrolled, so the record can start complete.
    let first = !runtime.directory.join("authority").exists();
    let identity = runtime
        .authority("identity", serde_json::Value::Null)
        .await
        .map_err(|error| {
            tracing::warn!(%error, operation = "identity", "embedded enrollment failed");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    if first {
        runtime.start_enrollment_record().map_err(|error| {
            tracing::warn!(%error, operation = "enrollment_record", "embedded enrollment failed");
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    }
    Ok(Json(identity))
}
/// Body of `POST /remote-access/register`. `deny_unknown_fields`, so a misspelt field fails
/// instead of registering without the invite it meant to carry.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RegisterPond {
    /// The operator's invite, needed once, for a household the coordinator has not seen.
    #[serde(default)]
    invite: Option<String>,
}

/// Loosely the coordinator's invite shape; it decides validity, this only refuses what
/// could never be one before it reaches the helper.
/// Whether this Pond was imaged with a device certificate, and the serial it was issued
/// under, so the dashboard can say why no invite is needed and an operator can quote the
/// serial to revoke a lost Pond. The helper presents the certificate itself; nothing here
/// signs or sends it.
async fn device_status(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    let registered = runtime.registered();
    Ok(Json(match runtime.provisioned_serial() {
        Some(serial) => {
            serde_json::json!({ "provisioned": true, "serial": serial, "registered": registered })
        }
        None => serde_json::json!({ "provisioned": false, "registered": registered }),
    }))
}

fn plausible_invite(invite: &str) -> bool {
    invite.len() <= 64
        && invite
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("giap-inv1-"))
        && invite
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b' ')
}

async fn register_pond(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    body: Option<Json<RegisterPond>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let refuse =
        |status: StatusCode, error: &str| (status, Json(serde_json::json!({ "error": error })));
    local(peer).map_err(|status| refuse(status, "host_only"))?;
    let invite = body
        .map(|Json(body)| body.invite)
        .unwrap_or_default()
        .map(|invite| invite.trim().to_owned())
        .filter(|invite| !invite.is_empty());
    if invite
        .as_deref()
        .is_some_and(|invite| !plausible_invite(invite))
    {
        return Err(refuse(StatusCode::BAD_REQUEST, "invite_invalid"));
    }
    let current = runtime
        .status
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let registration = Registration {
        auth_url: current
            .auth_url
            .ok_or_else(|| refuse(StatusCode::CONFLICT, "no_pending_registration"))?,
        node_key: current.node_key,
        machine_key: current.machine_key,
    };
    // A fresh Pond has no household key until something creates one, and only the self-hosting
    // panel's "Prepare household identity" did, so the hosted Enable flow reached registration
    // without one and the helper refused it. The identity action creates the key once and
    // otherwise loads it, so asking for it on every registration is safe.
    if let Err(error) = runtime.authority("identity", serde_json::Value::Null).await {
        tracing::warn!(%error, operation = "identity", "embedded enrollment failed");
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "registration_unavailable",
        ));
    }
    // Register the household first; idempotent for one already registered, which needs no
    // invite. The invite goes to the helper on stdin and is never stored here.
    if let Err(error) = runtime
        .authority(
            "register",
            serde_json::json!({ "invite": invite.unwrap_or_default() }),
        )
        .await
    {
        let reason = error
            .downcast_ref::<RefusedByCoordinator>()
            .and_then(|refused| refused.reason.clone());
        tracing::warn!(%error, reason = ?reason, "household registration failed");
        return Err(match reason.as_deref() {
            Some(
                reason @ ("invite_required"
                | "invite_invalid"
                | "invite_expired"
                | "invite_used"
                | "device_certificate_invalid"
                | "device_revoked"
                | "device_used"),
            ) => refuse(StatusCode::FORBIDDEN, reason),
            _ => refuse(StatusCode::SERVICE_UNAVAILABLE, "registration_unavailable"),
        });
    }
    runtime.record_registered();
    runtime
        .enroll("pond000000000001", "pond", &registration)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, operation = "enroll", "embedded enrollment failed");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "enrollment_unavailable")
        })
}
async fn remote_configuration(
    State(runtime): State<Arc<Runtime>>,
    axum::Extension(principal): axum::Extension<pond_core::security::ports::policy::Principal>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let device =
        pond_core::security::domain::proven_device::ProvenDevice::from_principal(&principal);
    device.id().ok_or(StatusCode::FORBIDDEN)?;
    let config = runtime.config().map_err(|error| {
        tracing::warn!(%error, operation = "config", "embedded enrollment failed");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    // When remote access lapses unless the device comes home, so the app can warn first.
    let lapses_at = match device.id() {
        Some(id) => {
            use pond_core::security::ports::remote_access::DevicePresence;
            runtime
                .lapses_at(id)
                .await
                .unwrap_or_default()
                .map(|at| at.to_rfc3339())
        }
        None => None,
    };
    Ok(Json(serde_json::json!({
        "enabled": config.enabled,
        "controlUrl": config.control_url,
        "state": runtime.status.read().unwrap_or_else(|p| p.into_inner()).state,
        "lapsesAt": lapses_at,
    })))
}
async fn register_phone(
    State(runtime): State<Arc<Runtime>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    axum::Extension(handshake): axum::Extension<
        Arc<dyn pond_core::security::ports::handshake::Handshake>,
    >,
    headers: axum::http::HeaderMap,
    Json(registration): Json<Registration>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    pond_api::network::require_lan(Some(ConnectInfo(peer))).map_err(|_| StatusCode::FORBIDDEN)?;
    let device = recovery::caller(&headers, handshake.as_ref()).await?.device;
    let lock = runtime.device_lock(&device);
    let _held = lock.lock().await;
    let store_failed = |error: anyhow::Error| {
        tracing::warn!(%error, %device, operation = "enrollment_store", "embedded enrollment failed");
        StatusCode::SERVICE_UNAVAILABLE
    };
    if runtime
        .pending_revocations()
        .map_err(store_failed)?
        .contains_key(&device)
    {
        return Err(StatusCode::CONFLICT);
    }
    let payload = runtime
        .registration_payload(&device, "phone", &registration)
        .map_err(|error| {
            tracing::warn!(%error, %device, operation = "registration_payload", "embedded enrollment failed");
            StatusCode::CONFLICT
        })?;

    // Skip re-enrolling a phone already active with the same identity (it would raise a
    // conflict); any other answer, or none, falls through to the enrollment below.
    let replaces_existing = match runtime.authority("inspect", payload.clone()).await {
        Ok(existing) => {
            let field = |name: &str| {
                existing
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let status = field("status");
            // Machine keys are public; only whether they match is logged, as the keys are noise.
            let same_identity = field("machineKey") == registration.machine_key;
            if status == "active" && same_identity {
                tracing::info!(
                    target: "giap::trace",
                    kind = "remote_access_already_enrolled",
                    %device,
                    "remote access: already enrolled with this identity and active; nothing to do"
                );
                runtime.record_enrolled(&device).map_err(store_failed)?;
                return Ok(Json(existing));
            }
            tracing::info!(
                %device, status = %status, same_identity,
                "remote access: an existing enrollment does not match, so enrolling again"
            );
            true
        }
        Err(error) => {
            // Not a failure: the enrollment below is the authority.
            tracing::info!(%error, %device, "remote access: could not inspect the existing enrollment; enrolling");
            false
        }
    };

    let enrolled = runtime
        .authority("enroll", payload)
        .await
        .map_err(|error| {
            let refused = error.downcast_ref::<RefusedByCoordinator>().is_some();
            if refused && replaces_existing {
                // Expected, not a fault: the coordinator will not let a new identity take over an
                // enrollment it already holds. The 409 offers the phone recovery, which someone
                // approves on the home network.
                tracing::info!(
                    target: "giap::trace",
                    kind = "remote_access_recovery_required",
                    %error,
                    %device,
                    "remote access: the coordinator kept the existing enrollment; the phone must recover it"
                );
            } else {
                tracing::warn!(%error, %device, refused, operation = "enroll_phone", "embedded enrollment failed");
            }
            if refused {
                StatusCode::CONFLICT
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            }
        })?;
    // Recorded even if a revocation raced in: the queued revocation then goes out unchecked.
    runtime.record_enrolled(&device).map_err(store_failed)?;
    if let Err(error) = runtime.ensure_not_revoked_meanwhile(&device) {
        tracing::warn!(%error, %device, "embedded enrollment superseded by a revocation");
        return Err(StatusCode::CONFLICT);
    }
    tracing::info!(%device, "remote access: phone enrolled");
    Ok(Json(enrolled))
}
/// Companion enrollment uses the same bearer middleware and actual-peer LAN checks.
pub fn companion_management(runtime: Arc<Runtime>, state: Arc<pond_api::AppState>) -> Router {
    let limiter = Arc::new(pond_api::middleware::RateLimiter::new(
        20,
        std::time::Duration::from_secs(60),
    ));
    Router::new()
        .route(
            "/api/v1/remote-access/configuration",
            get(remote_configuration),
        )
        .route(
            "/api/v1/remote-access/enrollment",
            axum::routing::post(register_phone),
        )
        .merge(recovery::phone_routes(
            state.handshake.clone(),
            state.device_registry.clone(),
        ))
        .with_state(runtime)
        // route_layer, not layer: merged into the companion, a layer would also wrap its
        // fallback, charging every unknown path to this 20-a-minute budget.
        .route_layer(axum::extract::DefaultBodyLimit::max(4096))
        .route_layer(axum::Extension(state.handshake.clone()))
        .route_layer(middleware::from_fn_with_state(
            state,
            pond_api::middleware::auth_middleware,
        ))
        .route_layer(middleware::from_fn(move |request: Request, next: Next| {
            let limiter = limiter.clone();
            async move {
                let peer = request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .ok_or(StatusCode::FORBIDDEN)?
                    .0
                    .ip()
                    .to_string();
                if !limiter.check_rate_limit(&peer).await {
                    return Err(StatusCode::TOO_MANY_REQUESTS);
                };
                Ok::<_, StatusCode>(next.run(request).await)
            }
        }))
}

#[cfg(test)]
mod tests {

    use super::helper_complaint;

    #[test]
    fn the_helper_complaint_never_carries_a_node_authorisation_url() {
        let noisy = "dial failed for https://controlpond.jarida.io/register/nodekey%3Aabc123 \
                     after 3 tries";
        let said = helper_complaint(noisy.as_bytes());
        assert!(!said.contains("/register/"), "{said}");
        assert!(!said.contains("nodekey"), "{said}");
        assert!(said.contains("<redacted enrolment URL>"), "{said}");
        assert!(said.contains("dial failed"), "{said}");
        assert!(said.contains("after 3 tries"), "{said}");
    }

    #[test]
    fn the_helper_complaint_is_one_bounded_line() {
        let long = format!("start {} end", "chatter ".repeat(400));
        let said = helper_complaint(long.as_bytes());
        assert!(said.len() <= 512, "unbounded: {} bytes", said.len());
        assert!(!said.contains('\n'), "a log line must be one line");
        // The tail is kept: the helper prints its reason last.
        assert!(said.ends_with("end"), "{said}");
        assert!(said.starts_with("..."), "{said}");
    }

    #[test]
    fn saying_nothing_is_reported_as_saying_nothing() {
        assert_eq!(helper_complaint(b""), "and said nothing");
        assert_eq!(helper_complaint(b"   \n  "), "and said nothing");
    }
    use super::*;

    #[test]
    fn enabling_without_a_coordinator_uses_the_hosted_one() {
        let filled = with_default_coordinator(Config {
            enabled: true,
            ..Default::default()
        });
        assert_eq!(filled.control_url, DEFAULT_CONTROL_URL);
        assert_eq!(filled.enrollment_url, DEFAULT_ENROLLMENT_URL);
    }

    #[test]
    fn a_configured_coordinator_is_never_replaced() {
        let chosen = Config {
            enabled: true,
            control_url: "https://control.example".into(),
            enrollment_url: "https://enroll.example".into(),
        };
        let filled = with_default_coordinator(chosen.clone());
        assert_eq!(filled.control_url, chosen.control_url);
        assert_eq!(filled.enrollment_url, chosen.enrollment_url);
    }

    #[test]
    fn a_half_configured_coordinator_is_not_quietly_completed() {
        let half = Config {
            enabled: true,
            control_url: "https://control.example".into(),
            enrollment_url: String::new(),
        };
        let filled = with_default_coordinator(half);
        assert_eq!(filled.control_url, "https://control.example");
        assert!(filled.enrollment_url.is_empty());
    }

    #[tokio::test]
    async fn a_household_that_never_enabled_remote_access_keeps_no_coordinator() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let stored = runtime.config().unwrap();
        assert!(!stored.enabled);
        assert!(stored.control_url.is_empty());
        assert!(stored.enrollment_url.is_empty());
    }
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    /// `management` as the dashboard reaches it, presenting the host credential on every
    /// request, for the tests that are about something else.
    fn management_with_credential(runtime: Arc<Runtime>) -> Router {
        let credential = pond_api::host_guard::HostCredential::generate();
        let presented = axum::http::HeaderValue::from_str(credential.as_str()).unwrap();
        management(runtime, credential).layer(axum::middleware::from_fn(
            move |mut request: axum::extract::Request, next: axum::middleware::Next| {
                request
                    .headers_mut()
                    .insert(pond_api::host_guard::CREDENTIAL_HEADER, presented.clone());
                next.run(request)
            },
        ))
    }

    #[tokio::test]
    async fn a_device_that_stops_coming_home_loses_its_remote_access() {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();

        runtime.seen_on_lan("phone000000000001").await;
        runtime.seen_on_lan("phone000000000002").await;
        // Nobody is absent yet, and a sweep must not revoke the household.
        runtime.sweep_absent_devices().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());

        // Age one device past the window by hand.
        let mut seen = runtime.presence().unwrap();
        let stale = Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS + 1);
        let key = seen.keys().next().unwrap().clone();
        seen.insert(key.clone(), stale.to_rfc3339());
        runtime.save_presence(&seen).unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        let queued = runtime.pending_revocations().unwrap();
        assert_eq!(
            queued.len(),
            1,
            "exactly the absent device, not the household"
        );
        // Presence and the queue are both keyed by the coordinator's (hashed) id.
        assert!(queued.contains_key(&key));

        assert!(!runtime.presence().unwrap().contains_key(&key));
        runtime.sweep_absent_devices().await.unwrap();
        assert_eq!(runtime.pending_revocations().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_device_with_no_sighting_is_never_swept() {
        use pond_core::security::ports::remote_access::DevicePresence;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();
        assert!(runtime.absent_since(Utc::now()).await.unwrap().is_empty());
        runtime.sweep_absent_devices().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_local_only_household_is_never_swept() {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.seen_on_lan("phone000000000001").await;
        let mut seen = runtime.presence().unwrap();
        let key = seen.keys().next().unwrap().clone();
        let stale = Utc::now() - chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS + 1);
        seen.insert(key, stale.to_rfc3339());
        runtime.save_presence(&seen).unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        assert!(!runtime.directory.join("revocations.json").exists());
    }

    #[tokio::test]
    async fn the_lapse_deadline_is_a_window_after_the_last_sighting() {
        use pond_core::security::ports::remote_access::{DevicePresence, LAN_PRESENCE_WINDOW_DAYS};
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        assert_eq!(runtime.lapses_at("phone000000000001").await.unwrap(), None);

        let before = Utc::now();
        runtime.seen_on_lan("phone000000000001").await;
        let lapses = runtime
            .lapses_at("phone000000000001")
            .await
            .unwrap()
            .unwrap();
        let expected = before + chrono::Duration::days(LAN_PRESENCE_WINDOW_DAYS);
        assert!(
            (lapses - expected).num_seconds().abs() < 60,
            "lapses at {lapses}, expected about {expected}"
        );
    }

    #[tokio::test]
    async fn revocation_is_durable_idempotent_and_local_only_does_not_enroll() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let data = tempfile::tempdir().unwrap();
        let (runtime, listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.queue("a").await.unwrap();
        assert!(!runtime.directory.join("revocations.json").exists());
        runtime
            .persist(&Config {
                enabled: false,
                control_url: "https://coord.example".into(),
                enrollment_url: "https://enroll.example".into(),
            })
            .unwrap();
        runtime.queue("phone000000000001").await.unwrap();
        runtime.queue("phone000000000001").await.unwrap();
        assert_eq!(runtime.pending_revocations().unwrap().len(), 1);
        drop(listener);
        drop(runtime);
        let (restored, _) = Runtime::new(data.path(), 4443).unwrap();
        assert!(restored
            .pending_revocations()
            .unwrap()
            .contains_key(&network_device("phone000000000001").unwrap()));
        std::fs::write(restored.directory.join("revocations.json"), b"corrupt").unwrap();
        assert!(restored.queue("phone000000000002").await.is_err());
    }

    /// Enabling remote access on a fresh Pond failed with `registration_unavailable`: nothing on
    /// the Enable path created the household key, and the helper refuses to register without
    /// one ("existing household authority is required").
    #[tokio::test]
    async fn registering_a_fresh_pond_creates_its_household_identity_first() {
        use std::os::unix::fs::PermissionsExt;
        let data = tempfile::tempdir().unwrap();
        // Stands in for the helper: records each authority action, answers with an empty object.
        let calls = data.path().join("calls");
        let helper = data.path().join("pondnet");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\necho \"$2\" >> '{}'\ncat > /dev/null\necho '{{}}'\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        // No other test in this module spawns the helper, so setting it process-wide is safe.
        std::env::set_var("POND_NETWORK_BINARY", &helper);

        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.status.write().unwrap().auth_url =
            Some("https://control.example/register/pending".into());
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/remote-access/register")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        management_with_credential(runtime)
            .oneshot(request)
            .await
            .unwrap();

        let recorded = std::fs::read_to_string(&calls).unwrap();
        let actions: Vec<&str> = recorded.lines().collect();
        assert_eq!(
            actions.get(..2),
            Some(&["identity", "register"][..]),
            "registration must ensure the household identity first: {actions:?}"
        );
    }

    #[tokio::test]
    async fn local_management_ignores_forged_forwarding_identity() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let credential = pond_api::host_guard::HostCredential::generate();
        let router = management(runtime, credential.clone());
        for ip in ["100.64.0.2:1234", "192.168.1.2:1234"] {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access")
                .header("x-forwarded-for", "127.0.0.1")
                .header(PEER_HEADER, "127.0.0.1:1234")
                .header(pond_api::host_guard::CREDENTIAL_HEADER, credential.as_str())
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
            assert_eq!(
                router.clone().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let request = Request::builder()
            .uri("/api/v1/remote-access")
            .header(pond_api::host_guard::CREDENTIAL_HEADER, credential.as_str())
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn local_management_requires_the_host_credential() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let credential = pond_api::host_guard::HostCredential::generate();
        let router = management(runtime, credential.clone());
        let request = |value: Option<&str>| {
            let mut request = Request::builder().uri("/api/v1/remote-access");
            if let Some(value) = value {
                request = request.header(pond_api::host_guard::CREDENTIAL_HEADER, value);
            }
            let mut request = request.body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            request
        };
        let stale = pond_api::host_guard::HostCredential::generate();
        for value in [None, Some(stale.as_str())] {
            assert_eq!(
                router
                    .clone()
                    .oneshot(request(value))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            router
                .oneshot(request(Some(credential.as_str())))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn embedded_pairing_remains_remote_even_with_local_forwarding_headers() {
        let router = private_companion(Router::new().route(
            "/pair",
            get(|peer: ConnectInfo<SocketAddr>| async move {
                pond_api::network::require_lan(Some(peer)).map(|_| StatusCode::OK)
            }),
        ));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/pair")
                    .header(PEER_HEADER, "100.64.0.2:1234")
                    .header("x-forwarded-for", "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn address_is_ready_only_after_certificate_coverage() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        runtime.publish(Status {
            state: "Running".into(),
            addresses: vec!["100.64.0.2".into()],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        });
        runtime.publish_ready(&["pond.local".into()]);
        assert!(runtime.address.0.read().unwrap().is_none());
        runtime.publish_ready(&["100.64.0.2".into()]);
        assert_eq!(
            runtime.address.0.read().unwrap().as_deref(),
            Some("100.64.0.2")
        );
    }

    /// A stand-in helper: records each call, and answers an action from `<action>.out` and
    /// `<action>.code` beside it (exit 0 with `{}` when neither exists).
    struct FakeHelper {
        dir: tempfile::TempDir,
    }

    impl FakeHelper {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("pondnet");
            std::fs::write(
                &script,
                "#!/bin/sh\n\
                 dir=$(dirname \"$0\")\n\
                 payload=$(cat)\n\
                 echo \"$2 $payload\" >> \"$dir/calls\"\n\
                 env > \"$dir/env\"\n\
                 if [ -f \"$dir/$2.out\" ]; then cat \"$dir/$2.out\"; else echo '{}'; fi\n\
                 exit $(cat \"$dir/$2.code\" 2>/dev/null || echo 0)\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self { dir }
        }

        fn answer(&self, action: &str, code: i32, stdout: &str) {
            std::fs::write(
                self.dir.path().join(format!("{action}.code")),
                code.to_string(),
            )
            .unwrap();
            std::fs::write(self.dir.path().join(format!("{action}.out")), stdout).unwrap();
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .map(|line| line.split(' ').next().unwrap().to_owned())
                .collect()
        }

        fn runtime(&self, data: &Path) -> Arc<Runtime> {
            let (runtime, _listener) =
                Runtime::with_helper(data, 4443, self.dir.path().join("pondnet")).unwrap();
            runtime
                .persist(&Config {
                    enabled: true,
                    control_url: "https://coord.example".into(),
                    enrollment_url: "https://enroll.example".into(),
                })
                .unwrap();
            runtime
        }
    }

    fn enrolled(runtime: &Runtime, complete: bool, devices: &[&str]) {
        revocation::write_private_json(
            &runtime.directory,
            revocation::ENROLLED_FILE,
            &revocation::Enrolled {
                complete,
                devices: devices.iter().map(|d| network_device(d).unwrap()).collect(),
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_enrolled_phone_is_revoked_and_a_device_that_never_enrolled_is_left_alone() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001"]);

        runtime.queue("phone000000000001").await.unwrap();
        // A desktop session or a Matter device: nothing enrolled, so nothing to revoke.
        runtime.queue("desktop0000000001").await.unwrap();
        let queued = runtime.pending_revocations().unwrap();
        assert_eq!(queued.len(), 1);
        assert!(!queued.values().next().unwrap().verify);

        runtime.reconcile_due().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        assert_eq!(helper.calls(), ["revoke"]);
        assert!(revocation::load_enrolled(&runtime.directory)
            .unwrap()
            .devices
            .is_empty());
    }

    #[tokio::test]
    async fn an_older_pond_checks_an_unrecorded_device_and_writes_no_tombstone() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        // No record at all: this Pond enrolled phones before it kept one.
        runtime.queue("matter-000000000001").await.unwrap();
        assert!(runtime
            .pending_revocations()
            .unwrap()
            .values()
            .all(|e| e.verify));

        helper.answer("inspect", 3, r#"{"refused":"enrollment_missing"}"#);
        runtime.reconcile_due().await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        assert_eq!(
            helper.calls(),
            ["inspect"],
            "revoked a device that never enrolled"
        );
    }

    #[tokio::test]
    async fn a_refusal_backs_off_one_device_and_an_outage_waits_for_all() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001", "phone000000000002"]);
        runtime.queue("phone000000000001").await.unwrap();
        runtime.queue("phone000000000002").await.unwrap();

        helper.answer("revoke", 3, r#"{"refused":"revocation_conflict"}"#);
        runtime.reconcile_due().await.unwrap();
        assert_eq!(
            helper.calls().len(),
            2,
            "a refusal stopped the devices behind it"
        );
        let queued = runtime.pending_revocations().unwrap();
        assert!(queued
            .values()
            .all(|e| e.attempts == 1 && e.next_attempt > Utc::now()));
        // Backed off, so the next tick sends nothing.
        runtime.reconcile_due().await.unwrap();
        assert_eq!(helper.calls().len(), 2);

        let mut due_now = runtime.pending_revocations().unwrap();
        for entry in due_now.values_mut() {
            entry.next_attempt = Utc::now();
        }
        runtime.save_revocations(&due_now).unwrap();
        helper.answer("revoke", 1, "");
        runtime.reconcile_due().await.unwrap();
        assert_eq!(
            helper.calls().len(),
            3,
            "an unreachable coordinator was tried again"
        );
        let attempts: Vec<u32> = runtime
            .pending_revocations()
            .unwrap()
            .values()
            .map(|e| e.attempts)
            .collect();
        assert_eq!(attempts.iter().filter(|a| **a == 2).count(), 1);
        assert_eq!(attempts.iter().filter(|a| **a == 1).count(), 1);
    }

    #[tokio::test]
    async fn a_new_household_records_every_enrollment_so_nothing_is_checked() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime.start_enrollment_record().unwrap();
        runtime.queue("desktop0000000001").await.unwrap();
        assert!(runtime.pending_revocations().unwrap().is_empty());
        // A second start keeps what is recorded.
        runtime
            .record_enrolled(&network_device("phone000000000001").unwrap())
            .unwrap();
        runtime.start_enrollment_record().unwrap();
        assert_eq!(
            revocation::load_enrolled(&runtime.directory)
                .unwrap()
                .devices
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_revocation_queued_during_enrollment_wins() {
        use pond_core::security::ports::remote_access::RemoteRevocation;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let device = network_device("phone000000000001").unwrap();
        runtime.record_enrolled(&device).unwrap();
        assert!(runtime.ensure_not_revoked_meanwhile(&device).is_ok());
        runtime.queue("phone000000000001").await.unwrap();
        assert!(runtime.ensure_not_revoked_meanwhile(&device).is_err());
    }

    #[tokio::test]
    async fn a_corrupt_presence_record_still_lets_enrolled_phones_lapse() {
        use pond_core::security::ports::remote_access::LAN_PRESENCE_WINDOW_DAYS;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        enrolled(&runtime, true, &["phone000000000001"]);
        let path = runtime.directory.join(PRESENCE_FILE);
        std::fs::write(&path, b"{not json").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let written = std::time::SystemTime::now()
            - std::time::Duration::from_secs(60 * 60 * 24 * (LAN_PRESENCE_WINDOW_DAYS as u64 + 1));
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(written)
            .unwrap();

        runtime.sweep_absent_devices().await.unwrap();
        let queued = runtime.pending_revocations().unwrap();
        assert!(queued.contains_key(&network_device("phone000000000001").unwrap()));
        let aside = std::fs::read_dir(&runtime.directory)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("presence.json.corrupt-")
            });
        assert!(aside, "the unreadable record was not kept for inspection");
    }

    #[tokio::test]
    async fn sightings_are_written_at_most_hourly_and_read_from_older_keys() {
        use pond_core::security::ports::remote_access::DevicePresence;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        // A record from before sightings were hashed.
        let old = (Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        runtime
            .save_presence(&BTreeMap::from([(
                "phone000000000001".to_owned(),
                old.clone(),
            )]))
            .unwrap();
        assert!(runtime
            .lapses_at("phone000000000001")
            .await
            .unwrap()
            .is_some());

        runtime.seen_on_lan("phone000000000001").await;
        let first = runtime.presence().unwrap();
        let device = network_device("phone000000000001").unwrap();
        assert_ne!(first[&device], old);
        // Put the old sighting back; within the hour a second request must not rewrite it.
        runtime
            .save_presence(&BTreeMap::from([(device.clone(), old.clone())]))
            .unwrap();
        runtime.seen_on_lan("phone000000000001").await;
        assert_eq!(runtime.presence().unwrap()[&device], old);
    }

    #[tokio::test]
    async fn the_helper_inherits_no_environment_and_must_not_be_replaceable() {
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime
            .authority("inspect", serde_json::json!({}))
            .await
            .unwrap();
        let env = std::fs::read_to_string(helper.dir.path().join("env")).unwrap();
        // Cargo sets these for every test process; none may reach the helper.
        for inherited in ["CARGO_PKG_NAME=", "RUST_TEST_THREADS=", "\nPATH="] {
            assert!(
                !format!("\n{env}").contains(inherited),
                "{inherited} reached the helper"
            );
        }

        let script = helper.dir.path().join("pondnet");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o720)).unwrap();
        let refused = runtime.authority("inspect", serde_json::json!({})).await;
        assert!(
            refused.is_err_and(|e| e.to_string().contains("writable by no one else")),
            "a group-writable helper was run"
        );
    }

    #[test]
    fn a_registration_names_well_formed_keys() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let registration = |node: &str, machine: &str| Registration {
            auth_url: "https://coord.example/register/abcdefghijklmnop".into(),
            node_key: node.into(),
            machine_key: machine.into(),
        };
        let machine = format!("mkey:{}", "a".repeat(64));
        let node = format!("nodekey:{}", "b".repeat(64));
        for (node_key, machine_key, ok) in [
            (node.as_str(), machine.as_str(), true),
            ("", machine.as_str(), true),
            (node.as_str(), "", false),
            (node.as_str(), "mkey:ABC", false),
            (node.as_str(), &format!("mkey:{}", "A".repeat(64)), false),
            ("nodekey:1", machine.as_str(), false),
        ] {
            let payload = runtime.registration_payload(
                "device0000000001",
                "phone",
                &registration(node_key, machine_key),
            );
            assert_eq!(payload.is_ok(), ok, "{node_key:?} {machine_key:?}");
        }
    }

    #[tokio::test]
    async fn the_dashboard_learns_whether_the_pond_was_provisioned() {
        use std::os::unix::fs::PermissionsExt;
        use tower::ServiceExt;
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let ask = |runtime: Arc<Runtime>| async move {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access/device")
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = management_with_credential(runtime)
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };
        assert_eq!(
            ask(runtime.clone()).await,
            serde_json::json!({ "provisioned": false, "registered": false })
        );

        let device = runtime.directory.join("device");
        std::fs::create_dir(&device).unwrap();
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o700)).unwrap();
        use base64::Engine;
        let serial = "0f".repeat(16);
        let payload = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!({
                "v": 1, "serial": serial, "devicePublicKey": "k", "issued": 1
            }))
            .unwrap(),
        );
        let certificate = device.join("certificate.json");
        std::fs::write(
            &certificate,
            serde_json::to_vec(&serde_json::json!({ "payload": payload, "signature": "s" }))
                .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&certificate, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            ask(runtime.clone()).await,
            serde_json::json!({ "provisioned": true, "serial": serial, "registered": false })
        );

        // A certificate others can read is not shown as a provisioning.
        std::fs::set_permissions(&certificate, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            ask(runtime).await,
            serde_json::json!({ "provisioned": false, "registered": false })
        );
    }

    #[tokio::test]
    async fn a_refused_device_certificate_is_explained_to_the_dashboard() {
        use tower::ServiceExt;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime.publish(Status {
            state: "NeedsLogin".into(),
            addresses: vec![],
            auth_url: Some("https://coord.example/register/abcdefghijklmnop".into()),
            node_key: format!("nodekey:{}", "b".repeat(64)),
            machine_key: format!("mkey:{}", "a".repeat(64)),
        });
        for reason in [
            "device_certificate_invalid",
            "device_revoked",
            "device_used",
        ] {
            helper.answer("register", 3, &format!(r#"{{"refused":"{reason}"}}"#));
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/remote-access/register")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = management_with_credential(runtime.clone())
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{reason}");
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"], reason);
        }
    }

    #[tokio::test]
    async fn registration_carries_the_invite_and_explains_its_refusal() {
        use tower::ServiceExt;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        runtime.publish(Status {
            state: "NeedsLogin".into(),
            addresses: vec![],
            auth_url: Some("https://coord.example/register/abcdefghijklmnop".into()),
            node_key: format!("nodekey:{}", "b".repeat(64)),
            machine_key: format!("mkey:{}", "a".repeat(64)),
        });
        let router = management_with_credential(runtime);
        let register = |body: &str| {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/remote-access/register")
                .header("content-type", "application/json")
                .body(Body::from(body.to_owned()))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            router.clone().oneshot(request)
        };
        let body = |response: axum::response::Response| async {
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };

        let response = register(r#"{"invite":"not an invite"}"#).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            helper.calls().is_empty(),
            "a malformed invite reached the helper"
        );

        helper.answer("register", 3, r#"{"refused":"invite_expired"}"#);
        let response = register(r#"{"invite":"giap-inv1-ABCD-EFGH"}"#)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body(response).await["error"], "invite_expired");
        let sent = std::fs::read_to_string(helper.dir.path().join("calls")).unwrap();
        assert!(
            sent.contains(r#"register {"invite":"giap-inv1-ABCD-EFGH"}"#),
            "{sent}"
        );

        // A household already registered sends no invite at all.
        assert!(
            !data
                .path()
                .join("embedded-network")
                .join(REGISTERED_FILE)
                .exists(),
            "a refusal was recorded as a registration"
        );
        helper.answer("register", 0, r#"{"household":"h"}"#);
        let response = register("{}").await.unwrap();
        assert_ne!(response.status(), StatusCode::FORBIDDEN);
        assert!(std::fs::read_to_string(helper.dir.path().join("calls"))
            .unwrap()
            .contains(r#"register {"invite":""}"#));
        let recorded: serde_json::Value = serde_json::from_slice(
            &std::fs::read(data.path().join("embedded-network").join(REGISTERED_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            recorded,
            serde_json::json!({ "enrollment": "https://enroll.example" })
        );
    }

    /// The dashboard asks for an invite only while the household has never registered with the
    /// coordination service configured now. A node that reached Running proves it registered,
    /// which is how a household that registered before the record existed gets one.
    #[tokio::test]
    async fn a_registered_household_is_not_asked_for_an_invite_again() {
        use std::os::unix::fs::PermissionsExt;
        use tower::ServiceExt;
        let helper = FakeHelper::new();
        let data = tempfile::tempdir().unwrap();
        let runtime = helper.runtime(data.path());
        let ask = |runtime: Arc<Runtime>| async move {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access/device")
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            let response = management_with_credential(runtime)
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["registered"].clone()
        };
        let state = |state: &str| Status {
            state: state.into(),
            addresses: vec![],
            auth_url: None,
            node_key: String::new(),
            machine_key: String::new(),
        };
        assert_eq!(ask(runtime.clone()).await, false);

        runtime.publish(state("Starting"));
        assert_eq!(ask(runtime.clone()).await, false, "starting proves nothing");
        runtime.publish(state("Running"));
        assert_eq!(ask(runtime.clone()).await, true);
        let record = runtime.directory.join(REGISTERED_FILE);
        let mode = std::fs::metadata(&record).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "the record is readable by others: {mode:o}"
        );

        // Another coordination service has never seen this household.
        runtime
            .persist(&Config {
                enabled: true,
                control_url: "https://elsewhere.example".into(),
                enrollment_url: "https://enroll.elsewhere.example".into(),
            })
            .unwrap();
        assert_eq!(ask(runtime.clone()).await, false);
        runtime.publish(state("Stopped"));
        runtime.publish(state("Running"));
        assert_eq!(ask(runtime.clone()).await, true);

        // An unreadable record is not taken as a registration.
        std::fs::write(&record, b"not json").unwrap();
        assert_eq!(ask(runtime).await, false);
    }
}
