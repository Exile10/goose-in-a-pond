//! Thin WebSocket client for python-matter-server.
//!
//! One connection carries both request/response pairs (matched by
//! `message_id`) and unsolicited events (`attribute_updated`, node lifecycle).
//! A background read task routes responses to their waiting callers and fans
//! events out on an mpsc channel the bridge consumes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{command_frame, parse_server_message, ServerMessage};

/// How long a command may wait for its response. Commissioned-device commands
/// round-trip in tens of milliseconds; this bounds a wedged server.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// An unsolicited event from the server.
#[derive(Debug, Clone)]
pub struct MatterEvent {
    pub event: String,
    pub data: Value,
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>>;

pub struct MatterClient {
    tx: mpsc::Sender<Message>,
    pending: Pending,
    next_id: AtomicU64,
}

impl MatterClient {
    /// Connect and start the read/write tasks. Returns the client plus the
    /// stream of unsolicited events. The first server-info greeting is
    /// consumed here as a handshake check.
    pub async fn connect(url: &str) -> Result<(Arc<Self>, mpsc::Receiver<MatterEvent>)> {
        let (socket, _) = connect_async(url)
            .await
            .with_context(|| format!("connecting to matter-server at {url}"))?;
        let (mut sink, mut stream) = socket.split();

        // Handshake: the server greets with its info frame immediately.
        let greeting = tokio::time::timeout(COMMAND_TIMEOUT, stream.next())
            .await
            .context("timed out waiting for matter-server greeting")?
            .ok_or_else(|| anyhow!("matter-server closed during handshake"))?
            .context("matter-server handshake failed")?;
        tracing::info!(
            url,
            "connected to matter-server ({} byte greeting)",
            greeting.len()
        );

        let (out_tx, mut out_rx) = mpsc::channel::<Message>(32);
        let (event_tx, event_rx) = mpsc::channel::<MatterEvent>(64);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        // Writer task: serialises all outbound frames.
        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
        });

        // Reader task: routes responses to waiters, events to the bridge.
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                let Ok(Message::Text(text)) = frame else {
                    if frame.is_err() {
                        break;
                    }
                    continue; // pings etc.
                };
                match parse_server_message(&text) {
                    ServerMessage::Result { message_id, result } => {
                        if let Some(waiter) = pending_reader
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&message_id)
                        {
                            let _ = waiter.send(Ok(result));
                        }
                    }
                    ServerMessage::Error {
                        message_id,
                        details,
                    } => {
                        if let Some(waiter) = pending_reader
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&message_id)
                        {
                            let _ = waiter.send(Err(anyhow!("matter-server: {details}")));
                        }
                    }
                    ServerMessage::Event { event, data } => {
                        // Full channel = slow bridge; drop oldest-style backpressure
                        // is fine for state updates (last write wins downstream).
                        let _ = event_tx.try_send(MatterEvent { event, data });
                    }
                    ServerMessage::Other => {}
                }
            }
            // Connection gone: fail all in-flight commands so callers see it.
            let mut map = pending_reader.lock().unwrap_or_else(|e| e.into_inner());
            for (_, waiter) in map.drain() {
                let _ = waiter.send(Err(anyhow!("matter-server connection lost")));
            }
            tracing::warn!("matter-server connection closed");
        });

        Ok((
            Arc::new(Self {
                tx: out_tx,
                pending,
                next_id: AtomicU64::new(1),
            }),
            event_rx,
        ))
    }

    /// Send `command` and await its response.
    pub async fn send_command(&self, command: &str, args: Value) -> Result<Value> {
        let id = format!("giap-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx);

        let frame = Message::Text(command_frame(&id, command, args).into());
        if self.tx.send(frame).await.is_err() {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(anyhow!("matter-server connection lost"));
        }

        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("matter-server dropped the response channel")),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
                Err(anyhow!("matter-server command '{command}' timed out"))
            }
        }
    }
}
