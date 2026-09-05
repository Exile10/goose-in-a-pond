//! Turning a place into coordinates, by whatever means the household allows.
//!
//! Two traits rather than one, and the split is a privacy boundary rather than
//! a taxonomy. [`PlaceLookup`] sends a place NAME to a geocoder — "Kisumu" is
//! not information about this household, and the answer is the same for anyone
//! who asks. [`NetworkPlaceLookup`] sends nothing and learns where you are from
//! the connection itself, which means a third party is told this household's IP
//! address and, by implication, roughly where it lives.
//!
//! Folding both into one trait would have made the second arrive wherever the
//! first was already wired, silently. Kept apart, the expensive one has to be
//! passed in on purpose, and the place that passes it is the place that has to
//! justify it.

use anyhow::Result;
use async_trait::async_trait;

/// A place, fixed to a point on the earth.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceFix {
    /// Readable name, e.g. `Kisumu, Kenya`.
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// The zone the source believes this point is in, when it says.
    pub timezone: Option<String>,
}

/// Name → coordinates. Reveals the query, not the asker.
#[async_trait]
pub trait PlaceLookup: Send + Sync {
    async fn by_name(&self, query: &str) -> Result<PlaceFix>;
}

/// This connection → coordinates. Reveals the asker.
///
/// Separate, optional, and never wired by default: see the module note.
#[async_trait]
pub trait NetworkPlaceLookup: Send + Sync {
    async fn by_network(&self) -> Result<PlaceFix>;
}
