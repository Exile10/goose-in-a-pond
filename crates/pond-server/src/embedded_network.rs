//! Bundled userspace networking lifecycle and its private companion boundary.
//! The public listeners never accept the embedded peer header.

use anyhow::{ensure, Context, Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
    routing::get,
    Json, Router,
};
use pond_api::network::{is_tailnet, EmbeddedAddress};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
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

const PEER_HEADER: &str = "x-pond-embedded-peer";

/// The coordination and enrollment services a household uses when it has not
/// chosen its own.
///
/// A household should not have to know what a Headscale origin is to reach its
/// own Pond from outside the house, so enabling remote access without naming a
/// coordinator uses these. They are substituted when the user enables remote
/// access, never when configuration is read: an empty control URL is what
/// distinguishes a local-only household, and defaulting on read would make every
/// such household start contacting coordination and start advertising a
/// coordinator to its paired phones.
///
/// Self-hosting stays supported: an explicitly configured origin is used as given
/// and never replaced.
pub const DEFAULT_CONTROL_URL: &str = "https://control.jarida.io";
pub const DEFAULT_ENROLLMENT_URL: &str = "https://enroll.jarida.io";

/// Fill in the hosted coordinator for a household that named none.
///
/// Both origins move together. A household that set one and not the other has
/// configured something deliberate and half-finished, and quietly completing it
/// from the other side would point it at a coordinator it never chose; the
/// existing validation rejects that instead.
fn with_default_coordinator(mut config: Config) -> Config {
    if config.control_url.is_empty() && config.enrollment_url.is_empty() {
        config.control_url = DEFAULT_CONTROL_URL.to_string();
        config.enrollment_url = DEFAULT_ENROLLMENT_URL.to_string();
    }
    config
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
    status: RwLock<Status>,
    process: Mutex<Option<Process>>,
    generation: AtomicU64,
    revocations: Mutex<()>,
    recovery: recovery::Queue,
    /// Address published by system information and pairing QR producers.
    pub address: EmbeddedAddress,
    /// Wake certificate renewal as soon as the node obtains an address.
    pub changed: tokio::sync::Notify,
}

impl Runtime {
    /// Create the private bridge. A short random socket path also fits macOS's
    /// sockaddr_un limit when the application's data-directory path is long.
    pub fn new(data: &Path, port: u16) -> Result<(Arc<Self>, tokio::net::UnixListener)> {
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
                status: RwLock::new(Status {
                    state: "Stopped".into(),
                    addresses: vec![],
                    auth_url: None,
                    node_key: String::new(),
                    machine_key: String::new(),
                }),
                process: Mutex::new(None),
                generation: AtomicU64::new(0),
                revocations: Mutex::new(()),
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

    async fn authority(
        &self,
        action: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        use tokio::io::AsyncWriteExt;
        let config = self.config()?;
        let helper = std::env::var_os("POND_NETWORK_BINARY")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_exe()?.with_file_name("pondnet"));
        let mut child = Command::new(helper)
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
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(mut input) = child.stdin.take() {
            input.write_all(&serde_json::to_vec(&payload)?).await?;
        }
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(25), child.wait_with_output())
                .await??;
        ensure!(
            output.status.success() && output.stdout.len() < 8192,
            "household enrollment operation failed"
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
        let _revocations = self.revocations.lock().await;
        ensure!(
            !self.pending_revocations()?.contains(&device),
            "device revocation is pending"
        );
        let payload = self.registration_payload(&device, role, registration)?;
        self.authority("enroll", payload).await
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
        Ok(
            serde_json::json!({"household":"", "device":device,"role":role,"action":"enroll","authId":auth_id,"nodeKey":registration.node_key,"machineKey":registration.machine_key,"nonce":"","expires":0}),
        )
    }

    fn pending_revocations(&self) -> Result<BTreeSet<String>> {
        let path = self.directory.join("revocations.json");
        let metadata = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeSet::new())
            }
            other => other?,
        };
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().mode() & 0o077 == 0
                && metadata.len() <= 65536,
            "invalid remote revocation queue"
        );
        let pending: BTreeSet<String> = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            pending.len() <= 256 && pending.iter().all(|id| valid_device(id)),
            "invalid remote revocation entries"
        );
        Ok(pending)
    }

    fn save_revocations(&self, pending: &BTreeSet<String>) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
        file.write_all(&serde_json::to_vec(pending)?)?;
        file.as_file().sync_all()?;
        file.persist(self.directory.join("revocations.json"))?;
        std::fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    /// Retry durable revocations at a bounded rate, including while networking is disabled.
    /// Dropping this future on shutdown cancels the current helper operation.
    pub async fn reconcile_revocations(&self) -> Result<()> {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(30));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            let pending = {
                let _guard = self.revocations.lock().await;
                self.pending_revocations()?
            };
            for device in pending.iter().take(4) {
                let _guard = self.revocations.lock().await;
                if !self.pending_revocations()?.contains(device) {
                    continue;
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
                    Ok(_) => {
                        let mut current = self.pending_revocations()?;
                        current.remove(device);
                        self.save_revocations(&current)?;
                        tracing::info!("queued remote network revocation completed");
                    }
                    Err(_) => {
                        tracing::warn!(
                            "remote network revocation remains queued; coordination unavailable"
                        );
                        break;
                    }
                }
            }
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
        let helper = std::env::var_os("POND_NETWORK_BINARY")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_exe()?.with_file_name("pondnet"));
        ensure!(
            helper.is_file(),
            "bundled pondnet helper is missing; build native/pondnet first"
        );
        let mut child = Command::new(helper)
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
            .env_remove("TS_AUTHKEY")
            .env_remove("TS_CLIENT_ID")
            .env_remove("TS_CLIENT_SECRET")
            .env_remove("TS_ID_TOKEN")
            .env_remove("TS_AUDIENCE")
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

