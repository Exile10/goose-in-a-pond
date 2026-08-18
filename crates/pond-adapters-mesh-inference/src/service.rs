use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{mpsc, Mutex};

use async_trait::async_trait;

use pond_core::mesh::domain::capabilities::PeerCapabilities;
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::domain::token_count::TokenCount;
use pond_core::mesh::ports::credit_ledger::CreditLedger;
use pond_core::mesh::ports::mesh_transport::{MeshTransport, MeshTransportError};
use pond_core::mesh::ports::payment_rail::PaymentRail;
use pond_core::mesh::ports::peer_capability_query::{
    PeerCapabilityQuery, PeerCapabilityQueryError,
};
use pond_core::mesh::ports::peer_directory::PeerDirectory;
use pond_core::mesh::ports::usage_tally::UsageTally;
use pond_core::models::ports::provider::{LlmProvider, StreamToken};
use pond_mesh_protocol::wire::{
    CapabilityRequest, CapabilityResponse, ChunkKind, InferenceChunk, InferenceRequest,
    InvoiceRequest, InvoiceResponse, InvoiceResponseKind, MeshFrame, MeshFrameKind,
};

use crate::from_wire_message;

/// Errors from [`MeshInferenceService::request_invoice`] — the client-role
/// counterpart to [`crate::MeshInferenceError`], which covers borrowing
/// compute rather than requesting an invoice.
#[derive(thiserror::Error, Debug)]
pub enum InvoiceRequestError {
    #[error("mesh transport error: {0}")]
    Transport(#[from] MeshTransportError),
    #[error("peer {0} reported an error: {1}")]
    PeerError(PeerId, String),
    #[error("no invoice response from peer {0} before timeout")]
    Timeout(PeerId),
}

/// Owns the *only* consumer of `MeshTransport::recv()` for a mesh-enabled
/// Pond. Dispatches every inbound frame by decoded kind:
///
/// - `InferenceRequest` → serve it with `backing_provider` (server role) and
///   stream the reply back.
/// - `InferenceChunk` → route to whichever in-flight outbound request
///   (`MeshInferenceProvider::stream_complete`) it answers (client-role demux).
/// - `InvoiceRequest` → issue an invoice via `payment_rail` (server role) and
///   send it back, or an error chunk if no `payment_rail` is configured.
/// - `InvoiceResponse` → route to whichever in-flight `request_invoice` call
///   it answers, same demux shape as `InferenceChunk`.
/// - `CapabilityRequest` → answer with what this Pond currently offers
///   (inference: always, since `backing_provider` always exists; Lightning:
///   `payment_rail.is_some()`).
/// - `CapabilityResponse` → route to whichever in-flight `capabilities_of`
///   call it answers, same demux shape as `InvoiceResponse`.
///
/// If anything else on this Pond ever needs to consume `recv()`, it has to
/// go through here too — a second independent `recv()` loop would silently
/// steal frames from this one.
pub struct MeshInferenceService {
    pub(crate) transport: Arc<dyn MeshTransport>,
    pub(crate) peer_directory: Arc<dyn PeerDirectory>,
    pub(crate) credit_ledger: Arc<dyn CreditLedger>,
    pub(crate) usage_tally: Arc<dyn UsageTally>,
    /// How long `MeshInferenceProvider::stream_complete` waits for each next
    /// chunk (reset on every chunk received, not an overall stream deadline)
    /// before giving up on a peer that's gone silent. A constructor
    /// parameter rather than a hardcoded constant so tests can use a short
    /// one instead of waiting out a real multi-second production timeout.
    pub(crate) chunk_timeout: std::time::Duration,
    backing_provider: Arc<dyn LlmProvider>,
    /// This Pond's own Lightning wallet, used to answer inbound
    /// `InvoiceRequest`s from peers who want to pay us. `None` when
    /// Lightning isn't configured (off by default) — inbound invoice
    /// requests then get an `InvoiceResponseKind::Error` reply rather than
    /// being silently dropped.
    payment_rail: Option<Arc<dyn PaymentRail>>,
    pending: Mutex<HashMap<u64, mpsc::UnboundedSender<InferenceChunk>>>,
    pending_invoices: Mutex<HashMap<u64, mpsc::UnboundedSender<InvoiceResponse>>>,
    pending_capabilities: Mutex<HashMap<u64, mpsc::UnboundedSender<CapabilityResponse>>>,
    next_request_id: AtomicU64,
}

impl MeshInferenceService {
    /// Constructs the service and spawns its `recv()` loop. `backing_provider`
    /// is whatever `LlmProvider` this Pond already has active locally — the
    /// service delegates inbound requests to it, it does not discover or
    /// build one itself.
    pub fn spawn(
        transport: Arc<dyn MeshTransport>,
        peer_directory: Arc<dyn PeerDirectory>,
        credit_ledger: Arc<dyn CreditLedger>,
        usage_tally: Arc<dyn UsageTally>,
        backing_provider: Arc<dyn LlmProvider>,
        chunk_timeout: std::time::Duration,
        payment_rail: Option<Arc<dyn PaymentRail>>,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            transport,
            peer_directory,
            credit_ledger,
            usage_tally,
            chunk_timeout,
            backing_provider,
            payment_rail,
            pending: Mutex::new(HashMap::new()),
            pending_invoices: Mutex::new(HashMap::new()),
            pending_capabilities: Mutex::new(HashMap::new()),
            next_request_id: AtomicU64::new(1),
        });
        tokio::spawn(Self::run(service.clone()));
        service
    }

    /// The `LlmProvider` to use when this Pond wants to *borrow* — a thin
    /// handle back into this same service (and therefore the same `recv()`
    /// loop already spawned by [`Self::spawn`]).
    pub fn provider(self: &Arc<Self>) -> crate::MeshInferenceProvider {
        crate::MeshInferenceProvider::new(self.clone())
    }

    /// A fresh id for a new outbound request, echoed back on every
    /// `InferenceChunk` answering it.
    pub(crate) fn next_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a channel that receives every `InferenceChunk` decoded for
    /// `request_id`, until `unregister` is called. Must be called *before*
    /// the request frame is sent, so a reply arriving unusually fast can't
    /// race the registration.
    pub(crate) async fn register_pending(
        &self,
        request_id: u64,
    ) -> mpsc::UnboundedReceiver<InferenceChunk> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.lock().await.insert(request_id, tx);
        rx
    }

    pub(crate) async fn unregister_pending(&self, request_id: u64) {
        self.pending.lock().await.remove(&request_id);
    }

    async fn run(self: Arc<Self>) {
        loop {
            match self.transport.recv().await {
                Ok((peer, bytes)) => {
                    let this = self.clone();
                    // Off the recv loop immediately: a slow inbound request
                    // (running the local model) must not block receiving the
                    // next frame, e.g. a chunk answering a different
                    // in-flight outbound request.
                    tokio::spawn(async move { this.dispatch(peer, bytes).await });
                }
                Err(err) => {
                    tracing::warn!("mesh-inference: recv loop stopped: {err}");
                    return;
                }
            }
        }
    }

    async fn dispatch(&self, peer: PeerId, bytes: Vec<u8>) {
        let frame = match MeshFrame::decode(&bytes[..]) {
            Ok(frame) => frame,
            Err(err) => {
                tracing::warn!("mesh-inference: malformed frame from {peer}: {err}");
                return;
            }
        };
        match frame.kind {
            Some(MeshFrameKind::Request(request)) => self.serve_request(peer, request).await,
            Some(MeshFrameKind::Chunk(chunk)) => {
                let pending = self.pending.lock().await;
                if let Some(tx) = pending.get(&chunk.request_id) {
                    // A dropped receiver means the requester already gave up
                    // (timed out / stream was dropped) — nothing to do.
                    let _ = tx.send(chunk);
                }
                // An unknown request_id means the requester already
                // unregistered (finished or gave up) — drop silently, not an
                // error: this is an expected race, not a protocol violation.
            }
            Some(MeshFrameKind::InvoiceRequest(request)) => {
                self.serve_invoice_request(peer, request).await
            }
            Some(MeshFrameKind::InvoiceResponse(response)) => {
                let pending = self.pending_invoices.lock().await;
                if let Some(tx) = pending.get(&response.request_id) {
                    let _ = tx.send(response);
                }
                // Same "requester already gave up" race as the InferenceChunk
                // arm above — an unknown request_id is expected, not an error.
            }
            Some(MeshFrameKind::CapabilityRequest(request)) => {
                self.serve_capability_request(peer, request).await
            }
            Some(MeshFrameKind::CapabilityResponse(response)) => {
                let pending = self.pending_capabilities.lock().await;
                if let Some(tx) = pending.get(&response.request_id) {
                    let _ = tx.send(response);
                }
                // Same expected race as the other *Response arms above.
            }
            None => {
                tracing::warn!("mesh-inference: frame from {peer} carried no payload");
            }
        }
    }

    /// Server role: answer with what this Pond currently offers. Inference
    /// is unconditionally `true` — `backing_provider` always exists, this
    /// service wouldn't be constructed otherwise — Lightning reflects
    /// whether `payment_rail` is configured right now.
    async fn serve_capability_request(&self, peer: PeerId, request: CapabilityRequest) {
        let frame = MeshFrame::capability_response(CapabilityResponse {
            request_id: request.request_id,
            inference_available: true,
            lightning_available: self.payment_rail.is_some(),
        })
        .encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            tracing::warn!("mesh-inference: failed to send capability response to {peer}: {err}");
        }
    }

    /// Server role: a peer wants to pay us and needs an invoice first. Uses
    /// this Pond's own `payment_rail` — never the requesting peer's.
    async fn serve_invoice_request(&self, peer: PeerId, request: InvoiceRequest) {
        let response_kind = match &self.payment_rail {
            Some(rail) => match rail
                .issue_invoice(Millisats::new(request.amount_millisats))
                .await
            {
                Ok(invoice) => InvoiceResponseKind::Invoice(invoice),
                Err(err) => {
                    tracing::warn!("mesh-inference: failed to issue invoice for {peer}: {err}");
                    InvoiceResponseKind::Error(err.to_string())
                }
            },
            None => InvoiceResponseKind::Error("no payment rail configured".to_string()),
        };
        let frame = MeshFrame::invoice_response(InvoiceResponse {
            request_id: request.request_id,
            kind: Some(response_kind),
        })
        .encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            tracing::warn!("mesh-inference: failed to send invoice response to {peer}: {err}");
        }
    }

    /// Client role: ask `peer` for an invoice covering `amount`, so
    /// `PaymentRail::batch_settle` has something real to pay. Callers (the
    /// settlement job) are expected to call this once per settlement attempt,
    /// not cache the result — a Lightning invoice from `issue_invoice` isn't
    /// necessarily reusable.
    pub async fn request_invoice(
        &self,
        peer: PeerId,
        amount: Millisats,
    ) -> Result<String, InvoiceRequestError> {
        let request_id = self.next_request_id();
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_invoices.lock().await.insert(request_id, tx);

        let frame = MeshFrame::invoice_request(InvoiceRequest {
            request_id,
            amount_millisats: amount.value(),
        })
        .encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            self.pending_invoices.lock().await.remove(&request_id);
            return Err(InvoiceRequestError::Transport(err));
        }

        let result = match tokio::time::timeout(self.chunk_timeout, rx.recv()).await {
            Ok(Some(response)) => match response.kind {
                Some(InvoiceResponseKind::Invoice(invoice)) => Ok(invoice),
                Some(InvoiceResponseKind::Error(message)) => {
                    Err(InvoiceRequestError::PeerError(peer, message))
                }
                None => Err(InvoiceRequestError::PeerError(
                    peer,
                    "empty invoice response".to_string(),
                )),
            },
            Ok(None) => Err(InvoiceRequestError::Timeout(peer)),
            Err(_elapsed) => Err(InvoiceRequestError::Timeout(peer)),
        };
        self.pending_invoices.lock().await.remove(&request_id);
        result
    }

    /// Server role: run `backing_provider` against the borrower's request and
    /// stream the reply back as a sequence of `InferenceChunk`s, terminated
    /// by exactly one `usage` or `error` chunk.
    async fn serve_request(&self, peer: PeerId, request: InferenceRequest) {
        let messages = request.messages.iter().map(from_wire_message).collect();
        let mut stream = self
            .backing_provider
            .stream_complete(&request.system_prompt, messages);

        let mut seq = 0u32;
        let mut usage = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamToken::Text(text)) => {
                    self.send_chunk(
                        peer,
                        InferenceChunk {
                            request_id: request.request_id,
                            seq,
                            kind: Some(ChunkKind::Text(text)),
                        },
                    )
                    .await;
                    seq += 1;
                }
                Ok(StreamToken::Usage(stats)) => {
                    usage = Some(pond_mesh_protocol::wire::UsageWire {
                        prompt_tokens: stats.prompt_tokens,
                        completion_tokens: stats.completion_tokens,
                    });
                }
                Err(err) => {
                    self.send_chunk(
                        peer,
                        InferenceChunk {
                            request_id: request.request_id,
                            seq,
                            kind: Some(ChunkKind::Error(err.to_string())),
                        },
                    )
                    .await;
                    return; // error chunk is terminal — don't also send usage
                }
            }
        }

        // Not every backing provider reports usage (the trait's default
        // `stream_complete` never does) — fall back to a zero tally rather
        // than never sending a terminal chunk. This only affects UsageTally
        // accuracy for that provider, not the correctness of the reply text
        // the borrower already received.
        let usage = usage.unwrap_or_default();
        let _ = self
            .usage_tally
            .record_usage(peer, TokenCount::new(usage.completion_tokens as u64))
            .await;
        self.send_chunk(
            peer,
            InferenceChunk {
                request_id: request.request_id,
                seq,
                kind: Some(ChunkKind::Usage(usage)),
            },
        )
        .await;
    }

    async fn send_chunk(&self, peer: PeerId, chunk: InferenceChunk) {
        let frame = MeshFrame::chunk(chunk).encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            tracing::warn!("mesh-inference: failed to send reply to {peer}: {err}");
        }
    }
}

