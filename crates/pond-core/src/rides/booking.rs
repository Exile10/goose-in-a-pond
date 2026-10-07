//! Quote, confirm, request, track and cancel one member's rides. The member who asked confirms
//! on their own phone; nothing is requested before that, and a quote is confirmed at most once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use super::domain::{BookingState, PendingRide, Place, Ride, RideStatus};
use super::ports::RideProvider;
use crate::user_data::services::nearby::distance_km;

/// Farther than this, a drop-off is a place matched wrongly, not a ride anyone means to take.
pub const MAX_RIDE_KM: f64 = 150.0;

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
    #[error("the drop-off is {0:.0} km from the pickup, too far for a ride booked here")]
    TooFar(f64),
    #[error("{0}")]
    Provider(#[source] anyhow::Error),
}

pub struct RideBooking {
    provider: Arc<dyn RideProvider>,
    rides: Mutex<HashMap<String, Entry>>,
}

/// A ride and what its member has been told about it.
struct Entry {
    ride: PendingRide,
    /// The last status the member was told about, or that needed no telling. Trails the ride's own
    /// status until an update is delivered, so an update that failed to send is sent again.
    announced: Option<RideStatus>,
}

/// A status the member has not heard about yet.
#[derive(Debug, Clone, PartialEq)]
pub struct RideNews {
    pub status: RideStatus,
    /// What to tell them; `None` when the change is not worth a notification.
    pub message: Option<String>,
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
        let km = distance_km(
            (pickup.latitude, pickup.longitude),
            (dropoff.latitude, dropoff.longitude),
        );
        if km > MAX_RIDE_KM {
            return Err(BookingError::TooFar(km));
        }
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
        self.lock().insert(
            pending.id.clone(),
            Entry {
                ride: pending.clone(),
                announced: None,
            },
        );
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

    /// Read the ride from the provider and keep it.
    pub async fn refresh(&self, id: &str, profile_id: &str) -> Result<Ride, BookingError> {
        let request_id = self.requested_id(id, profile_id)?;
        let ride = self
            .provider
            .ride(profile_id, &request_id)
            .await
            .map_err(BookingError::Provider)?;
        self.set_state(id, BookingState::Requested { ride: ride.clone() });
        Ok(ride)
    }

    /// Rides the tracker should look at: requested and still under way, or finished with news
    /// the member has not had yet.
    pub fn to_follow(&self) -> Vec<PendingRide> {
        self.lock()
            .values()
            .filter(|e| match &e.ride.state {
                BookingState::Requested { ride } => {
                    !ride.status.is_terminal() || e.announced.as_ref() != Some(&ride.status)
                }
                _ => false,
            })
            .map(|e| e.ride.clone())
            .collect()
    }

    /// The ride's status, when its member has not been told about it yet.
    pub fn news(&self, id: &str) -> Option<RideNews> {
        let rides = self.lock();
        let entry = rides.get(id)?;
        let BookingState::Requested { ride } = &entry.ride.state else {
            return None;
        };
        if entry.announced.as_ref() == Some(&ride.status) {
            return None;
        }
        Some(RideNews {
            status: ride.status.clone(),
            message: ride.announcement(entry.announced.as_ref()),
        })
    }

    /// Record that the member has had the news of `status`, so it is not sent again.
    pub fn told(&self, id: &str, status: &RideStatus) {
        if let Some(entry) = self.lock().get_mut(id) {
            entry.announced = Some(status.clone());
        }
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
        if let Some(entry) = self.lock().get_mut(id) {
            entry.ride.state = state;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // A poisoned map still holds consistent rows: every write is a single assignment.
        self.rides.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn owned<'a>(
    rides: &'a mut HashMap<String, Entry>,
    id: &str,
    profile_id: &str,
) -> Result<&'a mut PendingRide, BookingError> {
    let entry = rides.get_mut(id).ok_or(BookingError::NotFound)?;
    if entry.ride.profile_id != profile_id {
        return Err(BookingError::NotYours);
    }
    Ok(&mut entry.ride)
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
    async fn a_drop_off_too_far_from_the_pickup_is_never_quoted() {
        let provider = Arc::new(MockRideProvider::new());
        let booking = RideBooking::new(provider.clone());
        let mombasa = Place {
            name: "Mombasa".to_string(),
            latitude: -4.0435,
            longitude: 39.6682,
        };
        assert!(matches!(
            booking.quote("liz", place("Home"), mombasa, now()).await,
            Err(BookingError::TooFar(km)) if km > MAX_RIDE_KM
        ));
        assert_eq!(
            provider.quotes(),
            0,
            "a refused trip still asked for a fare"
        );
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
    async fn news_stays_until_the_member_is_told_and_then_stops() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.confirm(&pending.id, "liz", now()).await.unwrap();

        provider.set_status(RideStatus::Accepted);
        booking.refresh(&pending.id, "liz").await.unwrap();
        let news = booking.news(&pending.id).expect("a new status is news");
        assert!(news.message.is_some());
        assert_eq!(
            booking.news(&pending.id),
            Some(news.clone()),
            "news the member was never told about vanished"
        );
        booking.told(&pending.id, &news.status);
        booking.refresh(&pending.id, "liz").await.unwrap();
        assert_eq!(
            booking.news(&pending.id),
            None,
            "an unchanged status is news again"
        );

        provider.set_status(RideStatus::Completed);
        booking.refresh(&pending.id, "liz").await.unwrap();
        assert_eq!(
            booking.to_follow().len(),
            1,
            "an untold arrival was dropped"
        );
        booking.told(&pending.id, &RideStatus::Completed);
        assert!(
            booking.to_follow().is_empty(),
            "a finished ride the member was told about is still followed"
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
