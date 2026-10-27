//! Raises the household's system notices ([`SystemEvent`]) through [`NotificationSender`], and
//! keeps flapping health from becoming a stream of them ([`OutageGate`], [`disk_is_low`]).
//!
//! Every producer of a system notice goes through [`SystemNotices`], so the household's switch
//! (`Settings::system_notices_enabled`), the wording and the logging are decided in one place.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mcp::domain::system_notice::SystemEvent;
use crate::mcp::ports::notification::NotificationSender;
use crate::user_data::ports::settings::SettingsRepository;

/// What became of one announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announced {
    /// Handed to the sender: queued for every paired phone and published live.
    Sent,
    /// The household switched system notices off, and this one is not a security notice.
    Muted,
    /// The sender refused it; the reason is for logs.
    Failed(String),
}

/// The one way a system notice reaches the household.
pub struct SystemNotices {
    sender: Arc<dyn NotificationSender>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
}

impl SystemNotices {
    pub fn new(
        sender: Arc<dyn NotificationSender>,
        settings: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self { sender, settings }
    }

    /// Tell every paired phone.
    pub async fn announce(&self, event: SystemEvent) -> Announced {
        self.raise(event, None).await
    }

    /// Tell every paired phone except `actor_device`, whose own action this notice is about.
    pub async fn announce_except(&self, event: SystemEvent, actor_device: &str) -> Announced {
        self.raise(event, Some(actor_device)).await
    }

    async fn raise(&self, event: SystemEvent, except: Option<&str>) -> Announced {
        let notice = event.kind();
        if !event.is_security() {
            match self.settings.get().await {
                Ok(settings) if !settings.system_notices_enabled => {
                    tracing::info!(
                        kind = "system_notice_muted",
                        notice,
                        "system notice not sent: the household switched system notices off"
                    );
                    return Announced::Muted;
                }
                Ok(_) => {}
                // Telling is the safer failure: a notice nobody wanted costs less than an outage
                // nobody heard about.
                Err(e) => tracing::warn!(
                    kind = "system_notice_settings_unreadable",
                    notice,
                    error = %e,
                    "could not read whether system notices are on; sending this one"
                ),
            }
        }
        let notification = event.notification(
            uuid::Uuid::new_v4().to_string(),
            chrono::Utc::now().to_rfc3339(),
        );
        let id = notification.id.clone();
        let sent = match except {
            Some(device) => self.sender.broadcast_except(notification, device).await,
            None => self.sender.broadcast(notification).await,
        };
        match sent {
            Ok(()) => {
                tracing::info!(
                    kind = "system_notice_sent",
                    notice,
                    notification_id = %id,
                    except_device = except.unwrap_or(""),
                    "system notice sent to the household"
                );
                Announced::Sent
            }
            Err(e) => {
                tracing::warn!(
                    kind = "system_notice_failed",
                    notice,
                    notification_id = %id,
                    error = %e,
                    "system notice could not be sent"
                );
                Announced::Failed(e.to_string())
            }
        }
    }
}

/// A health transition worth telling the household about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutageNotice {
    /// The component has been unhealthy for the whole grace period.
    Down,
    /// It is healthy again after a [`OutageNotice::Down`] was announced.
    Recovered,
}

#[derive(Debug, Default)]
struct Track {
    unhealthy_since: Option<Instant>,
    announced: bool,
    last_announced: Option<Instant>,
}

/// Turns a component's health, observed again and again, into at most one notice per outage.
///
/// - An outage is announced only once it has lasted `grace`, so a restart's blip says nothing.
/// - A recovery is announced only for an outage that was, so a flap inside `grace` is silent.
/// - After an announcement, the next outage of the same component waits out `cooldown` before it
///   is announced; one that ends before then is never announced, and neither is its recovery.
#[derive(Debug)]
pub struct OutageGate {
    grace: Duration,
    cooldown: Duration,
    components: HashMap<String, Track>,
}

impl OutageGate {
    pub fn new(grace: Duration, cooldown: Duration) -> Self {
        Self {
            grace,
            cooldown,
            components: HashMap::new(),
        }
    }

