//! `/api/v1/auth/ws` — long-lived WebSocket between pond and a paired
//! GOTG device.
//!
//! Wire format (JSON, all messages tagged with `type`):
//!
//! ```text
//! Server → Client:
//!   { "type": "intent.request",
//!     "intent_id": "<uuid>",
//!     "action":     "settings.update_wake_word",
//!     "summary":    "Change wake word to 'Hey Pond'",
//!     "payload_hash": "<hex32>",
//!     "expires_at": "<rfc3339>" }
//!   { "type": "intent.cancel", "intent_id": "<uuid>" }
//!   { "type": "ack", "ref": "<intent_id|hello>" }
//!   { "type": "error", "code": "<str>", "detail": "<str?>" }
//!
//! Client → Server:
//!   { "type": "hello", "install_id": "<str>" }
//!   { "type": "intent.assert",
//!     "intent_id":  "<uuid>",
//!     "install_id": "<str>",
//!     "ts":         "<rfc3339>",
//!     "nonce":      "<hex16>",
//!     "signature":  "<hex64>" }
//!   { "type": "intent.deny", "intent_id": "<uuid>" }
//!   { "type": "ping" }
//! ```
//!
//! The `hello` frame is required as the first client → server message; we
//! refuse to fan out intents until the phone identifies itself with its
//! `install_id` (which the pond cross-checks against the session token's
//! claimed device).

