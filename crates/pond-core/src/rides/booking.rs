//! Quote, confirm, request, track and cancel one member's rides. The member who asked confirms
//! on their own phone; nothing is requested before that, and a quote is confirmed at most once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use super::domain::{BookingState, PendingRide, Place, Ride};
use super::ports::RideProvider;

#[derive(Debug, thiserror::Error)]
pub enum BookingError {
    #[error("no such ride")]
    NotFound,
    /// Confirming, declining, cancelling or reading another member's ride.
    #[error("this ride belongs to another member")]
    NotYours,
    #[error("the fare expired; ask for a new one")]
    Expired,
    #[error("this ride was already {0}")]
    AlreadyDecided(&'static str),
    #[error("the ride was never requested")]
    NotRequested,
    #[error("{0}")]
    Provider(#[source] anyhow::Error),
}

pub struct RideBooking {
    provider: Arc<dyn RideProvider>,
    rides: Mutex<HashMap<String, PendingRide>>,
}

impl RideBooking {
    pub fn new(provider: Arc<dyn RideProvider>) -> Self {
        Self {
            provider,
            rides: Mutex::new(HashMap::new()),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    /// Ask the provider for a fare and hold it for `profile_id` to confirm. Requests nothing.
    pub async fn quote(
        &self,
        profile_id: &str,
        pickup: Place,
        dropoff: Place,
        now: DateTime<Utc>,
    ) -> Result<PendingRide, BookingError> {
        let quote = self
            .provider
            .quote(profile_id, &pickup, &dropoff)
            .await
            .map_err(BookingError::Provider)?;
        let pending = PendingRide {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            pickup,
            dropoff,
            quote,
            created_at: now,
            state: BookingState::AwaitingConfirmation,
        };
        self.lock().insert(pending.id.clone(), pending.clone());
        Ok(pending)
    }

    /// The member's yes. Requests the ride once; a second confirm of the same quote is refused.
    pub async fn confirm(
        &self,
        id: &str,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Ride, BookingError> {
        // Claimed under the lock, so two taps on Confirm cannot both reach the provider.
        let pending = {
            let mut rides = self.lock();
            let pending = owned(&mut rides, id, profile_id)?;
            match &pending.state {
                BookingState::AwaitingConfirmation => {}
                other => return Err(BookingError::AlreadyDecided(state_word(other))),
            }
            if now >= pending.quote.expires_at {
                return Err(BookingError::Expired);
            }
            pending.state = BookingState::Requesting;
            pending.clone()
        };

        let outcome = self
            .provider
            .request(
                profile_id,
                &pending.pickup,
                &pending.dropoff,
                &pending.quote,
            )
            .await;
        let state = match &outcome {
            Ok(ride) => BookingState::Requested { ride: ride.clone() },
            Err(e) => BookingState::Failed {
                reason: format!("{e:#}"),
            },
        };
        self.set_state(id, state);
        outcome.map_err(BookingError::Provider)
    }

    pub fn decline(&self, id: &str, profile_id: &str) -> Result<(), BookingError> {
        let mut rides = self.lock();
        let pending = owned(&mut rides, id, profile_id)?;
        match &pending.state {
            BookingState::AwaitingConfirmation => {
                pending.state = BookingState::Declined;
                Ok(())
            }
            other => Err(BookingError::AlreadyDecided(state_word(other))),
        }
    }

    /// Cancel a requested ride with the provider. The provider may charge a cancellation fee.
    pub async fn cancel(&self, id: &str, profile_id: &str) -> Result<(), BookingError> {
        let request_id = self.requested_id(id, profile_id)?;
        self.provider
            .cancel(profile_id, &request_id)
            .await
            .map_err(BookingError::Provider)
    }

    /// Read the ride from the provider. Returns it with what to tell the member, if anything
    /// changed that is worth telling.
    pub async fn refresh(
        &self,
        id: &str,
        profile_id: &str,
    ) -> Result<(Ride, Option<String>), BookingError> {
        let request_id = self.requested_id(id, profile_id)?;
        let previous = match self.lock().get(id).map(|p| p.state.clone()) {
            Some(BookingState::Requested { ride }) => Some(ride.status),
            _ => None,
        };
        let ride = self
            .provider
            .ride(profile_id, &request_id)
            .await
            .map_err(BookingError::Provider)?;
        let announcement = ride.announcement(previous.as_ref());
        self.set_state(id, BookingState::Requested { ride: ride.clone() });
        Ok((ride, announcement))
    }

    /// Rides still worth reading: requested and not finished. For the tracker.
    pub fn active(&self) -> Vec<PendingRide> {
        self.lock()
            .values()
            .filter(|p| matches!(&p.state, BookingState::Requested { ride } if !ride.status.is_terminal()))
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &str, profile_id: &str) -> Result<PendingRide, BookingError> {
        let mut rides = self.lock();
        owned(&mut rides, id, profile_id).map(|p| p.clone())
    }

    fn requested_id(&self, id: &str, profile_id: &str) -> Result<String, BookingError> {
        let mut rides = self.lock();
        match &owned(&mut rides, id, profile_id)?.state {
            BookingState::Requested { ride } => Ok(ride.request_id.clone()),
            _ => Err(BookingError::NotRequested),
        }
    }

    fn set_state(&self, id: &str, state: BookingState) {
        if let Some(p) = self.lock().get_mut(id) {
            p.state = state;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, PendingRide>> {
        // A poisoned map still holds consistent rows: every write is a single assignment.
        self.rides.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn owned<'a>(
    rides: &'a mut HashMap<String, PendingRide>,
    id: &str,
    profile_id: &str,
) -> Result<&'a mut PendingRide, BookingError> {
    let pending = rides.get_mut(id).ok_or(BookingError::NotFound)?;
    if pending.profile_id != profile_id {
        return Err(BookingError::NotYours);
    }
    Ok(pending)
}

fn state_word(state: &BookingState) -> &'static str {
    match state {
        BookingState::AwaitingConfirmation => "waiting",
        BookingState::Requesting => "being requested",
        BookingState::Requested { .. } => "requested",
        BookingState::Declined => "declined",
        BookingState::Failed { .. } => "tried and failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rides::domain::RideStatus;
    use crate::rides::mocks::MockRideProvider;
    use chrono::Duration;

    fn place(name: &str) -> Place {
        Place {
            name: name.to_string(),
            latitude: -1.3,
            longitude: 36.8,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-06T08:00:00Z".parse().unwrap()
    }

    async fn quoted(provider: Arc<MockRideProvider>) -> (RideBooking, PendingRide) {
        let booking = RideBooking::new(provider);
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now())
            .await
            .unwrap();
        (booking, pending)
    }

    #[tokio::test]
    async fn a_quote_requests_nothing() {
        let provider = Arc::new(MockRideProvider::new());
        let (_, pending) = quoted(provider.clone()).await;
        assert_eq!(pending.state, BookingState::AwaitingConfirmation);
        assert_eq!(provider.requests(), 0);
    }

    #[tokio::test]
    async fn the_member_who_asked_confirms_and_the_ride_is_requested_once() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;

        let ride = booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert_eq!(ride.status, RideStatus::Processing);

        let again = booking.confirm(&pending.id, "liz", now()).await;
        assert!(matches!(
            again,
            Err(BookingError::AlreadyDecided("requested"))
        ));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn another_member_cannot_confirm_decline_or_cancel() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;

        assert!(matches!(
            booking.confirm(&pending.id, "jerry", now()).await,
            Err(BookingError::NotYours)
        ));
        assert!(matches!(
            booking.decline(&pending.id, "jerry"),
            Err(BookingError::NotYours)
        ));
        booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert!(matches!(
            booking.cancel(&pending.id, "jerry").await,
            Err(BookingError::NotYours)
        ));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn an_expired_fare_cannot_be_confirmed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        let late = pending.quote.expires_at + Duration::seconds(1);
        assert!(matches!(
            booking.confirm(&pending.id, "liz", late).await,
            Err(BookingError::Expired)
        ));
        assert_eq!(provider.requests(), 0);
    }

    #[tokio::test]
    async fn a_declined_ride_cannot_then_be_confirmed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.decline(&pending.id, "liz").unwrap();
        assert!(matches!(
            booking.confirm(&pending.id, "liz", now()).await,
            Err(BookingError::AlreadyDecided("declined"))
        ));
        assert_eq!(provider.requests(), 0);
    }

    #[tokio::test]
    async fn a_failed_request_is_never_retried() {
        let provider = Arc::new(MockRideProvider::new().failing_requests());
        let (booking, pending) = quoted(provider.clone()).await;
        assert!(matches!(
            booking.confirm(&pending.id, "liz", now()).await,
            Err(BookingError::Provider(_))
        ));
        assert!(matches!(
            booking.confirm(&pending.id, "liz", now()).await,
            Err(BookingError::AlreadyDecided("tried and failed"))
        ));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn refresh_announces_a_change_once() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.confirm(&pending.id, "liz", now()).await.unwrap();

        provider.set_status(RideStatus::Accepted);
        let (_, first) = booking.refresh(&pending.id, "liz").await.unwrap();
        assert!(first.is_some());
        let (_, second) = booking.refresh(&pending.id, "liz").await.unwrap();
        assert!(second.is_none(), "an unchanged status announced again");

        provider.set_status(RideStatus::Completed);
        booking.refresh(&pending.id, "liz").await.unwrap();
        assert!(
            booking.active().is_empty(),
            "a finished ride is still tracked"
        );
    }

    #[tokio::test]
    async fn an_unrequested_ride_cannot_be_cancelled_or_refreshed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider).await;
        assert!(matches!(
            booking.cancel(&pending.id, "liz").await,
            Err(BookingError::NotRequested)
        ));
        assert!(matches!(
            booking.refresh(&pending.id, "liz").await,
            Err(BookingError::NotRequested)
        ));
    }
}
