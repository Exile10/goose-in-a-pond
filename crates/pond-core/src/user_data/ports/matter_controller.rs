//! Driven Port: the Matter controller as a *process*, not as a protocol.
//!
//! [`device_commissioning`](super::device_commissioning) and
//! [`device_control`](super::device_control) speak to a controller. This port is
//! about the controller itself: where the one GIAP is talking to came from, how
//! long it has been up, and whether GIAP may restart it.
//!
//! It exists because reuse is silent. GIAP adopts anything already listening on
//! the controller port, which is deliberate — a user may run their own server,
//! and a controller left behind by an earlier Pond is worth keeping so the
//! fabric is not disturbed. But a long-lived controller can answer its port and
//! serve WebSocket clients perfectly while its mDNS state has gone stale, at
//! which point every commissioning attempt fails with a discovery timeout and
//! nothing in GIAP says why. Restarting the Pond does not help: the controller
//! is reused again, exactly as before.
//!
//! There is no honest health check to put here. The controller's own
//! `discover_commissionable_nodes` returns an empty list rather than an error
//! when its mDNS is dead, so it cannot distinguish a broken stack from a network
//! with nothing in pairing mode — a probe built on it would report healthy for
//! the very failure it was meant to catch. So this port reports provenance and
//! age, which are facts, and offers the remedy, instead of guessing at health.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Where the controller GIAP is currently talking to came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerOrigin {
    /// GIAP spawned it during this run.
    StartedByPond,
    /// It was already listening, and it is provably GIAP's own — a controller
    /// left behind by an earlier run, identified by the storage path it was
    /// started with. This is the one that goes stale unnoticed.
    AdoptedFromEarlierRun,
    /// It was already listening and is not GIAP's: the user's own server, or a
    /// remote one. Never restarted — GIAP does not manage what it did not start.
    External,
}

impl ControllerOrigin {
    /// One clause for a log line or a UI caption.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::StartedByPond => "started by this Pond",
            Self::AdoptedFromEarlierRun => "left running by an earlier Pond and adopted",
            Self::External => "not managed by this Pond",
        }
    }
}

/// What is known about the controller process behind the configured URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerStatus {
    pub origin: ControllerOrigin,
    /// The controller process, when it is a local one GIAP could identify.
    pub pid: Option<u32>,
    /// How long that process has been up. The number that matters: a controller
    /// running for days across network changes is the one worth suspecting.
    pub uptime_secs: Option<u64>,
    /// Whether [`MatterControllerAdmin::restart`] will act rather than refuse.
    pub restartable: bool,
}

impl ControllerStatus {
    /// A controller GIAP did not start is reported as-is and never restarted.
    pub fn external() -> Self {
        Self {
            origin: ControllerOrigin::External,
            pid: None,
            uptime_secs: None,
            restartable: false,
        }
    }

    /// A local controller GIAP owns, so restarting it is GIAP's to offer.
    pub fn owned(origin: ControllerOrigin, pid: u32, uptime_secs: u64) -> Self {
        Self {
            origin,
            pid: Some(pid),
            uptime_secs: Some(uptime_secs),
            restartable: true,
        }
    }
}

/// Driven Port: inspect and restart the local Matter controller.
#[async_trait]
pub trait MatterControllerAdmin: Send + Sync {
    /// Provenance and age of the controller behind the configured URL.
    async fn status(&self) -> ControllerStatus;

    /// Stop the controller and start a fresh one, so a stale network stack is
    /// cleared without the user resorting to `pkill`.
    ///
    /// The fabric is unaffected: commissioned nodes live in the controller's
    /// storage directory, not in the process. Errors when the controller is not
    /// GIAP's to restart — refusing is the point, not a limitation.
    async fn restart(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_external_controller_is_never_restartable() {
        let s = ControllerStatus::external();
        assert!(!s.restartable);
        // Nothing is claimed about a process GIAP did not start.
        assert_eq!(s.pid, None);
        assert_eq!(s.uptime_secs, None);
    }

    #[test]
    fn an_owned_controller_carries_the_age_that_makes_it_suspect() {
        let s = ControllerStatus::owned(ControllerOrigin::AdoptedFromEarlierRun, 4242, 97_200);
        assert!(s.restartable);
        assert_eq!(s.pid, Some(4242));
        assert_eq!(s.uptime_secs, Some(97_200));
    }

    #[test]
    fn each_origin_reads_differently_so_a_log_line_is_unambiguous() {
        let all = [
            ControllerOrigin::StartedByPond,
            ControllerOrigin::AdoptedFromEarlierRun,
            ControllerOrigin::External,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(!a.describe().is_empty());
            for b in &all[i + 1..] {
                assert_ne!(a.describe(), b.describe());
            }
        }
    }

    #[test]
    fn origin_serialises_as_snake_case_for_the_api() {
        let json = serde_json::to_string(&ControllerOrigin::AdoptedFromEarlierRun).unwrap();
        assert_eq!(json, "\"adopted_from_earlier_run\"");
    }
}
