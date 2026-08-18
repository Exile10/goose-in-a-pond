//! Wire messages exchanged between mesh peers (#132).
//!
//! Fields are derived directly on plain Rust structs via `prost::Message` —
//! no `.proto` file and no `protoc`/build-time codegen, which keeps this
//! crate free of an extra native build tool (the Jetson cross-build already
//! has to work around ggml's cmake probing; this crate deliberately doesn't
//! add a second one).
//!
//! `Handshake` is what a peer presents on `MeshTransport::connect` so the
//! receiving side can check the harness/model hash pin (an #132 acceptance
//! criterion) before trusting the connection.
//!
//! `MeshFrame` is the one message type sent over `MeshTransport::send`/`recv`
//! for anything application-level — `MeshTransport::recv()` is a single flat
//! queue, so only one consumer (`pond-adapters-mesh-inference`'s
//! `MeshInferenceService`) can own it, which means every message family that
//! needs to travel over the mesh has to be a variant of this one wrapper
//! rather than its own top-level message decoded by a second, competing
//! `recv()` loop. Two families so far:
//!
//! - Mesh inference (#132 Milestone 3): `InferenceRequest` (the borrower's
//!   ask) and `InferenceChunk` (the lender's streamed reply, correlated back
//!   by `request_id` since `MeshTransport` itself has no request/response
//!   correlation).
//! - Invoice exchange (#132 Milestone 5): `InvoiceRequest`/`InvoiceResponse`
//!   — `PaymentRail::batch_settle` needs *the peer's* invoice before it can
//!   pay them, and issuing one is a call only the peer itself can make
//!   against its own wallet, so the peer wanting to settle asks for one over
//!   the mesh first.
//! - Capability query (#132 Milestone 5): `CapabilityRequest`/
//!   `CapabilityResponse` — "what do you offer right now?", queried live
//!   (not persisted, unlike `PeerDirectory`'s trust scopes — a peer's
//!   Lightning wallet or backing model can go up/down between two queries)
//!   so the UI can show what a trusted peer actually offers.
//!
//! A single wrapper `oneof`, not independent top-level messages, because
//! `InferenceRequest`/`InferenceChunk`/`InvoiceRequest`/`InvoiceResponse`/
//! `CapabilityRequest`/`CapabilityResponse` all start with a
//! `request_id: u64` at tag 1 — decoding one directly against another's
//! bytes could silently "succeed" on a truncated/malformed frame instead of
//! erroring.

use prost::Message;

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};
use pond_core::mesh::domain::peer_id::PeerId;

#[derive(Clone, PartialEq, Eq, Message)]
pub struct Handshake {
    #[prost(bytes = "vec", tag = "1")]
    pub peer_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub harness_hash: Vec<u8>,
    #[prost(bytes = "vec", tag = "3")]
    pub model_hash: Vec<u8>,
    /// ed25519 signature (64 bytes) over `peer_id || harness_hash || model_hash`.
    #[prost(bytes = "vec", tag = "4")]
    pub signature: Vec<u8>,
}

impl Handshake {
    pub fn new(peer: PeerId, harness: HarnessHash, model: ModelHash, signature: [u8; 64]) -> Self {
        Self {
            peer_id: peer.as_bytes().to_vec(),
            harness_hash: harness.as_bytes().to_vec(),
            model_hash: model.as_bytes().to_vec(),
            signature: signature.to_vec(),
        }
    }

    /// The bytes a signer/verifier should sign/check over — deterministic
    /// concatenation of the three identity fields, excluding the signature
    /// itself.
    pub fn signed_payload(&self) -> Vec<u8> {
        [
            &self.peer_id[..],
            &self.harness_hash[..],
            &self.model_hash[..],
        ]
        .concat()
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, prost::DecodeError> {
        Message::decode(buf)
    }
}

/// A single message in a mesh-inference request, text-only for #132
/// Milestone 3 — no image attachments or tool-call round-tripping.
/// `pond_core::models::domain::message::ChatMessage` has both; mesh
/// inference v1 is a plain chat completion, not a full agentic/multimodal
/// relay across peers.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct ChatMessageWire {
    /// One of "system" | "user" | "assistant" | "tool", lowercased from
    /// `pond_core::models::domain::message::Role`. A plain string, not a
    /// prost enum, so an unrecognized value from a future harness version
    /// degrades to a decode-time mapping choice in the adapter rather than
    /// a wire-level decode failure.
    #[prost(string, tag = "1")]
    pub role: String,
    #[prost(string, tag = "2")]
    pub content: String,
}

