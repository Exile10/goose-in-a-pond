//! Telling the user when something Matter-related actually happened. Significant
//! occurrences push a [`Notification`], reaching phones over
//! `GET /api/v1/notifications/stream` and the desktop poller. Everything alerting is
//! debounced on the `routes.rs` window; `giap::trace` still records every occurrence.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::user_data::ports::device_commissioning::matter_bridged_endpoint;
use tokio::sync::{Mutex, RwLock};

/// How long an alert of a given kind suppresses the next one of that kind.
///
/// Ten minutes, matching the pairing-failure window in `routes.rs`. A flapping controller
/// reconnects far faster, so the user hears "Matter is down" once, not once per attempt.
const ALERT_WINDOW: Duration = Duration::from_secs(600);

/// How long after asking for a removal the resulting event still counts as ours.
///
/// Generous relative to the event, which follows within seconds: too tight gives a false
/// "your device left the network" alarm about something the user just did.
const REMOVAL_GRACE: Duration = Duration::from_secs(120);

/// Whether an unreachable alert is outstanding, so recovery is only announced to
/// someone who was told about the outage.
#[derive(Default)]
struct State {
    last_pairing_failure: Option<Instant>,
    last_unreachable: Option<Instant>,
    /// Set when an unreachable alert went out; cleared when recovery is
    /// announced. Without it, every ordinary reconnect would report a recovery
    /// from an outage the user never heard about.
    outage_announced: bool,
    /// Set while first-run setup is in progress, so "finished" is only reported
    /// for an install that was actually announced as starting.
    setup_announced: bool,
    /// Removals GIAP asked for, so the event they cause is not reported as a
    /// device leaving on its own.
    expected_removals: HashMap<String, Instant>,
    /// Which device ids an expectation has already answered for.
    ///
    /// Separate from `expected_removals` because one expectation answers for many events
    /// (a hub and every device behind it) while answering for each of them only ONCE.
    satisfied_removals: HashMap<String, Instant>,
}

impl State {
    /// Was this removal one GIAP asked for? Matches the id exactly, or as a bridged
    /// child of an expected hub, so one expectation answers for N child events and a
    /// prefix match is not consumed. Entries age out through the sweep in
    /// `expect_removal`. The trailing dash matters: `matter-9-` must not match `matter-90`.
    fn take_expected_removal(&mut self, device_id: &str) -> bool {
        // Already answered for. A second departure of the same device is news, or the
        // first deliberate removal would silence every genuine one after it.
        if let Some(at) = self.satisfied_removals.get(device_id) {
            if at.elapsed() < REMOVAL_GRACE {
                return false;
            }
        }

        let covered = self.expected_removals.iter().any(|(expected, at)| {
            at.elapsed() < REMOVAL_GRACE
                && (expected == device_id || device_id.starts_with(&format!("{expected}-")))
        });

        if covered {
            self.satisfied_removals
                .insert(device_id.to_string(), Instant::now());
        }
        covered
    }
}

/// Builds and pushes the Matter notifications, holding the debounce state.
///
/// Cloneable and cheap: bridge, supervisor and commissioner each hold one and share the
/// same window, so two paths cannot both alert for the same outage.
#[derive(Clone)]
pub struct MatterNotifier {
    /// `None` until a sender is attached, and on a build without the notification
    /// stack; every method is then a no-op, so callers never branch on it. Settable
    /// rather than fixed at construction because of startup order: the Matter runtime
    /// is built before the notification stack exists, so the sender arrives later.
    sender: Arc<RwLock<Option<Arc<dyn NotificationSender>>>>,
    state: Arc<Mutex<State>>,
}

