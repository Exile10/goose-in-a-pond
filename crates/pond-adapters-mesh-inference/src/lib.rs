//! Mesh inference (#132 Milestone 3): the piece that makes the private mesh
//! (`pond-adapters-mesh-libp2p`) actually serve requests instead of just
//! connecting peers.
//!
//! Every Pond in a Circle is symmetric — it can borrow a trusted peer's
//! compute *and* lend its own — so this crate has two roles, not one:
//!
//! - **Client role** (`MeshInferenceProvider`): the `LlmProvider` used when
//!   this Pond wants to borrow. Picks a trusted, connected, funded peer,
//!   sends an `InferenceRequest`, and streams the reply back as
//!   `StreamToken`s.
//! - **Server role**: a background task that listens for inbound
//!   `InferenceRequest`s from trusted peers and serves them using whatever
//!   `LlmProvider` this Pond already has active locally.
//!
//! Both roles share one `MeshInferenceService`, because `MeshTransport::recv()`
//! is a single flat "next frame from any peer" queue — only one consumer can
//! own it. `v1` deliberately routes to a single peer, no sharding across
//! multiple lenders (see the #132 issue's "throughput-aware chain routing" —
//! that's future work once the basic mechanism is proven).
//!
//! v1 is also text-only: no image attachments, no tool-call passthrough.
//! Mesh inference is a plain chat completion, not a full agentic/multimodal
//! relay across peers (see `pond-mesh-protocol::wire::ChatMessageWire`).

mod provider;
mod service;

pub use provider::{MeshInferenceError, MeshInferenceProvider};
pub use service::{InvoiceRequestError, MeshInferenceService};

use pond_core::models::domain::message::{ChatMessage, Role};
use pond_mesh_protocol::wire::ChatMessageWire;

fn role_to_wire(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn to_wire_message(message: &ChatMessage) -> ChatMessageWire {
    ChatMessageWire {
        role: role_to_wire(&message.role).to_string(),
        content: message.content.clone(),
    }
}

/// Unrecognized role strings degrade to `Role::User` rather than failing the
/// whole request — a future harness version adding a role this one doesn't
/// know about shouldn't refuse mesh inference outright.
fn from_wire_message(message: &ChatMessageWire) -> ChatMessage {
    match message.role.as_str() {
        "system" => ChatMessage::system(message.content.clone()),
        "assistant" => ChatMessage::assistant(message.content.clone()),
        "tool" => ChatMessage::user(message.content.clone()), // no tool_call_id on the wire (v1 drops tool_calls) — can't reconstruct a real tool_result
        _ => ChatMessage::user(message.content.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip_preserves_role_and_content() {
        for original in [
            ChatMessage::system("be helpful"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ] {
            let wire = to_wire_message(&original);
            let restored = from_wire_message(&wire);
            assert_eq!(restored.role, original.role);
            assert_eq!(restored.content, original.content);
        }
    }

    #[test]
    fn unrecognized_wire_role_degrades_to_user() {
        let wire = ChatMessageWire {
            role: "from_the_future".to_string(),
            content: "hi".to_string(),
        };
        assert_eq!(from_wire_message(&wire).role, Role::User);
    }
}
