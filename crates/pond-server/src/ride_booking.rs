//! Switching on ride booking: each member's Uber sign-in (through Jarida's credentials service),
//! the booking rules, the routes the phone uses, the `book_ride` tool, and the ride tracker.

use std::sync::Arc;
use std::time::Duration;

use pond_adapters_uber::accounts::{SignInRelay, UberAccounts};
use pond_adapters_uber::{UberConfig, UberRides};
use pond_core::mcp::ports::notification::MemberNotifier;
use pond_core::rides::booking::RideBooking;
use pond_core::security::ports::secret::SecretRepository;

const POLL_ENV: &str = "GIAP_RIDE_POLL_SECS";
const DEFAULT_POLL: Duration = Duration::from_secs(15);
/// Faster than this and a pond with a ride under way calls Uber more than any update needs.
const MIN_POLL: Duration = Duration::from_secs(5);

/// How often booked rides are read, from `GIAP_RIDE_POLL_SECS`.
pub fn poll_interval(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .map_or(DEFAULT_POLL, |d| d.max(MIN_POLL))
}

/// Turn ride booking on, or say why it stays off. Needs the secret store (member sign-ins) and
/// the credentials service (Uber's client secret); without either, `book_ride` and the phone's
/// ride routes answer that booking is not set up.
pub fn start(
    secrets: Option<Arc<dyn SecretRepository + Send + Sync>>,
    relay_url: Option<String>,
    notifier: Arc<dyn MemberNotifier>,
) -> Option<Arc<RideBooking>> {
    let (Some(secrets), Some(relay_url)) = (secrets, relay_url) else {
        tracing::info!(
            "ride booking is off: it needs the secret store and Jarida's credentials service"
        );
        return None;
    };
    let http = reqwest::Client::new();
    let accounts = Arc::new(UberAccounts::new(
        secrets,
        Some(SignInRelay::new(http.clone(), &relay_url)),
    ));
    let uber = UberRides::new(http, UberConfig::from_env(), accounts.clone());
    let booking = Arc::new(RideBooking::new(Arc::new(uber)));

    pond_api::rides::install(booking.clone());
    pond_mcp_server::travel::init_ride_accounts(accounts);

    let interval = poll_interval(std::env::var(POLL_ENV).ok().as_deref());
    let tracked = booking.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            pond_core::rides::tracking::track_once(&tracked, notifier.as_ref()).await;
        }
    });
    tracing::info!(poll_secs = interval.as_secs(), "ride booking is on (Uber)");
    Some(booking)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poll_interval_defaults_and_cannot_be_set_too_fast() {
        assert_eq!(poll_interval(None), DEFAULT_POLL);
        assert_eq!(poll_interval(Some("not a number")), DEFAULT_POLL);
        assert_eq!(poll_interval(Some("30")), Duration::from_secs(30));
        assert_eq!(poll_interval(Some("1")), MIN_POLL);
    }

    #[tokio::test]
    async fn booking_stays_off_without_sign_in_support() {
        let notifier: Arc<dyn MemberNotifier> =
            Arc::new(pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier::new());
        assert!(start(
            None,
            Some("https://credentials.example".into()),
            notifier.clone()
        )
        .is_none());
    }
}
