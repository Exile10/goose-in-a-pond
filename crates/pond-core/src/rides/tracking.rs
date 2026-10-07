//! Following booked rides: one pass reads every ride still under way and tells its member what
//! changed. The caller decides how often to run it.

use chrono::Utc;

use super::booking::RideBooking;
use super::domain::BookingState;
use crate::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification};

/// One pass. Returns how many members were sent an update. A ride that cannot be read, or an
/// update that could not be sent, is tried again on the next pass; it never stops the others.
pub async fn track_once(booking: &RideBooking, notifier: &dyn MemberNotifier) -> usize {
    let mut sent = 0;
    for pending in booking.to_follow() {
        let read = match &pending.state {
            BookingState::OutcomeUnknown { .. } => booking
                .recheck(&pending.id, &pending.profile_id)
                .await
                .map(|_| ()),
            BookingState::Requested { ride } if !ride.status.is_terminal() => booking
                .refresh(&pending.id, &pending.profile_id)
                .await
                .map(|_| ()),
            _ => Ok(()),
        };
        if let Err(e) = read {
            tracing::warn!(ride = %pending.id, error = %e, "could not read a ride's status");
            continue;
        }
        let Some(news) = booking.news(&pending.id) else {
            continue;
        };
        let Some(message) = news.message else {
            booking.told(&pending.id, &news.status);
            continue;
        };
        let notification = Notification {
            id: format!("ride-{}-{:?}", pending.id, news.status),
            target: pending.profile_id.clone(),
            category: "info".to_string(),
            title: pending.title(),
            body: message,
            timestamp: Utc::now().to_rfc3339(),
            data: Some(serde_json::json!({
                "action": "ride_update",
                "ride_id": pending.id,
                "status": news.status,
            })),
        };
        match notifier
            .notify_member(&pending.profile_id, notification)
            .await
        {
            MemberDelivery::Reached(_) => {
                booking.told(&pending.id, &news.status);
                sent += 1;
            }
            // Nobody to tell: trying again would only fail the same way.
            MemberDelivery::NoPhone => {
                tracing::info!(ride = %pending.id, "a ride update has no phone to go to");
                booking.told(&pending.id, &news.status);
            }
            MemberDelivery::Failed(why) => {
                tracing::warn!(ride = %pending.id, error = %why, "a ride update was not sent; trying again next pass");
            }
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use crate::rides::domain::{Place, RideStatus};
    use crate::rides::mocks::{MockRideProvider, OnCurrent, OnRequest};
    use std::sync::Arc;

    fn place(name: &str) -> Place {
        Place {
            name: name.to_string(),
            latitude: -1.3,
            longitude: 36.8,
        }
    }

    async fn booked(provider: Arc<MockRideProvider>) -> (RideBooking, String) {
        let booking = RideBooking::new(provider);
        let now = Utc::now();
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now)
            .await
            .unwrap();
        booking.confirm(&pending.id, "liz", now).await.unwrap();
        (booking, pending.id)
    }

    #[tokio::test]
    async fn a_status_change_reaches_the_member_once() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, ride_id) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert_eq!(
            track_once(&booking, &notifier).await,
            0,
            "the same status was sent twice"
        );

        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "liz");
        assert_eq!(sent[0].1.title, "Your ride to JKIA");
        assert!(sent[0].1.body.contains("Amina"), "{}", sent[0].1.body);
        let data = sent[0].1.data.as_ref().unwrap();
        assert_eq!(data["action"], "ride_update");
        assert_eq!(data["ride_id"], ride_id);
    }

    #[tokio::test]
    async fn a_finished_ride_stops_being_followed() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        provider.set_status(RideStatus::Completed);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        provider.set_status(RideStatus::Arriving);
        assert_eq!(
            track_once(&booking, &notifier).await,
            0,
            "a completed ride was read again"
        );
    }

    #[tokio::test]
    async fn an_update_that_failed_to_send_is_sent_on_the_next_pass() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new()
            .with_devices("liz", &["liz-phone"])
            .failing_next(1);

        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        let sent = notifier.sent();
        assert_eq!(sent.len(), 2, "one failed try, one delivery");
        assert_eq!(
            sent[0].1.body, sent[1].1.body,
            "the retry carries the same news"
        );
        assert_eq!(track_once(&booking, &notifier).await, 0);
    }

    #[tokio::test]
    async fn an_arrival_that_failed_to_send_is_retried_without_reading_the_ride_again() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new()
            .with_devices("liz", &["liz-phone"])
            .failing_next(1);

        provider.set_status(RideStatus::Completed);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        // A finished ride is not read again, so this later status is never seen.
        provider.set_status(RideStatus::Arriving);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert_eq!(notifier.sent()[1].1.body, "You have arrived.");
        assert!(booking.to_follow().is_empty());
    }

    #[tokio::test]
    async fn a_member_with_no_phone_is_not_tried_again_and_again() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider.clone()).await;
        let notifier = MockMemberNotifier::new();

        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert_eq!(notifier.sent().len(), 1);
    }

    #[tokio::test]
    async fn a_ride_whose_answer_was_lost_is_found_and_followed() {
        let provider = Arc::new(MockRideProvider::new().on_request(OnRequest::LoseTheAnswer));
        provider.set_current(OnCurrent::Unreachable);
        let booking = RideBooking::new(provider.clone());
        let now = Utc::now();
        let pending = booking
            .quote("liz", place("Home"), place("JKIA"), now)
            .await
            .unwrap();
        booking.confirm(&pending.id, "liz", now).await.unwrap();
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);

        assert_eq!(track_once(&booking, &notifier).await, 0);
        provider.set_current(OnCurrent::TheRide);
        provider.set_status(RideStatus::Accepted);
        assert_eq!(track_once(&booking, &notifier).await, 1);
        assert!(notifier.sent()[0].1.body.contains("Amina"));
        assert_eq!(provider.requests(), 1);
    }

    #[tokio::test]
    async fn nothing_is_sent_for_a_ride_still_being_matched() {
        let provider = Arc::new(MockRideProvider::new());
        let (booking, _) = booked(provider).await;
        let notifier = MockMemberNotifier::new().with_devices("liz", &["liz-phone"]);
        assert_eq!(track_once(&booking, &notifier).await, 0);
        assert!(notifier.sent().is_empty());
    }
}
