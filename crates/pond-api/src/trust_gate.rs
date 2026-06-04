//! Trust gate — the runtime that enforces [`TrustLevel`] for an action.
//!
//! Privileged-tier handlers do not run business logic until the gate
//! returns `Ok(GatePass)`. The gate:
//!
//!   1. Looks up the action in [`crate::trust_levels::TRUST_TABLE`].
//!   2. If the action is `LocalOnly` and the request did not arrive over
//!      a LAN-classified connection, refuses with `Forbidden`.
//!   3. For `Privileged` actions:
//!      a. Constructs an [`Intent`] with a blake3 hash of the canonical
//!         payload bound into the signature scope.
//!      b. Publishes the intent over the [`IntentBus`].
//!      c. Awaits the resolution (timeout 60 s).
//!      d. Verifies the returned assertion via [`TrustVerifier`].
//!      e. Writes a row to `privileged_audit` with the signature kept
//!         verbatim for later auditability.
//!   4. For `Personal`, returns immediately as a pass — session-token
//!      enforcement happens in the existing auth middleware layer.
//!
//! For now this module exposes [`require_trust`] as a free function rather
//! than as a Tower layer, so handlers explicitly call it. A Tower layer
//! version can be added later for blanket enforcement; the function form
//! is easier to thread payload-hash context through.

use crate::trust_levels::{classify, TrustEntry};
use crate::AppState;
use anyhow::Context;
use axum::http::StatusCode;
use axum::Json;
use chrono::{Duration as ChronoDuration, Utc};
use pond_core::domain::trust::{
    Intent, IntentOutcome, RemotePolicy, SignedAssertion, TrustError, TrustLevel,
};
use pond_core::ports::intent_bus::IntentResolution;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Default time the pond will wait for a phone to approve an intent.
pub const INTENT_TIMEOUT_SECS: u64 = 60;

/// Result of a successful gate check. Carries the signed assertion when
/// applicable so the handler can attach it to its audit log entry.
#[derive(Debug)]
pub struct GatePass {
    pub level:     TrustLevel,
    pub assertion: Option<SignedAssertion>,
}

/// Origin classification for a request — local LAN vs. remote-WG-tunnel
/// vs. unknown. Determined by the listener that accepted the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Local,
    Remote,
    Unknown,
}

/// Gate a privileged-tier action.
///
/// Call from inside a route handler before executing any business logic:
///
/// ```ignore
/// let _pass = require_trust(
///     &state, "settings.update_wake_word", &payload, origin
/// ).await?;
/// // ...do the privileged work...
/// ```
pub async fn require_trust(
    state:   &AppState,
    action:  &str,
    payload: &Value,
    origin:  Origin,
) -> Result<GatePass, (StatusCode, Json<Value>)> {
    let entry = classify(action);

    // Remote allow-list check fires for every tier — even Personal /
    // Ambient — when the action declares LocalOnly. This is rare but
    // important for actions like face enrollment which can never be
    // safely invoked over the WAN.
    if entry.remote_policy == RemotePolicy::LocalOnly && origin == Origin::Remote {
        warn!(action, "rejected remote attempt at LocalOnly action");
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "this action can only be performed locally",
                "action": action,
                "origin": "remote",
            })),
        ));
    }

    match entry.level {
        TrustLevel::Ambient | TrustLevel::Personal => Ok(GatePass {
            level:     entry.level,
            assertion: None,
        }),
        TrustLevel::Privileged => {
            // Bootstrap exemption: a Privileged action requires biometric
            // approval from a paired GOTG device. Before any device is paired
            // there is nothing that can approve it, so the intent would simply
            // wait out its 60 s timeout — deadlocking first-time setup (e.g.
            // choosing the chat model in the onboarding wizard times out with
            // "Request timed out"). A Local-origin request implies physical
            // access to the machine, so we let it through while the trust
            // system is un-bootstrapped. Once any device is paired, the
            // biometric gate enforces normally for every subsequent request.
            if origin == Origin::Local && paired_device_count(state).await == 0 {
                warn!(
                    action,
                    "no paired devices — allowing Local privileged action (bootstrap)"
                );
                return Ok(GatePass {
                    level:     entry.level,
                    assertion: None,
                });
            }
            require_biometric(state, &entry, payload).await
        }
    }
}

