//! Wire protocol for the private Pond Compute mesh (#132): content hashing,
//! peer identity/signatures, and wire message types. Near-pure — no `tokio`,
//! `sqlx`, `reqwest`, `goose`, or `rmcp` — the actual transport lives in
//! `pond-adapters-mesh-libp2p`, a later milestone.

pub mod hashing;
pub mod identity;
pub mod wire;