    /// Record one observation of `component` at `now`; returns the notice it calls for, if any.
    pub fn observe(
        &mut self,
        component: &str,
        healthy: bool,
        now: Instant,
    ) -> Option<OutageNotice> {
        let track = self.components.entry(component.to_string()).or_default();
        if healthy {
            track.unhealthy_since = None;
            if track.announced {
                track.announced = false;
                return Some(OutageNotice::Recovered);
            }
            return None;
        }
        let since = *track.unhealthy_since.get_or_insert(now);
        let lasted = now.saturating_duration_since(since) >= self.grace;
        let cooled = track
            .last_announced
            .is_none_or(|at| now.saturating_duration_since(at) >= self.cooldown);
        if !track.announced && lasted && cooled {
            track.announced = true;
            track.last_announced = Some(now);
            return Some(OutageNotice::Down);
        }
        None
    }

    /// Forget `component`'s outage without a notice: the household switched it off, so it is
    /// neither down nor back.
    pub fn forget(&mut self, component: &str) {
        if let Some(track) = self.components.get_mut(component) {
            track.unhealthy_since = None;
            track.announced = false;
        }
    }
}

/// Free space below which the disk counts as low: 5 % of the disk, and never under 2 GB, which is
/// about one small model.
pub const DISK_LOW_FLOOR_BYTES: u64 = 2_000_000_000;
/// The fraction of the disk below which free space counts as low.
pub const DISK_LOW_FRACTION: f64 = 0.05;
/// How far above the low-water mark free space must climb to count as recovered, so a disk that
/// hovers at the mark does not flap.
pub const DISK_RECOVERY_MARGIN_BYTES: u64 = 1_000_000_000;