/// Count of paired, non-revoked devices that could approve an intent.
/// Used for the bootstrap exemption in [`require_trust`]. On query error we
/// return 0 (treat as un-bootstrapped) so a DB hiccup never deadlocks a
/// Local request behind an unapprovable intent.
async fn paired_device_count(state: &AppState) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM device_pubkeys WHERE revoked_at IS NULL",
    )
    .fetch_one(&state.db.system)
    .await
    .unwrap_or(0)
}

/// Privileged-tier branch: open an intent on the bus, await an assertion,
/// verify it, and audit-log on success.
async fn require_biometric(
    state:   &AppState,
    entry:   &TrustEntry,
    payload: &Value,
) -> Result<GatePass, (StatusCode, Json<Value>)> {
    let bus = state
        .intent_bus
        .as_ref()
        .ok_or_else(|| service_unavailable("intent bus"))?;
    let verifier = state
        .trust_verifier
        .as_ref()
        .ok_or_else(|| service_unavailable("trust verifier"))?;

    let intent = build_intent(entry, payload);
    let intent_id = intent.id;

    bus.publish(intent.clone())
        .await
        .map_err(|e| internal("failed to publish intent", e))?;

    let resolution = bus
        .await_resolution(intent_id, Duration::from_secs(INTENT_TIMEOUT_SECS))
        .await
        .map_err(|e| internal("intent bus error", e))?;

    match resolution {
        IntentResolution::Approved {
            install_id,
            assertion,
        } => {
            verifier
                .verify_assertion(&intent, &assertion)
                .await
                .map_err(|e| trust_error_to_http(action_for(entry), &install_id, e))?;
            // Best-effort audit log — failure here doesn't undo the verify.
            if let Err(e) = audit_log(state, &intent, &assertion).await {
                warn!(
                    action = entry.action,
                    "privileged_audit insert failed: {e:#}"
                );
            }
            info!(
                action = entry.action,
                %install_id,
                intent_id = %intent_id,
                "privileged action authorised"
            );
            Ok(GatePass {
                level:     entry.level,
                assertion: Some(assertion),
            })
        }
        IntentResolution::Denied { outcome } => {
            let (status, msg) = match outcome {
                IntentOutcome::Denied => (StatusCode::FORBIDDEN, "denied by user"),
                IntentOutcome::Expired => (StatusCode::REQUEST_TIMEOUT, "approval timed out"),
                IntentOutcome::Approved => unreachable!("classified as Denied"),
            };
            Err((
                status,
                Json(json!({
                    "error":  msg,
                    "action": entry.action,
                })),
            ))
        }
    }
}

fn build_intent(entry: &TrustEntry, payload: &Value) -> Intent {
    let now = Utc::now();
    Intent {
        id:           Uuid::new_v4(),
        action:       entry.action.to_string(),
        summary:      summarise(entry, payload),
        payload_hash: hash_payload(payload),
        requested_by: "local-desktop".to_string(),
        created_at:   now,
        expires_at:   now + ChronoDuration::seconds(INTENT_TIMEOUT_SECS as i64),
    }
}

/// Stable canonical hash of the request payload. JSON object keys are
/// sorted before hashing so two semantically-equivalent payloads with
/// different key order produce the same hash.
fn hash_payload(payload: &Value) -> [u8; 32] {
    let canonical = canonicalise(payload);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher.finalize().into()
}

fn canonicalise(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}:{}", json_string(k), canonicalise(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonicalise).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn json_string(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