impl Default for MatterNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl MatterNotifier {
    /// A notifier that sends nothing until [`attach`](Self::attach) is called.
    pub fn new() -> Self {
        Self {
            sender: Arc::new(RwLock::new(None)),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// A notifier that will never send, for tests and for the paths that have no
    /// user to tell (a controller revival, which is already being reported).
    pub fn disabled() -> Self {
        Self::new()
    }

    /// Start sending through `sender`. Callers must do this before the first
    /// `apply`, or a first-run install would finish unannounced.
    pub async fn attach(&self, sender: Arc<dyn NotificationSender>) {
        *self.sender.write().await = Some(sender);
    }

    /// A device joined the fabric. Silent for a device behind a bridge: a hub's dozen
    /// children each register separately, and the hub's own alert covers them. That
    /// alert claims no count, because the children's descriptors have not populated at
    /// the moment the hub registers.
    pub async fn device_paired(&self, device_id: &str, name: &str, device_type: &str) {
        if matter_bridged_endpoint(device_id).is_some() {
            return;
        }
        let body = if device_type == "bridge" {
            format!(
                "\"{name}\" joined this Pond's Matter network. The devices it provides will \
                 appear as it reports them."
            )
        } else {
            format!("\"{name}\" joined this Pond's Matter network as a {device_type}.")
        };
        self.push("info", "Matter device added".to_string(), body)
            .await;
    }

    pub async fn pairing_failed(&self, reason: &str) {
        {
            let mut state = self.state.lock().await;
            if state
                .last_pairing_failure
                .is_some_and(|at| at.elapsed() < ALERT_WINDOW)
            {
                return;
            }
            state.last_pairing_failure = Some(Instant::now());
        }
        self.push(
            "alert",
            "Matter pairing failed".to_string(),
            reason.to_string(),
        )
        .await;
    }

    /// The controller has stopped answering and a restart is being attempted.
    ///
    /// Raised on the first revival attempt, not the first failed reconnect: a controller
    /// restarting normally is back within a couple of attempts.
    pub async fn controller_unreachable(&self, url: &str) {
        {
            let mut state = self.state.lock().await;
            if state
                .last_unreachable
                .is_some_and(|at| at.elapsed() < ALERT_WINDOW)
            {
                return;
            }
            state.last_unreachable = Some(Instant::now());
            state.outage_announced = true;
        }
        self.push(
            "alert",
            "Matter controller is not responding".to_string(),
            format!(
                "This Pond cannot reach its Matter controller at {url}, so Matter devices \
                 cannot be controlled. It is being restarted."
            ),
        )
        .await;
    }

    /// The connection is back. Silent unless an outage was announced, so a
    /// routine reconnect does not produce an all-clear for nothing.
    pub async fn controller_recovered(&self) {
        {
            let mut state = self.state.lock().await;
            if !state.outage_announced {
                return;
            }
            state.outage_announced = false;
        }
        self.push(
            "info",
            "Matter is working again".to_string(),
            "This Pond reconnected to its Matter controller. Matter devices can be controlled \
             again."
                .to_string(),
        )
        .await;
    }

    /// GIAP is about to remove `device_id` from the fabric itself, so the resulting
    /// `device_removed` is not alerted on. The delete path decommissions before it
    /// unregisters, and one expectation covers a hub and every device behind it, since
    /// removing a hub drops its children too. See `take_expected_removal`.
    pub async fn expect_removal(&self, device_id: &str) {
        let mut state = self.state.lock().await;
        // Opportunistic sweep: entries are only ever consumed by the matching
        // event, and one that never arrives would otherwise sit here for the
        // life of the process suppressing a real alert years later.
        state
            .expected_removals
            .retain(|_, at| at.elapsed() < REMOVAL_GRACE);
        state
            .satisfied_removals
            .retain(|_, at| at.elapsed() < REMOVAL_GRACE);
        state
            .expected_removals
            .insert(device_id.to_string(), Instant::now());
    }

    /// A device left the fabric. Silent when GIAP is the one that removed it.
    ///
    /// `name` is what the user calls the device. The raw id must not appear in the alert:
    /// a bridged child reads as `matter-90-7`, which names nothing a person recognises.
    pub async fn device_dropped(&self, device_id: &str, name: &str) {
        {
            let mut state = self.state.lock().await;
            if state.take_expected_removal(device_id) {
                return; // we asked for this
            }
        }
        self.push(
            "alert",
            "A Matter device left the network".to_string(),
            format!(
                "\"{name}\" is no longer on this Pond's Matter network. If it was not \
                 removed deliberately, it may have been factory reset."
            ),
        )
        .await;
    }

    /// First-run setup has started. It legitimately takes minutes, and the UI
    /// otherwise shows nothing but "Starting..." for the whole of it.
    pub async fn setup_started(&self) {
        self.state.lock().await.setup_announced = true;
        self.push(
            "info",
            "Setting up Matter".to_string(),
            "This Pond is installing its Matter controller. This takes a few minutes and only \
             happens once."
                .to_string(),
        )
        .await;
    }

    /// Setup finished. Only reported when the start was, so a Pond whose
    /// controller was already installed says nothing.
    pub async fn setup_finished(&self) {
        {
            let mut state = self.state.lock().await;
            if !state.setup_announced {
                return;
            }
            state.setup_announced = false;
        }
        self.push(
            "info",
            "Matter is ready".to_string(),
            "This Pond's Matter controller is installed and running. Matter devices can now be \
             added from the Devices tab."
                .to_string(),
        )
        .await;
    }

    async fn push(&self, category: &str, title: String, body: String) {
        let Some(sender) = self.sender.read().await.clone() else {
            return;
        };
        let notification = Notification {
            id: uuid::Uuid::new_v4().to_string(),
            target: "broadcast".to_string(),
            category: category.to_string(),
            title,
            body,
            timestamp: chrono::Utc::now().to_rfc3339(),
            data: None,
        };
        if let Err(e) = sender.broadcast(notification).await {
            // A failed notification must not fail the thing it was reporting on.
            tracing::warn!(error = %e, category, "matter: could not push a notification");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;

    #[derive(Default)]
    struct Recorder {
        sent: std::sync::Mutex<Vec<Notification>>,
    }

    impl Recorder {
        fn titles(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|n| n.title.clone())
                .collect()
        }

        /// The body text, which is where a device is named to the user.
        fn bodies(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|n| n.body.clone())
                .collect()
        }
    }

    #[async_trait]
    impl NotificationSender for Recorder {
        async fn send(&self, notification: Notification) -> Result<()> {
            self.sent.lock().unwrap().push(notification);
            Ok(())
        }
        async fn broadcast(&self, notification: Notification) -> Result<()> {
            self.sent.lock().unwrap().push(notification);
            Ok(())
        }
    }

    async fn notifier() -> (MatterNotifier, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let notifier = MatterNotifier::new();
        notifier.attach(recorder.clone()).await;
        (notifier, recorder)
    }

    #[tokio::test]
    async fn a_flapping_controller_alerts_once_not_once_per_attempt() {
        // The supervisor retries for as long as an outage lasts. Without the
        // window, a controller down for an hour would be an hour of alerts.
        let (notifier, recorder) = notifier().await;
        for _ in 0..5 {
            notifier
                .controller_unreachable("ws://127.0.0.1:5580/giap")
                .await;
        }
        assert_eq!(recorder.titles().len(), 1);
    }

    #[tokio::test]
    async fn recovery_is_only_announced_to_someone_who_heard_about_the_outage() {
        let (notifier, recorder) = notifier().await;

        // An ordinary reconnect, with no outage announced: says nothing.
        notifier.controller_recovered().await;
        assert!(recorder.titles().is_empty(), "an all-clear for nothing");

        notifier
            .controller_unreachable("ws://127.0.0.1:5580/giap")
            .await;
        notifier.controller_recovered().await;
        assert_eq!(
            recorder.titles(),
            vec![
                "Matter controller is not responding",
                "Matter is working again"
            ]
        );

        // And the all-clear is not repeated on the next reconnect.
        notifier.controller_recovered().await;
        assert_eq!(recorder.titles().len(), 2);
    }

    #[tokio::test]
    async fn a_retry_burst_produces_one_pairing_alert() {
        let (notifier, recorder) = notifier().await;
        for _ in 0..4 {
            notifier.pairing_failed("nothing was in pairing mode").await;
        }
        assert_eq!(recorder.titles().len(), 1);
    }

    #[tokio::test]
    async fn setup_finished_says_nothing_when_setup_never_started() {
        // The common case by far: every start after the first finds the
        // controller already installed and must be silent.
        let (notifier, recorder) = notifier().await;
        notifier.setup_finished().await;
        assert!(recorder.titles().is_empty());

        notifier.setup_started().await;
        notifier.setup_finished().await;
        assert_eq!(
            recorder.titles(),
            vec!["Setting up Matter", "Matter is ready"]
        );
    }

    #[tokio::test]
    async fn a_removal_giap_asked_for_is_not_reported_as_a_device_leaving() {
        // The delete path decommissions before it removes the registry row, so
        // the event arrives while the device still looks registered. Alerting on
        // it would tell the user their device had vanished, moments after they
        // deliberately removed it.
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-18").await;
        notifier.device_dropped("matter-18", "Porch Light").await;
        assert!(
            recorder.titles().is_empty(),
            "alerted on a deliberate removal"
        );

        // A different device leaving at the same time is still news.
        notifier.device_dropped("matter-4", "Hall Sensor").await;
        assert_eq!(recorder.titles(), vec!["A Matter device left the network"]);
    }

    #[tokio::test]
    async fn the_expectation_is_consumed_not_permanent() {
        // Otherwise the first deliberate removal of a device would silence every
        // later, genuine departure of one that reused the id.
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-18").await;
        notifier.device_dropped("matter-18", "Porch Light").await;
        notifier.device_dropped("matter-18", "Porch Light").await;

        assert_eq!(recorder.titles().len(), 1, "the expectation was permanent");
    }

    #[tokio::test]
    async fn a_hub_arriving_with_a_dozen_devices_is_one_alert() {
        // One thing the user did. A hub arrives with everything it speaks for and
        // each child registers separately, so this fired once per bulb.
        let (notifier, recorder) = notifier().await;

        notifier
            .device_paired("matter-90", "Living Room Hub", "bridge")
            .await;
        for child in ["matter-90-3", "matter-90-4", "matter-90-5"] {
            notifier.device_paired(child, "a bulb", "light").await;
        }

        assert_eq!(recorder.titles(), vec!["Matter device added"]);
        // And it does not claim a count it cannot know: the children's descriptors
        // have not populated when the hub registers, which is why they arrive as
        // separate events seconds later.
        let body = recorder.bodies().join(" ");
        assert!(body.contains("Living Room Hub"), "{body}");
        assert!(body.contains("as it reports them"), "{body}");
    }

    #[tokio::test]
    async fn removing_a_hub_silences_its_children_too() {
        // A hub's children leave the fabric with it, so the controller emits one
        // `device_removed` per child. The adapter never held the child list and
        // registers ONE expectation, which must silence all of them.
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-90").await;
        notifier
            .device_dropped("matter-90", "Living room hub")
            .await;
        for child in ["matter-90-2", "matter-90-3", "matter-90-11"] {
            notifier.device_dropped(child, "a bulb").await;
        }

        assert!(
            recorder.titles().is_empty(),
            "alerted on a hub removal the user asked for: {:?}",
            recorder.titles()
        );
    }

    #[tokio::test]
    async fn one_hubs_removal_does_not_silence_another() {
        // The trailing dash in the prefix test. Without it `matter-9-` also matches
        // `matter-90`, so deleting one hub would silence an unrelated one's children
        // leaving for real.
        let (notifier, recorder) = notifier().await;

        notifier.expect_removal("matter-9").await;
        notifier
            .device_dropped("matter-90-2", "someone else's bulb")
            .await;

        assert_eq!(
            recorder.titles(),
            vec!["A Matter device left the network"],
            "a different hub's child was silenced"
        );
    }

    #[tokio::test]
    async fn a_departure_names_the_device_not_its_id() {
        // The alert interpolated the raw device id, so it read `"matter-18" is no
        // longer on this Pond's Matter network`. A bridged child would have made
        // that `"matter-90-7"` — an id names nothing a person recognises.
        let (notifier, recorder) = notifier().await;

        notifier.device_dropped("matter-18", "Porch Light").await;

        let body = recorder.bodies().join(" ");
        assert!(
            body.contains("Porch Light"),
            "did not name the device: {body}"
        );
        assert!(!body.contains("matter-18"), "leaked the id: {body}");
    }

    #[tokio::test]
    async fn pairing_success_is_not_debounced() {
        // Adding several devices in one sitting is a normal thing to do, and each
        // one is a distinct fact the user wants confirmed.
        let (notifier, recorder) = notifier().await;
        notifier
            .device_paired("matter-2", "Hall light", "light")
            .await;
        notifier
            .device_paired("matter-3", "Porch lock", "lock")
            .await;
        assert_eq!(recorder.titles().len(), 2);
    }

    #[tokio::test]
    async fn a_notifier_without_a_sender_is_inert() {
        // Every notifier starts this way, and stays this way on a build without
        // the notification stack, so no caller may have to branch on it.
        let notifier = MatterNotifier::disabled();
        notifier
            .device_paired("matter-2", "Hall light", "light")
            .await;
        notifier.controller_unreachable("ws://x").await;
    }
}