/// Whether the disk is low, given whether it was low at the last look. Between the low-water mark
/// and the mark plus [`DISK_RECOVERY_MARGIN_BYTES`] the answer stays what it was.
pub fn disk_is_low(free_bytes: u64, total_bytes: u64, was_low: bool) -> bool {
    let fraction = (total_bytes as f64 * DISK_LOW_FRACTION) as u64;
    let mark = fraction.max(DISK_LOW_FLOOR_BYTES);
    if free_bytes < mark {
        true
    } else if free_bytes >= mark.saturating_add(DISK_RECOVERY_MARGIN_BYTES) {
        false
    } else {
        was_low
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::mocks::mock_notification_sender::{Delivery, MockNotificationSender};
    use crate::user_data::mocks::mock_settings::MockSettingsRepository;

    const MIN: Duration = Duration::from_secs(60);

    fn gate() -> OutageGate {
        OutageGate::new(5 * MIN, 60 * MIN)
    }

    #[test]
    fn a_blip_shorter_than_the_grace_period_says_nothing() {
        let mut gate = gate();
        let t0 = Instant::now();
        assert_eq!(gate.observe("ra", false, t0), None);
        assert_eq!(gate.observe("ra", false, t0 + 4 * MIN), None);
        assert_eq!(gate.observe("ra", true, t0 + 5 * MIN), None);
    }

    #[test]
    fn an_outage_is_announced_once_and_its_recovery_once() {
        let mut gate = gate();
        let t0 = Instant::now();
        assert_eq!(gate.observe("ra", false, t0), None);
        assert_eq!(
            gate.observe("ra", false, t0 + 5 * MIN),
            Some(OutageNotice::Down)
        );
        assert_eq!(gate.observe("ra", false, t0 + 6 * MIN), None);
        assert_eq!(gate.observe("ra", false, t0 + 90 * MIN), None);
        assert_eq!(
            gate.observe("ra", true, t0 + 91 * MIN),
            Some(OutageNotice::Recovered)
        );
        assert_eq!(gate.observe("ra", true, t0 + 92 * MIN), None);
    }

    #[test]
    fn a_flapping_component_is_announced_once_per_cooldown_and_silent_recoveries_follow() {
        let mut gate = gate();
        let t0 = Instant::now();
        gate.observe("ra", false, t0);
        assert_eq!(
            gate.observe("ra", false, t0 + 5 * MIN),
            Some(OutageNotice::Down)
        );
        assert_eq!(
            gate.observe("ra", true, t0 + 6 * MIN),
            Some(OutageNotice::Recovered)
        );
        // Down again ten minutes later, for longer than the grace: inside the cooldown.
        gate.observe("ra", false, t0 + 10 * MIN);
        assert_eq!(gate.observe("ra", false, t0 + 20 * MIN), None);
        // Its recovery is not news either: the outage never was.
        assert_eq!(gate.observe("ra", true, t0 + 21 * MIN), None);
        // An outage that outlasts the cooldown is announced when the cooldown ends.
        gate.observe("ra", false, t0 + 30 * MIN);
        assert_eq!(gate.observe("ra", false, t0 + 64 * MIN), None);
        assert_eq!(
            gate.observe("ra", false, t0 + 65 * MIN),
            Some(OutageNotice::Down)
        );
    }

    #[test]
    fn components_are_tracked_apart_and_forgetting_one_is_silent() {
        let mut gate = gate();
        let t0 = Instant::now();
        gate.observe("a", false, t0);
        gate.observe("b", false, t0);
        assert_eq!(
            gate.observe("a", false, t0 + 5 * MIN),
            Some(OutageNotice::Down)
        );
        assert_eq!(
            gate.observe("b", false, t0 + 5 * MIN),
            Some(OutageNotice::Down)
        );
        gate.forget("a");
        assert_eq!(gate.observe("a", true, t0 + 6 * MIN), None);
        assert_eq!(
            gate.observe("b", true, t0 + 6 * MIN),
            Some(OutageNotice::Recovered)
        );
    }

    #[test]
    fn a_low_disk_has_hysteresis() {
        const GB: u64 = 1_000_000_000;
        // 64 GB disk: 5 % is 3.2 GB, above the 2 GB floor.
        assert!(disk_is_low(3 * GB, 64 * GB, false));
        assert!(!disk_is_low(4 * GB, 64 * GB, false));
        // Between the mark and the mark plus 1 GB, it stays what it was.
        assert!(disk_is_low(3_500_000_000, 64 * GB, true));
        assert!(!disk_is_low(3_500_000_000, 64 * GB, false));
        assert!(!disk_is_low(4_300_000_000, 64 * GB, true));
        // A small disk uses the 2 GB floor.
        assert!(disk_is_low(1_900_000_000, 16 * GB, false));
        assert!(!disk_is_low(3_100_000_000, 16 * GB, true));
    }

    async fn notices(enabled: bool) -> (SystemNotices, Arc<MockNotificationSender>) {
        let sender = Arc::new(MockNotificationSender::new());
        let repo = Arc::new(MockSettingsRepository::new());
        repo.set_key("system_notices_enabled", enabled.to_string())
            .await
            .unwrap();
        (SystemNotices::new(sender.clone(), repo), sender)
    }

    #[tokio::test]
    async fn a_notice_is_broadcast_to_the_household() {
        let (notices, sender) = notices(true).await;
        assert_eq!(
            notices.announce(SystemEvent::RemoteAccessDown).await,
            Announced::Sent
        );
        let sent = sender.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, Delivery::Broadcast);
        assert_eq!(sent[0].1.title, "Remote access stopped");
    }

    #[tokio::test]
    async fn a_muted_household_hears_no_system_notice_but_still_hears_security() {
        let (notices, sender) = notices(false).await;
        assert_eq!(
            notices.announce(SystemEvent::RemoteAccessDown).await,
            Announced::Muted
        );
        assert!(sender.sent().is_empty());
        assert_eq!(
            notices
                .announce(SystemEvent::DeviceRemoved {
                    name: "Liz's phone".into()
                })
                .await,
            Announced::Sent
        );
        assert_eq!(sender.sent().len(), 1);
    }

    #[tokio::test]
    async fn a_notice_about_a_device_s_own_action_skips_that_device() {
        let (notices, sender) = notices(true).await;
        notices
            .announce_except(
                SystemEvent::DeviceRemoved {
                    name: "old tablet".into(),
                },
                "jerry-phone",
            )
            .await;
        assert_eq!(
            sender.sent()[0].0,
            Delivery::BroadcastExcept("jerry-phone".into())
        );
    }

    #[tokio::test]
    async fn a_sender_failure_is_reported_not_swallowed() {
        let sender = Arc::new(MockNotificationSender::new().failing());
        let repo = Arc::new(MockSettingsRepository::new());
        let notices = SystemNotices::new(sender, repo);
        assert!(matches!(
            notices.announce(SystemEvent::RemoteAccessDown).await,
            Announced::Failed(_)
        ));
    }
}
