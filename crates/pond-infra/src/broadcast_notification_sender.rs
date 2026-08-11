//! `NotificationSender` over an in-process broadcast channel + offline queue (#99).
//!
//! - foreground: publish on a `tokio::sync::broadcast` channel that connected
//!   `/notifications/stream` clients subscribe to;
//! - offline: persist targeted notifications to the queue so a disconnected
//!   device gets them on reconnect;
//! - background (scaffold): hand targeted notifications to the optional
//!   [`NotificationRelay`] (FCM/APNs), best-effort.
//!
//! STATUS, corrected 2026-08-11 by PAI-7 P5. This comment used to read "the
//! queue + relay paths are exercised only via targeted `send()` … only the
//! producer call site is pending", and half of it is still true: all four
//! production producers (`main.rs`'s schedule-completion bridge,
//! `schedule_executors.rs`'s rule `Notify`, the `send_notification` MCP tool and
//! `routes.rs`'s pairing notice) still call `broadcast()`.
//!
//! What has changed is that there is now a way to address one household member
//! rather than the house: [`BroadcastNotificationSender::send_to_profile`],
//! which resolves PAI-1 P9's `devices.profile_id` through `DeviceAttribution`
//! and delivers one copy per attributed device down the existing targeted
//! `send()` path -- queue, relay, live fan-out. **Nothing in production calls it
//! yet**, because every producer above lives in a file PAI-7 P5 did not own, and
//! the phase stamp says so rather than implying otherwise. Wiring it is one line
//! in `main.rs` (keep the concrete `Arc<BroadcastNotificationSender>` alongside
//! the `dyn NotificationSender` it already builds) plus a caller -- PAI-7 P4's
//! reviewer is the intended one.
//!
//! The rule the new path enforces is PAI-7's invariant 4: a proposal is
//! addressed to a profile and **never** broadcast. So a member with no
//! attributed device reaches nobody, a failed attribution read reaches nobody,
//! and an unwired attribution reaches nobody. None of those falls back to
//! `broadcast()`, and there is no code path here that could: the decision is
//! carried by `TargetedDelivery`, which has no variant meaning "the household".

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use pond_core::user_data::ports::device_attribution::{
    DeviceAttribution, TargetedDelivery, Undeliverable, RESERVED_BROADCAST_TARGET,
};
use tokio::sync::broadcast;

/// Sentinel `target` meaning "deliver to every connected device".
///
/// Defined from the domain constant rather than spelled again here. The
/// targeted path recognises this string in order to REFUSE it, so a second
/// literal that drifted by one character would switch the refusal off without
/// changing any line a reader would think to check.
pub const BROADCAST_TARGET: &str = RESERVED_BROADCAST_TARGET;

/// Caps applied to every notification at this single enforcement point, so no
/// producer (tool call, schedule bridge, …) can persist or stream an
/// arbitrarily large payload. Truncation is by character, never by byte, so a
/// multi-byte boundary can't panic.
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 2000;

fn clamp(mut n: Notification) -> Notification {
    if n.title.chars().count() > MAX_TITLE_CHARS {
        n.title = n.title.chars().take(MAX_TITLE_CHARS).collect();
    }
    if n.body.chars().count() > MAX_BODY_CHARS {
        n.body = n.body.chars().take(MAX_BODY_CHARS).collect();
    }
    n
}

/// What actually happened when a notification was addressed to one member.
///
/// The plan says who it was *for*; `queued` and `failed` say who it reached.
/// They are separate because they fail for different reasons and only one of
/// them is a bug: an empty `queued` under [`TargetedDelivery::Undeliverable`] is
/// the system working, and an empty `queued` under
/// [`TargetedDelivery::ToDevices`] means every write failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDeliveryReport {
    /// Who this was addressed to, and why it reached nobody when it did.
    pub plan: TargetedDelivery,
    /// Device ids the notification was queued and fanned out for.
    pub queued: Vec<String>,
    /// Device ids whose delivery failed, with the error.
    pub failed: Vec<(String, String)>,
}

impl ProfileDeliveryReport {
    /// True when nothing at all was delivered.
    pub fn reached_nobody(&self) -> bool {
        self.queued.is_empty()
    }
}

pub struct BroadcastNotificationSender {
    tx: broadcast::Sender<Notification>,
    queue: Arc<dyn NotificationQueueRepository>,
    relay: Option<Arc<dyn NotificationRelay>>,
    /// PAI-1 P9's device-to-profile rung, when it has been wired.
    ///
    /// `Option` rather than a required constructor argument so that adding
    /// targeted delivery did not change `new`'s signature -- `main.rs` and two
    /// `pond-api` integration tests build this and none of them is in PAI-7 P5's
    /// footprint. `None` is a refusal, not a fallback: see
    /// [`Self::send_to_profile`].
    attribution: Option<Arc<dyn DeviceAttribution>>,
}