/// Client role for capability queries — implemented directly on the service
/// (rather than a thin handle type like [`crate::MeshInferenceProvider`])
/// because there's only one method and no per-caller state to hold, unlike
/// borrowing compute where `model_name()` needs `last_peer`.
#[async_trait]
impl PeerCapabilityQuery for MeshInferenceService {
    async fn capabilities_of(
        &self,
        peer: PeerId,
    ) -> Result<PeerCapabilities, PeerCapabilityQueryError> {
        let request_id = self.next_request_id();
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.pending_capabilities
            .lock()
            .await
            .insert(request_id, tx);

        let frame = MeshFrame::capability_request(CapabilityRequest { request_id }).encode_to_vec();
        if let Err(err) = self.transport.send(peer, frame).await {
            self.pending_capabilities.lock().await.remove(&request_id);
            return Err(PeerCapabilityQueryError::Transport(err.to_string()));
        }

        let result = match tokio::time::timeout(self.chunk_timeout, rx.recv()).await {
            Ok(Some(response)) => Ok(PeerCapabilities {
                inference_available: response.inference_available,
                lightning_available: response.lightning_available,
            }),
            Ok(None) | Err(_) => Err(PeerCapabilityQueryError::Timeout(peer)),
        };
        self.pending_capabilities.lock().await.remove(&request_id);
        result
    }
}