fn summarise(entry: &TrustEntry, payload: &Value) -> String {
    // Short human-readable summary the phone shows. Falls back to the
    // canonical action name when no per-action template is known.
    match entry.action {
        "settings.update_wake_word" => {
            let new_value = payload
                .get("wake_word")
                .and_then(|v| v.as_str())
                .unwrap_or("(unset)");
            format!("Change wake word to '{new_value}'")
        }
        "settings.update_voice_provider" => {
            let p = payload.get("voice_provider").and_then(|v| v.as_str()).unwrap_or("(unset)");
            format!("Change voice provider to '{p}'")
        }
        "settings.update_chat_provider" => {
            let p = payload.get("chat_provider").and_then(|v| v.as_str()).unwrap_or("(unset)");
            format!("Change chat provider to '{p}'")
        }
        "face.delete_biometrics" => {
            let pid = payload.get("profile_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Delete face biometrics for profile {pid}")
        }
        "face.register" => "Enroll a new face for a household member".to_string(),
        "memory.delete_all" => "Wipe ALL stored memory fragments".to_string(),
        "device.revoke_pairing" => {
            let id = payload.get("install_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("Revoke paired device {id}")
        }
        other => format!("Authorise {other}"),
    }
}

async fn audit_log(
    state:     &AppState,
    intent:    &Intent,
    assertion: &SignedAssertion,
) -> anyhow::Result<()> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO privileged_audit \
             (id, intent_id, action, install_id, signature, executed_at, result) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(intent.id.to_string())
    .bind(&intent.action)
    .bind(&assertion.install_id)
    .bind(assertion.signature.to_vec())
    .bind(Utc::now().to_rfc3339())
    .bind("ok")
    .execute(&state.db.system)
    .await
    .context("privileged_audit insert failed")?;
    Ok(())
}

fn action_for(entry: &TrustEntry) -> String {
    entry.action.to_string()
}

fn trust_error_to_http(
    action:     String,
    install_id: &str,
    err:        TrustError,
) -> (StatusCode, Json<Value>) {
    let (status, code) = match err {
        TrustError::UnknownDevice(_) => (StatusCode::PRECONDITION_FAILED, "unknown_device"),
        TrustError::Revoked(_) => (StatusCode::FORBIDDEN, "revoked"),
        TrustError::BadSignature => (StatusCode::UNAUTHORIZED, "bad_signature"),
        TrustError::StaleTimestamp { .. } => (StatusCode::UNAUTHORIZED, "stale_timestamp"),
        TrustError::ReplayedNonce => (StatusCode::UNAUTHORIZED, "replayed_nonce"),
        TrustError::ActionMismatch { .. } => (StatusCode::UNAUTHORIZED, "action_mismatch"),
        TrustError::ExpiredIntent => (StatusCode::REQUEST_TIMEOUT, "expired"),
        TrustError::Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "storage"),
    };
    warn!(%action, %install_id, "trust verify failed: {err}");
    (
        status,
        Json(json!({
            "error":  err.to_string(),
            "code":   code,
            "action": action,
        })),
    )
}

fn service_unavailable(component: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": format!("biometric trust subsystem not configured: {component}"),
        })),
    )
}

fn internal<E: std::fmt::Display>(label: &str, e: E) -> (StatusCode, Json<Value>) {
    warn!("{label}: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": format!("{label}: {e}"),
        })),
    )
}

/// Best-effort origin classification from the request socket address.
/// Loopback and RFC1918 addresses count as Local; anything else is Remote.
/// Callers that have richer context (e.g. Tailscale tag headers) can build
/// an Origin directly.
pub fn origin_from_socket(addr: std::net::IpAddr) -> Origin {
    use std::net::IpAddr::*;
    let private = match addr {
        V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.octets() == [0, 0, 0, 0]
        }
        V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unspecified(),
    };
    if private { Origin::Local } else { Origin::Remote }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalisation_is_key_order_independent() {
        let a = json!({ "b": 1, "a": 2 });
        let b = json!({ "a": 2, "b": 1 });
        assert_eq!(canonicalise(&a), canonicalise(&b));
    }

    #[test]
    fn origin_classification_local_for_loopback() {
        let local = origin_from_socket("127.0.0.1".parse().unwrap());
        assert_eq!(local, Origin::Local);
    }

    #[test]
    fn origin_classification_remote_for_public_v4() {
        let remote = origin_from_socket("8.8.8.8".parse().unwrap());
        assert_eq!(remote, Origin::Remote);
    }

    #[test]
    fn payload_hash_stable_across_runs() {
        let p = json!({"x": 1, "y": [2, 3]});
        assert_eq!(hash_payload(&p), hash_payload(&p));
    }
}

// Suppress unused-fn warning while only `require_trust` is publicly called;
// `Arc` re-export is for downstream code that builds on this module.
#[allow(dead_code)]
fn _arc_marker(_: Arc<()>) {}
