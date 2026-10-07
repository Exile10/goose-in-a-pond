//! The development-only plaintext LAN listener, for iterating on the companion app from Expo Go.
//!
//! Expo Go has no native module to pin the Pond's TLS key, so it cannot reach the HTTPS
//! companion listener. This listener serves the companion API over plain HTTP instead. It is
//! plaintext and unpinned: anyone on the network can read every request, including bearer
//! tokens and pairing transcripts, and can impersonate the Pond to the phone or the phone to
//! the Pond. It therefore exists only in debug builds, only when asked for by name, and says so
//! in the log for as long as it runs. See `docs/auth-network-posture.md`.

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use axum::{Extension, Router};

/// Set to `1` to ask a debug build for the plaintext listener. Release builds ignore it.
pub const ENABLE_VAR: &str = "POND_DEV_INSECURE_LAN";
/// The port the plaintext listener binds on every interface; [`DEFAULT_PORT`] when unset.
pub const PORT_VAR: &str = "POND_DEV_INSECURE_LAN_PORT";
/// The plaintext listener's port when [`PORT_VAR`] is unset.
pub const DEFAULT_PORT: u16 = 4080;
/// How often the running listener repeats its warning, so it cannot scroll out of mind.
pub const REMINDER_INTERVAL: Duration = Duration::from_secs(600);

/// Whether this process runs the plaintext listener, decided once at startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The variable is unset or `0`: nothing extra is opened.
    Off,
    /// The variable is set but this is a release binary, which never opens the listener.
    IgnoredInRelease,
    /// A debug binary asked for the listener on this port.
    On {
        /// The port to bind on every interface.
        port: u16,
    },
}

/// Process-wide marker: the plaintext listener is running on this port.
///
/// Layered onto every router so `/system/info` can report `insecure_dev` on any of them.
#[derive(Clone, Copy, Debug)]
pub struct InsecureDevLan {
    /// The plaintext listener's port.
    pub port: u16,
}

/// Per-request marker: this request arrived on the plaintext listener, not over pinned TLS.
///
/// Inserted only by [`router`], so its presence is a fact about the socket, never about
/// anything the client sent.
#[derive(Clone, Copy, Debug)]
pub struct InsecureDevTransport;

/// Decide whether to open the plaintext listener.
///
/// Both conditions are required: `debug_build` (the caller passes `cfg!(debug_assertions)`)
/// and `enable == Some("1")`. A release binary with the variable set at all gets
/// [`Decision::IgnoredInRelease`] without its value being parsed, so no spelling of it can
/// open a port there. In a debug build any value other than unset, empty, `0` or `1` is an
/// error rather than a silent "off", as is a port that is not a number in `1..=65535`.
pub fn decide(
    enable: Option<&str>,
    port: Option<&str>,
    debug_build: bool,
) -> anyhow::Result<Decision> {
    let Some(enable) = enable.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(Decision::Off);
    };
    if !debug_build {
        return Ok(Decision::IgnoredInRelease);
    }
    match enable {
        "0" => Ok(Decision::Off),
        "1" => {
            let port = match port.map(str::trim).filter(|v| !v.is_empty()) {
                None => DEFAULT_PORT,
                Some(raw) => raw
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .with_context(|| format!("{PORT_VAR}={raw:?} is not a port in 1..=65535"))?,
            };
            Ok(Decision::On { port })
        }
        other => anyhow::bail!("{ENABLE_VAR}={other:?} is not understood; set it to 1 or unset it"),
    }
}

/// [`decide`] from the process environment and this binary's build profile.
///
/// A release binary that was asked for the listener logs a WARN saying it was ignored, so a
/// developer who deployed the wrong binary finds out from the log rather than from a phone
/// that cannot connect.
pub fn from_env() -> anyhow::Result<Decision> {
    let decision = decide(
        std::env::var(ENABLE_VAR).ok().as_deref(),
        std::env::var(PORT_VAR).ok().as_deref(),
        cfg!(debug_assertions),
    )?;
    if decision == Decision::IgnoredInRelease {
        tracing::warn!(
            kind = "insecure_dev_lan_ignored",
            "{ENABLE_VAR} is set, but this is a release build: no plaintext listener was opened"
        );
    }
    Ok(decision)
}