impl BroadcastNotificationSender {
    pub fn new(
        tx: broadcast::Sender<Notification>,
        queue: Arc<dyn NotificationQueueRepository>,
        relay: Option<Arc<dyn NotificationRelay>>,
    ) -> Self {
        Self {
            tx,
            queue,
            relay,
            attribution: None,
        }
    }

    /// Give this sender the ability to address a household member.
    ///
    /// Without it [`Self::send_to_profile`] delivers to nobody, which is the
    /// correct direction for an unwired dependency: an assistant that cannot
    /// work out whose phone to use must not resolve that by using everybody's.
    pub fn with_device_attribution(mut self, attribution: Arc<dyn DeviceAttribution>) -> Self {
        self.attribution = Some(attribution);
        self
    }

    /// Deliver one notification to one household member's devices.
    ///
    /// PAI-7 P5, and the first path in the tree that addresses a person rather
    /// than a house. Four things about it are load-bearing:
    ///
    /// 1. **It never broadcasts.** Not when the member owns no device, not when
    ///    the attribution read fails, not when the attribution is unwired.
    ///    [`TargetedDelivery`] cannot express a broadcast, so this is a property
    ///    of the type rather than of the arms written below.
    /// 2. **Each device gets its own notification id.** `notifications.id` is the
    ///    PRIMARY KEY and `SqliteNotificationQueue::enqueue` is an
    ///    `INSERT OR REPLACE`, so reusing one id across a member's two devices
    ///    would leave exactly one queued row -- the phone that was switched off,
    ///    the one the queue exists for, being the one most likely to lose it.
    /// 3. **A per-device failure does not abort the rest.** One device with a
    ///    stale row should not cost a member the notification on their other.
    /// 4. **It returns a report rather than a `Result`.** "Nobody was reachable"
    ///    is not an error, and typing it as one invites a caller to answer it
    ///    with a fallback.
    pub async fn send_to_profile(
        &self,
        profile_id: &str,
        notification: Notification,
    ) -> ProfileDeliveryReport {
        let Some(attribution) = self.attribution.as_ref() else {
            return ProfileDeliveryReport {
                plan: TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(
                    "no DeviceAttribution wired into the notification sender".to_string(),
                )),
                queued: Vec::new(),
                failed: Vec::new(),
            };
        };
        let plan = TargetedDelivery::plan(
            profile_id,
            attribution.devices_for_profile(profile_id).await,
        );

        let mut queued = Vec::new();
        let mut failed = Vec::new();
        for device_id in plan.devices() {
            let mut per_device = notification.clone();
            per_device.id = per_device_notification_id(&notification.id, device_id);
            per_device.target = device_id.clone();
            match self.send(per_device).await {
                Ok(()) => queued.push(device_id.clone()),
                Err(e) => failed.push((device_id.clone(), e.to_string())),
            }
        }

        if let TargetedDelivery::Undeliverable(reason) = &plan {
            tracing::info!(
                profile = %profile_id,
                reason = reason.as_str(),
                "targeted notification reached nobody; not broadcasting"
            );
        }
        ProfileDeliveryReport {
            plan,
            queued,
            failed,
        }
    }
}

/// One stored row per device, derived from the logical notification id.
///
/// Deterministic so a re-send of the same logical notification replaces its own
/// row per device rather than accumulating, which is what `INSERT OR REPLACE`
/// already gives us for a single device.
fn per_device_notification_id(notification_id: &str, device_id: &str) -> String {
    format!("{notification_id}:{device_id}")
}

#[async_trait]
impl NotificationSender for BroadcastNotificationSender {
    async fn send(&self, notification: Notification) -> Result<()> {
        let notification = clamp(notification);
        let targeted = notification.target != BROADCAST_TARGET;
        if targeted {
            // Persist for offline delivery before fanning out live.
            self.queue.enqueue(notification.clone()).await?;
            // Background push is best-effort — never fail the send on it.
            if let Some(relay) = &self.relay {
                if let Err(e) = relay.relay(&notification).await {
                    tracing::warn!(error = %e, "notification relay failed");
                }
            }
        }
        // No subscribers is normal (no device connected) — ignore the error.
        let _ = self.tx.send(notification);
        Ok(())
    }