/// Bridges the inherent `request_invoice` (above) to `pond-core`'s
/// `InvoiceRequester` port, so a caller outside this crate (the settlement
/// job in `pond-server`) can hold this service behind `Arc<dyn
/// InvoiceRequester>` without depending on this concrete type. The
/// fully-qualified call below is deliberate, not decorative: this type has
/// both an inherent `request_invoice` and this trait's `request_invoice` in
/// scope, and `self.request_invoice(...)` would still resolve to the
/// inherent one (dot-call syntax always prefers inherent methods) — but
/// spelling it out means a future reader never has to know that rule to be
/// sure this isn't infinite recursion.
#[async_trait]
impl pond_core::mesh::ports::invoice_requester::InvoiceRequester for MeshInferenceService {
    async fn request_invoice(
        &self,
        peer: PeerId,
        amount: Millisats,
    ) -> Result<String, pond_core::mesh::ports::invoice_requester::InvoiceRequesterError> {
        MeshInferenceService::request_invoice(self, peer, amount)
            .await
            .map_err(Into::into)
    }
}

impl From<InvoiceRequestError>
    for pond_core::mesh::ports::invoice_requester::InvoiceRequesterError
{
    fn from(err: InvoiceRequestError) -> Self {
        match err {
            InvoiceRequestError::Transport(err) => Self::Transport(err.to_string()),
            InvoiceRequestError::PeerError(peer, message) => Self::PeerError(peer, message),
            InvoiceRequestError::Timeout(peer) => Self::Timeout(peer),
        }
    }
}