impl Decision {
    /// The marker to layer onto every router, when the listener is on.
    pub fn marker(self) -> Option<InsecureDevLan> {
        match self {
            Decision::On { port } => Some(InsecureDevLan { port }),
            Decision::Off | Decision::IgnoredInRelease => None,
        }
    }
}

/// Layer the process-wide [`InsecureDevLan`] marker onto `router` when the listener is on.
pub fn advertise(router: Router, lan: Option<InsecureDevLan>) -> Router {
    match lan {
        Some(lan) => router.layer(Extension(lan)),
        None => router,
    }
}

/// The plaintext listener's router: the companion router it is given, with every request
/// marked [`InsecureDevTransport`]. Never pass it the dashboard router.
pub fn router(companion: Router) -> Router {
    companion.layer(Extension(InsecureDevTransport))
}

/// Bind the plaintext listener on every interface, or nothing when the decision is not
/// [`Decision::On`]. The exact port is bound, with no fallback: the app is pointed at it.
pub async fn bind(decision: Decision) -> anyhow::Result<Option<tokio::net::TcpListener>> {
    let Some(InsecureDevLan { port }) = decision.marker() else {
        return Ok(None);
    };
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("binding the insecure development listener on port {port}"))?;
    Ok(Some(listener))
}

fn warn_running(port: u16) {
    tracing::warn!(
        kind = "insecure_dev_lan",
        port,
        "insecure development listener is serving the companion API over plaintext HTTP on every \
         interface: unpinned, development only, and anyone on this network can read and \
         impersonate its traffic"
    );
}

/// Serve `router` on the plaintext listener, warning at once and every [`REMINDER_INTERVAL`].
///
/// With no listener this never resolves, so the caller can race it beside the real listeners.
pub async fn serve(
    listener: Option<tokio::net::TcpListener>,
    router: Router,
) -> anyhow::Result<()> {
    let Some(listener) = listener else {
        return std::future::pending().await;
    };
    let port = listener.local_addr()?.port();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .into_future();
    tokio::pin!(server);
    // The first tick is immediate: that is the startup warning.
    let mut reminders = tokio::time::interval(REMINDER_INTERVAL);
    loop {
        tokio::select! {
            result = &mut server => return result.context("insecure development listener stopped"),
            _ = reminders.tick() => warn_running(port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_binary_ignores_the_flag_whatever_its_value() {
        for value in ["1", "true", "0", "anything"] {
            assert_eq!(
                decide(Some(value), Some("not a port"), false).unwrap(),
                Decision::IgnoredInRelease,
                "{value}"
            );
        }
        assert_eq!(decide(None, None, false).unwrap(), Decision::Off);
    }

    #[test]
    fn a_debug_binary_needs_the_flag() {
        assert_eq!(decide(None, None, true).unwrap(), Decision::Off);
        assert_eq!(decide(Some(""), Some("5000"), true).unwrap(), Decision::Off);
        assert_eq!(decide(Some("0"), None, true).unwrap(), Decision::Off);
        assert_eq!(
            decide(Some("1"), None, true).unwrap(),
            Decision::On { port: DEFAULT_PORT }
        );
        assert_eq!(
            decide(Some("1"), Some("4999"), true).unwrap(),
            Decision::On { port: 4999 }
        );
    }

    #[test]
    fn a_debug_binary_refuses_values_it_does_not_understand() {
        assert!(decide(Some("true"), None, true).is_err());
        for port in ["0", "65536", "http", "-1"] {
            assert!(decide(Some("1"), Some(port), true).is_err(), "{port}");
        }
    }

    #[test]
    fn only_an_enabled_decision_carries_a_marker() {
        assert!(Decision::Off.marker().is_none());
        assert!(Decision::IgnoredInRelease.marker().is_none());
        assert_eq!(Decision::On { port: 4080 }.marker().unwrap().port, 4080);
    }

    #[tokio::test]
    async fn no_listener_is_bound_unless_enabled() {
        assert!(bind(Decision::Off).await.unwrap().is_none());
        assert!(bind(Decision::IgnoredInRelease).await.unwrap().is_none());
        // Port 0 cannot come from `decide`; here it asks the OS for any free port.
        let listener = bind(Decision::On { port: 0 })
            .await
            .unwrap()
            .expect("bound");
        assert!(listener.local_addr().unwrap().ip().is_unspecified());
    }
}