    async fn broadcast(&self, notification: Notification) -> Result<()> {
        // Broadcasts are ephemeral (not per-device queued).
        let mut notification = clamp(notification);
        notification.target = BROADCAST_TARGET.to_string();
        let _ = self.tx.send(notification);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::push_token::PushToken;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubQueue {
        enqueued: Mutex<Vec<Notification>>,
    }
    #[async_trait]
    impl NotificationQueueRepository for StubQueue {
        async fn enqueue(&self, n: Notification) -> Result<()> {
            self.enqueued.lock().unwrap().push(n);
            Ok(())
        }
        async fn list_undelivered(&self, _device_id: &str) -> Result<Vec<Notification>> {
            Ok(self.enqueued.lock().unwrap().clone())
        }
        async fn mark_delivered(&self, _ids: &[String]) -> Result<()> {
            Ok(())
        }
    }

    fn notif(target: &str) -> Notification {
        Notification {
            id: "n1".into(),
            target: target.into(),
            category: "info".into(),
            title: "t".into(),
            body: "b".into(),
            timestamp: "2026-06-29T00:00:00Z".into(),
            data: None,
        }
    }

    #[tokio::test]
    async fn targeted_send_enqueues_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        sender.send(notif("dev-1")).await.unwrap();

        assert_eq!(queue.enqueued.lock().unwrap().len(), 1, "targeted enqueued");
        assert_eq!(rx.recv().await.unwrap().target, "dev-1", "fanned out live");
    }

    #[tokio::test]
    async fn oversized_title_and_body_are_truncated_char_safe() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        let mut n = notif("dev-1");
        // Multi-byte chars so a byte-based slice would panic at the boundary.
        n.title = "é".repeat(MAX_TITLE_CHARS + 50);
        n.body = "🦆".repeat(MAX_BODY_CHARS + 50);
        sender.send(n).await.unwrap();

        let got = rx.recv().await.unwrap();
        assert_eq!(got.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(got.body.chars().count(), MAX_BODY_CHARS);
        // The queued copy is clamped too (clamp happens before enqueue).
        let queued = &queue.enqueued.lock().unwrap()[0];
        assert_eq!(queued.body.chars().count(), MAX_BODY_CHARS);
    }

    /// Answers `devices_for_profile` from a script, and records nothing else --
    /// the other three methods panic, so a test that passes because the code
    /// under test asked a different question is impossible.
    struct ScriptedAttribution {
        devices: std::sync::Mutex<Option<Result<Vec<String>>>>,
    }

