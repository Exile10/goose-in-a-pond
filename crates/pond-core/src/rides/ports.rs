//! Driven port: a ride-hailing provider, acting for one member with that member's own account.

use anyhow::Result;
use async_trait::async_trait;

use super::domain::{FareQuote, Place, Ride};

#[async_trait]
pub trait RideProvider: Send + Sync {
    /// Stable, lower-case provider name (`uber`), for logs and the member's notification.
    fn name(&self) -> &'static str;

    /// An upfront fare from `pickup` to `dropoff`. Requests nothing.
    async fn quote(&self, profile_id: &str, pickup: &Place, dropoff: &Place) -> Result<FareQuote>;

    /// Request the ride at `quote`. Call only after the member confirmed it.
    async fn request(
        &self,
        profile_id: &str,
        pickup: &Place,
        dropoff: &Place,
        quote: &FareQuote,
    ) -> Result<Ride>;

    /// The ride as the provider holds it now.
    async fn ride(&self, profile_id: &str, request_id: &str) -> Result<Ride>;

    async fn cancel(&self, profile_id: &str, request_id: &str) -> Result<()>;
}