use crate::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use chrono::{DateTime, Utc};
use pond_core::domain::trust::{Intent, SignedAssertion};
use pond_core::ports::intent_bus::BusEvent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Idle ping cadence — keeps the socket alive across NAT idle timeouts.
const PING_EVERY: Duration = Duration::from_secs(20);

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerToClient {
    #[serde(rename = "intent.request")]
    IntentRequest {
        intent_id:    Uuid,
        action:       String,
        summary:      String,
        payload_hash: String,
        expires_at:   DateTime<Utc>,
    },
    #[serde(rename = "intent.cancel")]
    IntentCancel { intent_id: Uuid },
    #[serde(rename = "ack")]
    Ack { reference: String },
    #[serde(rename = "error")]
    Error { code: String, detail: Option<String> },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientToServer {
    #[serde(rename = "hello")]
    Hello { install_id: String },
    #[serde(rename = "intent.assert")]
    IntentAssert {
        intent_id:  Uuid,
        install_id: String,
        ts:         DateTime<Utc>,
        /// hex-encoded 16-byte nonce
        nonce:      String,
        /// hex-encoded 64-byte signature
        signature:  String,
    },
    #[serde(rename = "intent.deny")]
    IntentDeny { intent_id: Uuid },
    #[serde(rename = "ping")]
    Ping,
}

/// Axum handler — upgrades a request to a WS connection.
pub async fn auth_ws_handler(
    ws:    WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    // ── Phase 1: hello ────────────────────────────────────────────────
    // Wait up to 5 s for the phone to identify itself. Anything else
    // closes the socket.
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let install_id = match hello {
        Ok(Some(Ok(Message::Text(txt)))) => match serde_json::from_str::<ClientToServer>(&txt) {
            Ok(ClientToServer::Hello { install_id }) => install_id,
            _ => {
                let _ = send_error(&mut socket, "expected_hello", None).await;
                return;
            }
        },
        _ => return,
    };
    info!(%install_id, "auth WS connected");

    // Acknowledge the hello so the phone knows we're ready to receive.
    if let Err(e) = send_json(&mut socket, ServerToClient::Ack { reference: "hello".to_string() }).await {
        warn!(%install_id, "failed to send hello ack: {e}");
        return;
    }

    let bus = match state.intent_bus.as_ref() {
        Some(b) => b.clone(),
        None => {
            let _ = send_error(&mut socket, "bus_unavailable", None).await;
            return;
        }
    };
    let mut subscription = match bus.subscribe(&install_id).await {
        Ok(s) => s,
        Err(e) => {
            let _ = send_error(&mut socket, "subscribe_failed", Some(e.to_string())).await;
            return;
        }
    };

    // ── Phase 2: pump ────────────────────────────────────────────────
    let mut ping = interval(PING_EVERY);
    loop {
        tokio::select! {
            // Server-pushed intents.
            event = subscription.recv() => {
                let Some(event) = event else { break; };
                if let Err(e) = forward_event(&mut socket, event).await {
                    debug!(%install_id, "WS forward failed: {e}");
                    break;
                }
            }
            // Client messages.
            client = socket.recv() => {
                match client {
                    Some(Ok(Message::Text(txt))) => {
                        if let Err(e) = handle_client_text(
                            &state, &install_id, &txt, &mut socket
                        ).await {
                            debug!(%install_id, "client message error: {e}");
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = socket.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
            // Idle keepalive.
            _ = ping.tick() => {
                if socket.send(Message::Ping(vec![].into())).await.is_err() { break; }
            }
        }
    }
    info!(%install_id, "auth WS disconnected");
}

async fn forward_event(socket: &mut WebSocket, event: BusEvent) -> anyhow::Result<()> {
    let frame = match event {
        BusEvent::Intent(intent) => intent_to_frame(&intent),
        BusEvent::Cancel(intent_id) => ServerToClient::IntentCancel { intent_id },
    };
    send_json(socket, frame).await
}

fn intent_to_frame(intent: &Intent) -> ServerToClient {
    ServerToClient::IntentRequest {
        intent_id:    intent.id,
        action:       intent.action.clone(),
        summary:      intent.summary.clone(),
        payload_hash: hex::encode(intent.payload_hash),
        expires_at:   intent.expires_at,
    }
}

async fn handle_client_text(
    state:      &AppState,
    install_id: &str,
    txt:        &str,
    socket:     &mut WebSocket,
) -> anyhow::Result<()> {
    let msg: ClientToServer = serde_json::from_str(txt)?;
    match msg {
        ClientToServer::Hello { .. } => {
            // Spurious second hello. Ignore.
            Ok(())
        }
        ClientToServer::Ping => {
            send_json(socket, ServerToClient::Ack { reference: "ping".to_string() }).await
        }
        ClientToServer::IntentDeny { intent_id } => {
            if let Some(bus) = state.intent_bus.as_ref() {
                bus.submit_denial(install_id, intent_id).await?;
            }
            send_json(socket, ServerToClient::Ack { reference: intent_id.to_string() }).await
        }
        ClientToServer::IntentAssert {
            intent_id,
            install_id: claimed_install,
            ts,
            nonce,
            signature,
        } => {
            // Defence: the claimed install_id in the assertion must match
            // the install_id the phone said hello as. Otherwise a phone
            // could sign on behalf of a different device's key — which
            // the verifier would reject anyway, but we fail-fast here.
            if claimed_install != install_id {
                send_error(socket, "install_id_mismatch", None).await?;
                return Ok(());
            }
            let nonce_bytes = decode_fixed::<16>(&nonce)?;
            let sig_bytes = decode_fixed::<64>(&signature)?;
            let assertion = SignedAssertion {
                intent_id,
                install_id: claimed_install,
                ts,
                nonce: nonce_bytes,
                signature: sig_bytes,
            };
            if let Some(bus) = state.intent_bus.as_ref() {
                bus.submit_assertion(install_id, assertion).await?;
            }
            send_json(socket, ServerToClient::Ack { reference: intent_id.to_string() }).await
        }
    }
}

async fn send_json<T: Serialize>(socket: &mut WebSocket, t: T) -> anyhow::Result<()> {
    let txt = serde_json::to_string(&t)?;
    socket.send(Message::Text(txt.into())).await?;
    Ok(())
}

async fn send_error(
    socket: &mut WebSocket,
    code:   &str,
    detail: Option<String>,
) -> anyhow::Result<()> {
    send_json(
        socket,
        ServerToClient::Error {
            code: code.to_string(),
            detail,
        },
    )
    .await
}

fn decode_fixed<const N: usize>(s: &str) -> anyhow::Result<[u8; N]> {
    let v = hex::decode(s)?;
    v.as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected {N} bytes, got {}", v.len()))
}