/// The borrower's ask: run a completion against the lender's active model.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct InferenceRequest {
    /// Chosen by the borrower, echoed back on every `InferenceChunk` so the
    /// borrower's `pond-adapters-mesh-inference` can demux replies from
    /// multiple in-flight requests sharing one `MeshTransport::recv()`.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(string, tag = "2")]
    pub system_prompt: String,
    #[prost(message, repeated, tag = "3")]
    pub messages: Vec<ChatMessageWire>,
    #[prost(uint32, tag = "4")]
    pub max_tokens: u32,
}

/// Token usage for a completed request — always the payload of the
/// *terminal* `InferenceChunk` on success, mirroring
/// `pond_core::models::ports::provider::StreamToken::Usage`'s existing
/// "emitted once as the last stream item" convention.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct UsageWire {
    #[prost(uint32, tag = "1")]
    pub prompt_tokens: u32,
    #[prost(uint32, tag = "2")]
    pub completion_tokens: u32,
}

/// One piece of the lender's streamed reply. `usage` and `error` are both
/// terminal — the borrower stops waiting on either one, never on `text`.
#[derive(Clone, PartialEq, Eq, ::prost::Oneof)]
pub enum ChunkKind {
    #[prost(string, tag = "3")]
    Text(String),
    #[prost(message, tag = "4")]
    Usage(UsageWire),
    #[prost(string, tag = "5")]
    Error(String),
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct InferenceChunk {
    /// Matches the `InferenceRequest::request_id` this chunk answers.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    /// Monotonic per request, from 0 — lets the borrower detect a dropped
    /// or reordered chunk instead of silently splicing text out of order.
    #[prost(uint32, tag = "2")]
    pub seq: u32,
    #[prost(oneof = "ChunkKind", tags = "3, 4, 5")]
    pub kind: Option<ChunkKind>,
}

/// The borrower's ask: "issue an invoice for this many millisats so I can
/// pay you." Sent ahead of `PaymentRail::batch_settle`, which needs the
/// peer's invoice string and has no other way to get one.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct InvoiceRequest {
    /// Echoed back on the matching `InvoiceResponse`, same demux purpose as
    /// `InferenceRequest::request_id`.
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(uint64, tag = "2")]
    pub amount_millisats: u64,
}

/// `invoice` is the peer's real BOLT11-shaped string from its own
/// `PaymentRail::issue_invoice`; `error` covers the peer having no
/// `PaymentRail` configured at all (Lightning is off by default) or its own
/// `issue_invoice` call failing.
#[derive(Clone, PartialEq, Eq, ::prost::Oneof)]
pub enum InvoiceResponseKind {
    #[prost(string, tag = "2")]
    Invoice(String),
    #[prost(string, tag = "3")]
    Error(String),
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct InvoiceResponse {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(oneof = "InvoiceResponseKind", tags = "2, 3")]
    pub kind: Option<InvoiceResponseKind>,
}

/// "What do you offer right now?" — no payload beyond the correlation id;
/// the answer is the interesting part.
#[derive(Clone, PartialEq, Eq, Message)]
pub struct CapabilityRequest {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct CapabilityResponse {
    #[prost(uint64, tag = "1")]
    pub request_id: u64,
    #[prost(bool, tag = "2")]
    pub inference_available: bool,
    #[prost(bool, tag = "3")]
    pub lightning_available: bool,
}

/// The one type ever passed to `MeshTransport::send`/`recv` — see the module
/// doc comment for why every message family shares this wrapper instead of
/// being sent as a bare top-level message.
#[derive(Clone, PartialEq, Eq, ::prost::Oneof)]
pub enum MeshFrameKind {
    #[prost(message, tag = "1")]
    Request(InferenceRequest),
    #[prost(message, tag = "2")]
    Chunk(InferenceChunk),
    #[prost(message, tag = "3")]
    InvoiceRequest(InvoiceRequest),
    #[prost(message, tag = "4")]
    InvoiceResponse(InvoiceResponse),
    #[prost(message, tag = "5")]
    CapabilityRequest(CapabilityRequest),
    #[prost(message, tag = "6")]
    CapabilityResponse(CapabilityResponse),
}

#[derive(Clone, PartialEq, Eq, Message)]
pub struct MeshFrame {
    #[prost(oneof = "MeshFrameKind", tags = "1, 2, 3, 4, 5, 6")]
    pub kind: Option<MeshFrameKind>,
}

impl MeshFrame {
    pub fn request(req: InferenceRequest) -> Self {
        Self {
            kind: Some(MeshFrameKind::Request(req)),
        }
    }

