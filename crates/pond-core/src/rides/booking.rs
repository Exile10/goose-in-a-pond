//! Quote, confirm, request, track and cancel one member's rides. The member who asked confirms
//! on their own phone; nothing is requested before that, and a quote is confirmed at most once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};

use super::domain::{
    BookingState, PendingRide, Place, QuotedTrip, RequestFailure, Ride, RideStatus,
};
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
            trip: Some(QuotedTrip {
                pickup,
                dropoff,
                quote,
            }),
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
    /// Returns where the ride then stands: requested, or, when the provider's answer was lost and
    /// no trip of the member's shows the ride, [`BookingState::OutcomeUnknown`].
    pub async fn confirm(
        &self,
        id: &str,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<BookingState, BookingError> {
        // Claimed under the lock, so two taps on Confirm cannot both reach the provider.
        let trip = {
            let mut rides = self.lock();
            let pending = owned(&mut rides, id, profile_id)?;
            let trip = match (&pending.state, &pending.trip) {
                (BookingState::AwaitingConfirmation, Some(trip)) => trip.clone(),
                (other, _) => return Err(BookingError::AlreadyDecided(state_word(other))),
            };
            if now >= trip.quote.expires_at {
                return Err(BookingError::Expired);
            }
            pending.state = BookingState::Requesting;
            trip
        };

        let outcome = self
            .provider
            .request(profile_id, &trip.pickup, &trip.dropoff, &trip.quote)
            .await;
        let state = match outcome {
            Ok(ride) => BookingState::Requested { ride },
            Err(RequestFailure::Refused(reason)) => {
                self.set_state(
                    id,
                    BookingState::Failed {
                        reason: reason.clone(),
                    },
                );
                return Err(BookingError::Provider(anyhow::anyhow!(reason)));
            }
            Err(RequestFailure::Uncertain(reason)) => {
                tracing::warn!(ride = %id, error = %reason, "a ride request got no clear answer");
                match self.live_trip(Some(id), profile_id).await {
                    Ok(Some(ride)) => BookingState::Requested { ride },
                    Ok(None) => BookingState::OutcomeUnknown { reason },
                    Err(e) => {
                        tracing::warn!(ride = %id, error = %format!("{e:#}"), "could not read the member's current trip");
                        BookingState::OutcomeUnknown { reason }
                    }
                }
            }
        };
        self.set_state(id, state.clone());
        Ok(state)
    }

    /// Ask the provider again about a ride whose request got no clear answer: the member's live
    /// trip becomes the ride. Until one shows, it stays [`BookingState::OutcomeUnknown`].
    pub async fn recheck(&self, id: &str, profile_id: &str) -> Result<BookingState, BookingError> {
        let state = self.get(id, profile_id)?.state;
        if !matches!(state, BookingState::OutcomeUnknown { .. }) {
            return Ok(state);
        }
        match self
            .live_trip(Some(id), profile_id)
            .await
            .map_err(BookingError::Provider)?
        {
            Some(ride) => {
                let requested = BookingState::Requested { ride };
                self.set_state(id, requested.clone());
                Ok(requested)
            }
            None => Ok(state),
        }
    }

    /// The member's trip under way; `None` when there is none, or a ride here other than
    /// `except` already follows it.
    async fn live_trip(
        &self,
        except: Option<&str>,
        profile_id: &str,
    ) -> anyhow::Result<Option<Ride>> {
        let Some(ride) = self.provider.current(profile_id).await? else {
            return Ok(None);
        };
        if ride.status.is_terminal() || followed(&self.lock(), except, &ride.request_id) {
            return Ok(None);
        }
        Ok(Some(ride))
    }

    /// Follow the member's trip under way with the provider. Rides live only in memory, so this
    /// is how one in flight when the pond restarted is followed again. Returns its id here, or
    /// `None` when there is no trip under way or a ride here already follows it.
    pub async fn take_over_current(
        &self,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, BookingError> {
        let Some(ride) = self
            .live_trip(None, profile_id)
            .await
            .map_err(BookingError::Provider)?
        else {
            return Ok(None);
        };
        // The same id on every restart, so a link in an earlier update still finds it.
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{}:{}", self.provider.name(), ride.request_id).as_bytes(),
        )
        .to_string();
        let mut rides = self.lock();
        if rides.contains_key(&id) || followed(&rides, None, &ride.request_id) {
            return Ok(None);
        }
        // Told already, as far as anyone here knows; only what changes from now is sent.
        let announced = Some(ride.status.clone());
        rides.insert(
            id.clone(),
            Entry {
                ride: PendingRide {
                    id: id.clone(),
                    profile_id: profile_id.to_string(),
                    trip: None,
                    created_at: now,
                    state: BookingState::Requested { ride },
                },
                announced,
            },
        );
        Ok(Some(id))
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
        self.recheck(id, profile_id).await?;
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

    /// Rides the tracker should look at: requested and still under way, finished with news the
    /// member has not had yet, or sent without a clear answer.
    pub fn to_follow(&self) -> Vec<PendingRide> {
        self.lock()
            .values()
            .filter(|e| match &e.ride.state {
                BookingState::Requested { ride } => {
                    !ride.status.is_terminal() || e.announced.as_ref() != Some(&ride.status)
                }
                BookingState::OutcomeUnknown { .. } => true,
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

/// Whether a ride here other than `except` follows the provider's `request_id`.
fn followed(rides: &HashMap<String, Entry>, except: Option<&str>, request_id: &str) -> bool {
    rides.iter().any(|(id, entry)| {
        Some(id.as_str()) != except
            && matches!(&entry.ride.state, BookingState::Requested { ride }
                if ride.request_id == request_id)
    })
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
        BookingState::OutcomeUnknown { .. } => "sent without a clear answer",
        BookingState::Declined => "declined",
        BookingState::Failed { .. } => "tried and failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rides::domain::RideStatus;
    use crate::rides::mocks::{MockRideProvider, OnCurrent, OnRequest};
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

        let state = booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert!(
            matches!(&state, BookingState::Requested { ride } if ride.status == RideStatus::Processing),
            "{state:?}"
        );

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
        let late = pending.trip.as_ref().unwrap().quote.expires_at + Duration::seconds(1);
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
    async fn a_lost_answer_with_the_members_trip_under_way_is_that_ride() {
        let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
        provider.set_current(OnCurrent::TheRide);
        let (booking, pending) = quoted(provider.clone()).await;

        let state = booking.confirm(&pending.id, "liz", now()).await.unwrap();
        assert!(matches!(state, BookingState::Requested { .. }), "{state:?}");
        assert_eq!(booking.to_follow().len(), 1, "the ride is not followed");
        booking.cancel(&pending.id, "liz").await.unwrap();
        assert_eq!(provider.requests(), 1, "a lost answer was requested again");
    }

    #[tokio::test]
    async fn a_lost_answer_with_no_trip_to_read_is_unknown_never_failed() {
        for current in [OnCurrent::NoTrip, OnCurrent::Unreachable] {
            let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
            provider.set_current(current);
            let (booking, pending) = quoted(provider.clone()).await;

            let state = booking.confirm(&pending.id, "liz", now()).await.unwrap();
            assert!(
                matches!(state, BookingState::OutcomeUnknown { .. }),
                "{current:?}: {state:?}"
            );
            assert!(matches!(
                booking.confirm(&pending.id, "liz", now()).await,
                Err(BookingError::AlreadyDecided(_))
            ));
            assert_eq!(booking.to_follow().len(), 1, "{current:?}: not rechecked");

            // The trip shows up later: the recheck takes it, and then it can be cancelled.
            provider.set_current(OnCurrent::TheRide);
            assert!(matches!(
                booking.recheck(&pending.id, "liz").await.unwrap(),
                BookingState::Requested { .. }
            ));
            booking.cancel(&pending.id, "liz").await.unwrap();
            assert_eq!(provider.requests(), 1);
        }
    }

    #[tokio::test]
    async fn a_trip_another_ride_here_already_follows_is_not_taken_twice() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, first) = quoted(provider.clone()).await;
        booking.confirm(&first.id, "liz", now()).await.unwrap();
        let second = booking
            .quote("liz", place("Home"), place("JKIA"), now())
            .await
            .unwrap();

        provider.set_current(OnCurrent::TheRide);
        assert!(
            booking
                .live_trip(Some(&second.id), "liz")
                .await
                .unwrap()
                .is_none(),
            "the first ride's trip was taken as the second's"
        );
        assert!(booking
            .live_trip(Some(&first.id), "liz")
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn a_trip_under_way_at_startup_is_followed_once() {
        let provider = Arc::new(MockRideProvider::new());
        let booking = RideBooking::new(provider.clone());
        assert_eq!(booking.take_over_current("liz", now()).await.unwrap(), None);

        provider.set_current(OnCurrent::TheRide);
        provider.set_status(RideStatus::Accepted);
        let id = booking
            .take_over_current("liz", now())
            .await
            .unwrap()
            .expect("the trip under way is taken over");
        assert_eq!(
            booking.take_over_current("liz", now()).await.unwrap(),
            None,
            "the same trip was taken over twice"
        );
        let followed = booking.to_follow();
        assert_eq!(followed.len(), 1);
        assert_eq!(followed[0].id, id);
        assert_eq!(followed[0].title(), "Your ride");
        assert_eq!(
            booking.news(&id),
            None,
            "a status from before the restart was announced again"
        );

        provider.set_status(RideStatus::Arriving);
        booking.refresh(&id, "liz").await.unwrap();
        assert!(booking.news(&id).is_some());
        assert!(matches!(
            booking.cancel(&id, "jerry").await,
            Err(BookingError::NotYours)
        ));
        booking.cancel(&id, "liz").await.unwrap();
    }

    #[tokio::test]
    async fn a_finished_trip_or_one_already_followed_is_not_taken_over() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, pending) = quoted(provider.clone()).await;
        booking.confirm(&pending.id, "liz", now()).await.unwrap();
        provider.set_current(OnCurrent::TheRide);
        assert_eq!(booking.take_over_current("liz", now()).await.unwrap(), None);

        let fresh = RideBooking::new(provider.clone());
        provider.set_status(RideStatus::Completed);
        assert_eq!(fresh.take_over_current("liz", now()).await.unwrap(), None);
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
