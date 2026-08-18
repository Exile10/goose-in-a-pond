//! What a mesh peer currently offers (#132 Milestone 5), queried live rather
//! than persisted — see `ports::peer_capability_query` for why.

/// A snapshot, not a promise: a peer answering `inference_available: true`
/// says its `MeshInferenceService` will *attempt* to serve a request right
/// now, not that it has capacity or will still be true a second later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerCapabilities {
    pub inference_available: bool,
    pub lightning_available: bool,
}