    pub fn chunk(chunk: InferenceChunk) -> Self {
        Self {
            kind: Some(MeshFrameKind::Chunk(chunk)),
        }
    }

    pub fn invoice_request(req: InvoiceRequest) -> Self {
        Self {
            kind: Some(MeshFrameKind::InvoiceRequest(req)),
        }
    }

    pub fn invoice_response(resp: InvoiceResponse) -> Self {
        Self {
            kind: Some(MeshFrameKind::InvoiceResponse(resp)),
        }
    }

    pub fn capability_request(req: CapabilityRequest) -> Self {
        Self {
            kind: Some(MeshFrameKind::CapabilityRequest(req)),
        }
    }

    pub fn capability_response(resp: CapabilityResponse) -> Self {
        Self {
            kind: Some(MeshFrameKind::CapabilityResponse(resp)),
        }
    }

    pub fn encode_to_vec(&self) -> Vec<u8> {
        Message::encode_to_vec(self)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, prost::DecodeError> {
        Message::decode(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::MeshKeypair;

    fn sample_handshake() -> (Handshake, MeshKeypair) {
        let keypair = MeshKeypair::generate();
        let harness = HarnessHash::from([1u8; 32]);
        let model = ModelHash::from([2u8; 32]);
        let unsigned = Handshake::new(keypair.peer_id(), harness, model, [0u8; 64]);
        let signature = keypair.sign(&unsigned.signed_payload());
        (
            Handshake::new(keypair.peer_id(), harness, model, signature),
            keypair,
        )
    }

    #[test]
    fn encode_decode_roundtrips() {
        let (handshake, _keypair) = sample_handshake();
        let bytes = handshake.encode_to_vec();
        let decoded = Handshake::decode(&bytes[..]).unwrap();
        assert_eq!(handshake, decoded);
    }

    #[test]
    fn signature_verifies_against_signed_payload() {
        let (handshake, keypair) = sample_handshake();
        let signature: [u8; 64] = handshake.signature.clone().try_into().unwrap();
        assert!(crate::identity::verify(
            keypair.peer_id(),
            &handshake.signed_payload(),
            &signature
        )
        .unwrap());
    }

    #[test]
    fn decode_of_garbage_bytes_errors_not_panics() {
        // A handful of random bytes are very unlikely to be a valid encoding,
        // but even if a length happens to parse, decode() must never panic —
        // this is exactly the "refuse a malformed peer" path.
        let result = Handshake::decode(&[0xff, 0x00, 0x01][..]);
        let _ = result; // either Ok or Err is acceptable; a panic is not.
    }

    fn sample_request() -> InferenceRequest {
        InferenceRequest {
            request_id: 42,
            system_prompt: "You are a helpful assistant.".to_string(),
            messages: vec![ChatMessageWire {
                role: "user".to_string(),
                content: "hello mesh".to_string(),
            }],
            max_tokens: 256,
        }
    }

    #[test]
    fn inference_frame_request_roundtrips() {
        let frame = MeshFrame::request(sample_request());
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        assert!(matches!(decoded.kind, Some(MeshFrameKind::Request(_))));
    }

    #[test]
    fn inference_frame_text_chunk_roundtrips() {
        let frame = MeshFrame::chunk(InferenceChunk {
            request_id: 42,
            seq: 0,
            kind: Some(ChunkKind::Text("hel".to_string())),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn inference_frame_terminal_usage_chunk_roundtrips() {
        let frame = MeshFrame::chunk(InferenceChunk {
            request_id: 42,
            seq: 3,
            kind: Some(ChunkKind::Usage(UsageWire {
                prompt_tokens: 12,
                completion_tokens: 8,
            })),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        match decoded.kind {
            Some(MeshFrameKind::Chunk(InferenceChunk {
                kind: Some(ChunkKind::Usage(usage)),
                ..
            })) => {
                assert_eq!(usage.prompt_tokens, 12);
                assert_eq!(usage.completion_tokens, 8);
            }
            other => panic!("expected a terminal usage chunk, got {other:?}"),
        }
    }

    #[test]
    fn inference_frame_error_chunk_roundtrips() {
        let frame = MeshFrame::chunk(InferenceChunk {
            request_id: 7,
            seq: 0,
            kind: Some(ChunkKind::Error("insufficient credit".to_string())),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn request_and_chunk_are_never_confused_despite_sharing_a_request_id_tag() {
        // Both InferenceRequest and InferenceChunk start with `request_id: u64`
        // at tag 1 — the reason MeshFrame wraps them in a oneof instead of
        // sending either as a bare top-level message (see the module doc
        // comment). Decoding a request's bytes directly as an InferenceChunk
        // must not silently produce a chunk with a bogus `kind`.
        let request = sample_request();
        let bytes = Message::encode_to_vec(&request);
        let decoded = InferenceChunk::decode(&bytes[..]);
        match decoded {
            Err(_) => {} // ideal outcome
            Ok(chunk) => assert_eq!(chunk.kind, None, "must not fabricate a chunk kind"),
        }
    }

    #[test]
    fn inference_frame_decode_of_garbage_bytes_errors_not_panics() {
        let result = MeshFrame::decode(&[0xff, 0x00, 0x01][..]);
        let _ = result;
    }

    #[test]
    fn invoice_request_frame_roundtrips() {
        let frame = MeshFrame::invoice_request(InvoiceRequest {
            request_id: 1,
            amount_millisats: 5000,
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        assert!(matches!(
            decoded.kind,
            Some(MeshFrameKind::InvoiceRequest(_))
        ));
    }

    #[test]
    fn invoice_response_with_invoice_roundtrips() {
        let frame = MeshFrame::invoice_response(InvoiceResponse {
            request_id: 1,
            kind: Some(InvoiceResponseKind::Invoice("lnbc1...".to_string())),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn invoice_response_with_error_roundtrips() {
        let frame = MeshFrame::invoice_response(InvoiceResponse {
            request_id: 2,
            kind: Some(InvoiceResponseKind::Error(
                "no payment rail configured".to_string(),
            )),
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn invoice_request_and_inference_request_are_never_confused() {
        // Both start with `request_id: u64` at tag 1, same hazard the
        // InferenceRequest/InferenceChunk test above documents.
        let invoice_req = InvoiceRequest {
            request_id: 9,
            amount_millisats: 1000,
        };
        let bytes = Message::encode_to_vec(&invoice_req);
        let decoded = InferenceRequest::decode(&bytes[..]);
        match decoded {
            Err(_) => {}
            Ok(req) => assert!(req.messages.is_empty(), "must not fabricate messages"),
        }
    }

    #[test]
    fn capability_request_frame_roundtrips() {
        let frame = MeshFrame::capability_request(CapabilityRequest { request_id: 5 });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        assert!(matches!(
            decoded.kind,
            Some(MeshFrameKind::CapabilityRequest(_))
        ));
    }

    #[test]
    fn capability_response_frame_roundtrips() {
        let frame = MeshFrame::capability_response(CapabilityResponse {
            request_id: 5,
            inference_available: true,
            lightning_available: false,
        });
        let bytes = frame.encode_to_vec();
        let decoded = MeshFrame::decode(&bytes[..]).unwrap();
        assert_eq!(frame, decoded);
        match decoded.kind {
            Some(MeshFrameKind::CapabilityResponse(resp)) => {
                assert!(resp.inference_available);
                assert!(!resp.lightning_available);
            }
            other => panic!("expected a capability response, got {other:?}"),
        }
    }
}