fn valid_device(id: &str) -> bool {
    (16..=80).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[async_trait::async_trait]
impl pond_core::security::ports::remote_access::RemoteRevocation for Runtime {
    async fn queue(&self, device_id: &str) -> Result<()> {
        let _guard = self.revocations.lock().await;
        // A local-only household has never registered a node and must not contact coordination.
        if self.config()?.enrollment_url.is_empty() {
            return Ok(());
        }
        let device_id = network_device(device_id)?;
        self.recovery.revoke(&device_id);
        let mut pending = self.pending_revocations()?;
        ensure!(
            pending.contains(&device_id) || pending.len() < 256,
            "remote revocation queue is full"
        );
        pending.insert(device_id);
        self.save_revocations(&pending)?;
        tracing::info!("remote network revocation queued durably");
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

/// Management is available only on the loopback listener, guarded by socket peer.
pub fn management(runtime: Arc<Runtime>) -> Router {
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
        .merge(recovery::local_routes())
        .with_state(runtime)
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

async fn authority_identity(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    runtime
        .authority("identity", serde_json::Value::Null)
        .await
        .map(Json)
        .map_err(|_| {
            tracing::warn!("embedded enrollment operation failed");
            StatusCode::SERVICE_UNAVAILABLE
        })
}
async fn register_pond(
    State(runtime): State<Arc<Runtime>>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    local(peer)?;
    let current = runtime
        .status
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let registration = Registration {
        auth_url: current.auth_url.ok_or(StatusCode::CONFLICT)?,
        node_key: current.node_key,
        machine_key: current.machine_key,
    };
    // Introduce the household first. A household that an operator created
    // already exists and this answers with it, so the two paths converge here
    // and a household nobody provisioned can still set itself up.
    if let Err(error) = runtime.authority("register", serde_json::Value::Null).await {
        tracing::warn!(%error, "household registration failed");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    runtime
        .enroll("pond000000000001", "pond", &registration)
        .await
        .map(Json)
        .map_err(|_| {
            tracing::warn!("embedded enrollment operation failed");
            StatusCode::SERVICE_UNAVAILABLE
        })
}
async fn remote_configuration(
    State(runtime): State<Arc<Runtime>>,
    axum::Extension(principal): axum::Extension<pond_core::security::ports::policy::Principal>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let device =
        pond_core::security::domain::proven_device::ProvenDevice::from_principal(&principal);
    device.id().ok_or(StatusCode::FORBIDDEN)?;
    let config = runtime.config().map_err(|_| {
        tracing::warn!("embedded enrollment operation failed");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok(Json(
        serde_json::json!({"enabled":config.enabled,"controlUrl":config.control_url,"state":runtime.status.read().unwrap_or_else(|p|p.into_inner()).state}),
    ))
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
    let _guard = runtime.revocations.lock().await;
    let (device, _) = recovery::caller(&headers, handshake.as_ref()).await?;
    if runtime
        .pending_revocations()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .contains(&device)
    {
        return Err(StatusCode::CONFLICT);
    }
    let payload = runtime
        .registration_payload(&device, "phone", &registration)
        .map_err(|_| StatusCode::CONFLICT)?;
    runtime
        .authority("enroll", payload)
        .await
        .map(Json)
        .map_err(|_| {
            tracing::warn!("embedded enrollment operation failed");
            StatusCode::SERVICE_UNAVAILABLE
        })
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
        .merge(recovery::phone_routes(state.handshake.clone()))
        .with_state(runtime)
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(axum::Extension(state.handshake.clone()))
        .layer(middleware::from_fn_with_state(
            state,
            pond_api::middleware::auth_middleware,
        ))
        .layer(middleware::from_fn(move |request: Request, next: Next| {
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
        // Completing this from the other side would point the household at a
        // coordinator it never chose. start() rejects it instead.
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
        // The default must not reach configuration on disk: an empty control URL
        // is what marks a household local-only, and queue() relies on it to stay
        // silent.
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let stored = runtime.config().unwrap();
        assert!(!stored.enabled);
        assert!(stored.control_url.is_empty());
        assert!(stored.enrollment_url.is_empty());
    }
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

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
            .contains(&network_device("phone000000000001").unwrap()));
        std::fs::write(restored.directory.join("revocations.json"), b"corrupt").unwrap();
        assert!(restored.queue("phone000000000002").await.is_err());
    }

    #[tokio::test]
    async fn local_management_ignores_forged_forwarding_identity() {
        let data = tempfile::tempdir().unwrap();
        let (runtime, _listener) = Runtime::new(data.path(), 4443).unwrap();
        let router = management(runtime);
        for ip in ["100.64.0.2:1234", "192.168.1.2:1234"] {
            let mut request = Request::builder()
                .uri("/api/v1/remote-access")
                .header("x-forwarded-for", "127.0.0.1")
                .header(PEER_HEADER, "127.0.0.1:1234")
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
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
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
}