    impl ScriptedAttribution {
        fn returning(ids: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                devices: Mutex::new(Some(Ok(ids.iter().map(|s| s.to_string()).collect()))),
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                devices: Mutex::new(Some(Err(anyhow::anyhow!("database is locked")))),
            })
        }
    }

    #[async_trait]
    impl DeviceAttribution for ScriptedAttribution {
        async fn set_device_profile(&self, _d: &str, _p: Option<&str>) -> Result<()> {
            unreachable!("delivery never writes an attribution")
        }
        async fn device_profile(&self, _d: &str) -> Result<Option<String>> {
            unreachable!("delivery asks who a member's devices are, not whose a device is")
        }
        async fn devices_for_profile(&self, _p: &str) -> Result<Vec<String>> {
            self.devices
                .lock()
                .unwrap()
                .take()
                .expect("devices_for_profile asked twice for one delivery")
        }
        async fn push_tokens_for_profile(&self, _p: &str) -> Result<Vec<PushToken>> {
            unreachable!("the relay resolves a token from the device id it is handed")
        }
    }

    #[derive(Default)]
    struct CountingRelay {
        relayed: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl NotificationRelay for CountingRelay {
        async fn relay(&self, notification: &Notification) -> Result<()> {
            self.relayed
                .lock()
                .unwrap()
                .push(notification.target.clone());
            Ok(())
        }
    }

    /// Everything on the channel right now. `try_recv` rather than `recv` on
    /// purpose: the assertions below are about what did NOT get published, and
    /// an `await` on an empty channel would hang instead of failing.
    fn drain(rx: &mut broadcast::Receiver<Notification>) -> Vec<Notification> {
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            out.push(n);
        }
        out
    }

    /// The positive case, and the vacuity control for the three refusal tests
    /// below: without it they would all pass against a `send_to_profile` whose
    /// body was `return`.
    #[tokio::test]
    async fn a_member_with_two_devices_gets_one_copy_each_and_the_house_gets_none() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let relay = Arc::new(CountingRelay::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), Some(relay.clone()))
            .with_device_attribution(ScriptedAttribution::returning(&["phone-liz", "watch-liz"]));

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert_eq!(report.queued, vec!["phone-liz", "watch-liz"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let published = drain(&mut rx);
        let targets: Vec<&str> = published.iter().map(|n| n.target.as_str()).collect();
        assert_eq!(
            targets,
            vec!["phone-liz", "watch-liz"],
            "each device is addressed by name and the sentinel is never published"
        );
        assert_eq!(
            *relay.relayed.lock().unwrap(),
            vec!["phone-liz".to_string(), "watch-liz".to_string()],
            "background push is attempted per device, not once for the member"
        );

        // Per-device ids, because `notifications.id` is the PRIMARY KEY and
        // `enqueue` is an INSERT OR REPLACE.
        let enqueued = queue.enqueued.lock().unwrap();
        let ids: Vec<&str> = enqueued.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(
            ids[0], ids[1],
            "two devices sharing one notification id means the second row replaces the first, \
             and the phone that was switched off is the one that loses it"
        );
    }

    /// PAI-7 invariant 4, in the shape that costs a privacy failure when it is
    /// wrong. `devices_for_profile` deliberately returns no unattributed device,
    /// so a member who has never paired a phone is unreachable -- and the
    /// tempting repair, "fall back to broadcast so they still see it", is
    /// exactly the disclosure this workstream exists to prevent.
    #[tokio::test]
    async fn a_member_with_no_attributed_device_reaches_nobody_rather_than_everybody() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None)
            .with_device_attribution(ScriptedAttribution::returning(&[]));

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert_eq!(
            report.plan,
            TargetedDelivery::Undeliverable(Undeliverable::NoAttributedDevice)
        );
        assert!(report.reached_nobody());
        assert!(
            queue.enqueued.lock().unwrap().is_empty(),
            "nothing is queued for a member with no device"
        );
        assert!(
            drain(&mut rx).is_empty(),
            "and NOTHING reaches the broadcast channel -- every connected screen in the house \
             subscribes to it, so one message there is the whole household reading a proposal \
             addressed to one person"
        );
    }

    /// A failed attribution read narrows. This is the case a delivery path is
    /// most tempted to widen, because the notification is real and the only
    /// thing missing is the address.
    #[tokio::test]
    async fn a_failed_attribution_read_reaches_nobody() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None)
            .with_device_attribution(ScriptedAttribution::failing());

        let report = sender.send_to_profile("liz", notif("unused")).await;

        match &report.plan {
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(why)) => {
                assert!(why.contains("database is locked"), "{why}")
            }
            other => panic!("a failed read must be AttributionUnavailable, got {other:?}"),
        }
        assert!(queue.enqueued.lock().unwrap().is_empty());
        assert!(
            drain(&mut rx).is_empty(),
            "a read failure publishes nothing"
        );
    }

    /// The state every install is in today: `main.rs` builds this sender and
    /// wires no attribution. An unwired dependency must refuse, because the
    /// alternative -- treating "I have no way to address a member" as "address
    /// everyone" -- is a scope-widening default reached by omission.
    #[tokio::test]
    async fn an_unwired_attribution_reaches_nobody() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert!(matches!(
            report.plan,
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(_))
        ));
        assert!(queue.enqueued.lock().unwrap().is_empty());
        assert!(drain(&mut rx).is_empty());
    }

    /// A device id is caller-supplied at registration, so a device really can be
    /// registered as `"broadcast"`. Attributed to a member, delivering to it
    /// would take the sentinel branch of `send` and publish to every subscriber
    /// -- a household broadcast produced by the targeted path.
    #[tokio::test]
    async fn a_device_registered_as_the_sentinel_is_not_delivered_to() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender =
            BroadcastNotificationSender::new(tx, queue.clone(), None).with_device_attribution(
                ScriptedAttribution::returning(&[BROADCAST_TARGET, "phone-liz"]),
            );

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert_eq!(report.queued, vec!["phone-liz"]);
        let published = drain(&mut rx);
        assert_eq!(published.len(), 1);
        assert_eq!(
            published[0].target, "phone-liz",
            "the member's real device is still reached and the sentinel is not"
        );
    }

    #[tokio::test]
    async fn broadcast_does_not_enqueue() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        sender.broadcast(notif("ignored")).await.unwrap();

        assert!(
            queue.enqueued.lock().unwrap().is_empty(),
            "broadcast not queued"
        );
        assert_eq!(rx.recv().await.unwrap().target, BROADCAST_TARGET);
    }
}
