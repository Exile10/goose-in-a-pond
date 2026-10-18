//! Sole microphone owner: a second concurrent `build_input_stream` fails (ALSA without dmix).
//! An OS thread (`cpal::Stream` is `!Send`); mic off closes the device so the OS light goes out.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::ring::Ring;

/// What the microphone is doing, as far as the rest of the system is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicState {
    /// No device open. The resting state.
    Closed,
    Open,
    /// Refused because the user turned the mic off; not a fault, unlike `Failed`.
    Denied,
    /// The device could not be opened or was lost mid-capture.
    Failed(String),
}

impl MicState {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }
}

/// Commands accepted by the owner thread.
#[derive(Debug)]
pub enum MicCommand {
    /// Open the device and begin filling the ring, unclaimed: only `Close` releases it.
    Open,
    /// Stop capturing and release the device, unconditionally, claims and all.
    Close,
    /// Claim the device for session `generation`; it stays open while any claim is live.
    Claim(u64),
    /// Release session `generation`'s claim; the device closes once no claim is left.
    /// Claims overlap (a reply's barge-in hold, a capture, a detector), so the last one
    /// out closes the device rather than whichever claimed most recently.
    CloseIfGeneration(u64),
    /// Apply the privacy setting. `false` closes an open device immediately.
    SetEnabled(bool),
    /// Drop buffered audio without closing, so stale audio doesn't leak into the next turn.
    Clear,
    /// Stop the thread. Joinable, so a device handoff can know the device is really released.
    Shutdown,
    /// Capture from this device from now on (`None`: the system default). An open capture moves
    /// to it at once, and a lost one is reopened even when the name is unchanged.
    Retarget(Option<String>),
    /// Run a job on the owner thread, between commands, so nothing else opens a device while it
    /// runs. Device probes go through here to keep this thread the only opener.
    Exclusive(ExclusiveJob),
    /// Restart a wanted capture on its current device: one that stalled while still "open".
    Reopen,
}

/// A job for [`MicCommand::Exclusive`].
pub struct ExclusiveJob(pub Box<dyn FnOnce(&OwnerView<'_>) + Send>);

impl std::fmt::Debug for ExclusiveJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExclusiveJob")
    }
}

/// What an exclusive job may know about the owner.
pub struct OwnerView<'a> {
    /// The device open right now, by name: measure it from the ring rather than open it twice.
    pub open_device: Option<String>,
    pub shared: &'a Arc<MicShared>,
}

/// Shared state a subscriber can read without going through the channel.
#[derive(Debug)]
pub struct MicShared {
    pub ring: Mutex<Ring>,
    state: Mutex<MicState>,
    /// Bumped on every state change and ring write, for cheap change detection.
    tick: Mutex<u64>,
    enabled: AtomicBool,
    /// Which capture owns the device, so a stale `Close` can't land on whoever opened next.
    generation: AtomicU64,
}

impl MicShared {
    fn new(ring: Ring, enabled: bool) -> Self {
        Self {
            ring: Mutex::new(ring),
            state: Mutex::new(MicState::Closed),
            tick: Mutex::new(0),
            enabled: AtomicBool::new(enabled),
            generation: AtomicU64::new(0),
        }
    }

    pub fn state(&self) -> MicState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The most recent claim token handed out.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub(crate) fn set_state(&self, s: MicState) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if *g != s {
            tracing::debug!(from = ?*g, to = ?s, "microphone state");
            *g = s;
        }
        drop(g);
        self.bump();
    }

    pub(crate) fn bump(&self) {
        *self.tick.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    }

    /// Monotonic counter for change detection.
    pub fn tick(&self) -> u64 {
        *self.tick.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub(crate) fn set_enabled(&self, v: bool) {
        self.enabled.store(v, Ordering::SeqCst);
    }

    /// The most recent `n` samples of 16 kHz mono f32.
    pub fn recent(&self, n: usize) -> Vec<f32> {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recent(n)
    }

    /// RMS of the most recent `n` samples, for barge-in and the energy gate.
    pub fn recent_rms(&self, n: usize) -> f32 {
        pond_voice::dsp::rms(&self.recent(n))
    }

    /// Total samples ever captured. The basis for [`MicReader`]'s cursor.
    pub fn written(&self) -> u64 {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .written()
    }
}

/// A cursor into the shared ring; counts samples lost rather than returning a short clip.
pub struct MicReader {
    shared: Arc<MicShared>,
    cursor: u64,
    dropped: u64,
}

impl MicReader {
    /// Begin reading from now, so a new capture never opens with the previous turn's tail.
    pub fn new(shared: Arc<MicShared>) -> Self {
        Self::starting_back(shared, 0)
    }

    /// Begin `back` samples before now, for speech already under way; never past what is held.
    pub fn starting_back(shared: Arc<MicShared>, back: usize) -> Self {
        let cursor = {
            let ring = shared.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.written() - back.min(ring.len()) as u64
        };
        Self {
            shared,
            cursor,
            dropped: 0,
        }
    }

    /// Everything captured since the last call, oldest first.
    pub fn drain(&mut self) -> Vec<f32> {
        let ring = self.shared.ring.lock().unwrap_or_else(|e| e.into_inner());
        let written = ring.written();
        let pending = written.saturating_sub(self.cursor);
        // A reader more than a window behind has lost that audio for good.
        let take = pending.min(ring.len() as u64);
        let out = ring.recent(take as usize);
        drop(ring);
        self.dropped += pending - take;
        self.cursor = written;
        out
    }

    /// Samples lost to falling behind or a clear; non-zero means the capture has a hole.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Cloneable handle to the owner; dropping the last clone ends the thread and frees the device.
#[derive(Clone)]
pub struct MicHandle {
    tx: Sender<MicCommand>,
    shared: Arc<MicShared>,
}

impl MicHandle {
    pub fn open(&self) {
        let _ = self.tx.send(MicCommand::Open);
    }
    pub fn close(&self) {
        let _ = self.tx.send(MicCommand::Close);
    }

    /// Claim the device; pass the returned token to [`close_session`](Self::close_session).
    pub fn open_session(&self) -> u64 {
        let generation = self.shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx.send(MicCommand::Claim(generation));
        generation
    }

    /// Release `generation`'s claim; the device closes when no other claim is live.
    pub fn close_session(&self, generation: u64) {
        let _ = self.tx.send(MicCommand::CloseIfGeneration(generation));
    }
    pub fn set_enabled(&self, v: bool) {
        let _ = self.tx.send(MicCommand::SetEnabled(v));
    }
    pub fn clear(&self) {
        let _ = self.tx.send(MicCommand::Clear);
    }
    pub fn shutdown(&self) {
        let _ = self.tx.send(MicCommand::Shutdown);
    }
    pub fn retarget(&self, device: Option<String>) {
        let _ = self.tx.send(MicCommand::Retarget(device));
    }
    pub fn reopen(&self) {
        let _ = self.tx.send(MicCommand::Reopen);
    }

    /// Run `job` on the owner thread and wait for its answer; `None` if the owner is gone or the
    /// job outlived `timeout`.
    pub fn run_exclusive<R: Send + 'static>(
        &self,
        timeout: std::time::Duration,
        job: impl FnOnce(&OwnerView<'_>) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = ExclusiveJob(Box::new(move |view| {
            let _ = tx.send(job(view));
        }));
        self.tx.send(MicCommand::Exclusive(job)).ok()?;
        rx.recv_timeout(timeout).ok()
    }

    /// Read access to the buffer and state.
    pub fn shared(&self) -> &Arc<MicShared> {
        &self.shared
    }

    pub fn state(&self) -> MicState {
        self.shared.state()
    }

    /// Block until `pred` holds or the timeout elapses; "asked to stop" is not "device free".
    pub fn wait_for(&self, pred: impl Fn(&MicState) -> bool, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if pred(&self.shared.state()) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// How the owner obtains audio; abstracted so the owner is testable without a sound card.
pub trait CaptureDevice: Send {
    /// Begin delivering normalised 16 kHz mono f32 into `shared`.
    fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String>;
    /// Release the device. Idempotent.
    fn stop(&mut self);
    /// Open this device on the next `start` (`None`: the system default). True if it changed.
    fn retarget(&mut self, _device: Option<String>) -> bool {
        false
    }
    /// The device the last successful `start` opened, by name.
    fn opened(&self) -> Option<String> {
        None
    }
}

/// Run the owner loop. Returns when `Shutdown` is received or the channel dies.
pub fn run(mut device: Box<dyn CaptureDevice>, shared: Arc<MicShared>, rx: Receiver<MicCommand>) {
    let mut want_open = false;
    let mut claims: BTreeSet<u64> = BTreeSet::new();
    let mut unclaimed_open = false;

    // A helper so every path that opens the device applies the privacy gate.
    let apply = |device: &mut Box<dyn CaptureDevice>, want_open: bool| {
        if !want_open {
            device.stop();
            shared.set_state(MicState::Closed);
            return;
        }
        if !shared.enabled() {
            // Closed, not filtered. See the module docs.
            device.stop();
            shared.set_state(MicState::Denied);
            return;
        }
        // Audio from before the device stopped isn't contiguous with what follows; a window
        // over both would hand the next capture the last one's tail.
        shared
            .ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        match device.start(shared.clone()) {
            Ok(()) => shared.set_state(MicState::Open),
            Err(e) => {
                tracing::warn!(error = %e, "microphone could not be opened");
                shared.set_state(MicState::Failed(e));
            }
        }
    };

    while let Ok(cmd) = rx.recv() {
        match cmd {
            MicCommand::Open | MicCommand::Claim(_) => {
                match cmd {
                    MicCommand::Claim(generation) => {
                        claims.insert(generation);
                    }
                    _ => unclaimed_open = true,
                }
                // Idempotent: `CpalCapture::start` stops first, cutting off a reader mid-read.
                if want_open && shared.state().is_open() {
                    continue;
                }
                want_open = true;
                apply(&mut device, want_open);
            }
            MicCommand::Close => {
                claims.clear();
                unclaimed_open = false;
                want_open = false;
                apply(&mut device, want_open);
            }
            MicCommand::CloseIfGeneration(generation) => {
                if !claims.remove(&generation) {
                    tracing::debug!(
                        generation,
                        "mic: ignoring a release of a claim that is not live"
                    );
                    continue;
                }
                if unclaimed_open || !claims.is_empty() {
                    tracing::debug!(
                        released = generation,
                        live = claims.len(),
                        "mic: still claimed, staying open"
                    );
                    continue;
                }
                want_open = false;
                apply(&mut device, want_open);
            }
            MicCommand::SetEnabled(v) => {
                let changed = shared.enabled() != v;
                shared.set_enabled(v);
                if changed {
                    tracing::info!(
                        mic_enabled = v,
                        "microphone permission changed; device will {}",
                        if v { "reopen if wanted" } else { "close now" }
                    );
                    // Revoking takes effect immediately, not at the next open.
                    apply(&mut device, want_open);
                }
            }
            MicCommand::Clear => {
                shared
                    .ring
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                shared.bump();
            }
            MicCommand::Shutdown => {
                device.stop();
                shared.set_state(MicState::Closed);
                return;
            }
            MicCommand::Retarget(name) => {
                tracing::info!(device = ?name, "microphone target");
                let moved = device.retarget(name);
                let lost = matches!(shared.state(), MicState::Failed(_));
                if want_open && (moved || lost) {
                    apply(&mut device, want_open);
                }
            }
            MicCommand::Reopen => {
                if want_open {
                    apply(&mut device, want_open);
                }
            }
            MicCommand::Exclusive(ExclusiveJob(job)) => {
                let view = OwnerView {
                    open_device: shared.state().is_open().then(|| device.opened()).flatten(),
                    shared: &shared,
                };
                job(&view);
            }
        }
    }
    // Channel closed: release the device rather than holding it forever.
    device.stop();
    shared.set_state(MicState::Closed);
}

/// Spawn an owner thread; `window_ms` must cover the longest window any subscriber needs.
pub fn spawn(
    device: Box<dyn CaptureDevice>,
    sample_rate: u32,
    window_ms: u64,
    enabled: bool,
) -> (MicHandle, std::thread::JoinHandle<()>) {
    let shared = Arc::new(MicShared::new(
        Ring::with_window(sample_rate, window_ms),
        enabled,
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    let s = shared.clone();
    let join = std::thread::Builder::new()
        .name("pond-mic-owner".into())
        .spawn(move || run(device, s, rx))
        .expect("spawn mic owner thread");
    (MicHandle { tx, shared }, join)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Records what the owner asked of it, and can be told to fail.
    #[derive(Default)]
    struct FakeDevice {
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        open: Arc<AtomicBool>,
        fail_with: Option<String>,
        target: Option<String>,
        /// Every device `start` opened, in order.
        opened_on: Arc<Mutex<Vec<Option<String>>>>,
    }

    impl CaptureDevice for FakeDevice {
        fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = &self.fail_with {
                return Err(e.clone());
            }
            self.open.store(true, Ordering::SeqCst);
            self.opened_on.lock().unwrap().push(self.target.clone());
            shared.ring.lock().unwrap().push(&[0.1, 0.2, 0.3]);
            Ok(())
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.open.store(false, Ordering::SeqCst);
        }
        fn retarget(&mut self, device: Option<String>) -> bool {
            let changed = self.target != device;
            self.target = device;
            changed
        }
        fn opened(&self) -> Option<String> {
            self.open
                .load(Ordering::SeqCst)
                .then(|| self.target.clone().unwrap_or_else(|| "default".into()))
        }
    }

    struct Harness {
        handle: MicHandle,
        join: Option<std::thread::JoinHandle<()>>,
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
        open: Arc<AtomicBool>,
    }

    impl Harness {
        fn new(enabled: bool, fail_with: Option<&str>) -> Self {
            let starts = Arc::new(AtomicUsize::new(0));
            let stops = Arc::new(AtomicUsize::new(0));
            let open = Arc::new(AtomicBool::new(false));
            let dev = FakeDevice {
                starts: starts.clone(),
                stops: stops.clone(),
                open: open.clone(),
                fail_with: fail_with.map(str::to_string),
                ..Default::default()
            };
            let (handle, join) = spawn(Box::new(dev), 16_000, 2_000, enabled);
            Self {
                handle,
                join: Some(join),
                starts,
                stops,
                open,
            }
        }
        /// Wait for a predicate, failing the test rather than hanging.
        fn settle(&self, pred: impl Fn(&MicState) -> bool) -> MicState {
            assert!(
                self.handle
                    .wait_for(&pred, std::time::Duration::from_secs(2)),
                "state never settled; stuck at {:?}",
                self.handle.state()
            );
            self.handle.state()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.handle.shutdown();
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    #[test]
    fn opening_and_closing_drives_the_device() {
        let h = Harness::new(true, None);
        assert_eq!(h.handle.state(), MicState::Closed);

        h.handle.open();
        assert_eq!(h.settle(|s| s.is_open()), MicState::Open);
        assert!(h.open.load(Ordering::SeqCst));

        h.handle.close();
        h.settle(|s| *s == MicState::Closed);
        assert!(!h.open.load(Ordering::SeqCst), "device must be released");
    }

    /// Refuse to open, not capture-and-discard: an open device keeps the OS indicator lit.
    #[test]
    fn a_disabled_mic_is_never_opened() {
        let h = Harness::new(false, None);
        h.handle.open();
        assert_eq!(h.settle(|s| *s == MicState::Denied), MicState::Denied);
        assert_eq!(
            h.starts.load(Ordering::SeqCst),
            0,
            "the device must not be touched at all"
        );
        assert!(!h.open.load(Ordering::SeqCst));
    }

    #[test]
    fn revoking_permission_closes_an_already_open_device() {
        let h = Harness::new(true, None);
        h.handle.open();
        h.settle(|s| s.is_open());

        h.handle.set_enabled(false);
        assert_eq!(h.settle(|s| *s == MicState::Denied), MicState::Denied);
        assert!(!h.open.load(Ordering::SeqCst), "must close, not just gate");
    }

    #[test]
    fn re_granting_permission_reopens_if_capture_was_wanted() {
        let h = Harness::new(true, None);
        h.handle.open();
        h.settle(|s| s.is_open());
        h.handle.set_enabled(false);
        h.settle(|s| *s == MicState::Denied);

        h.handle.set_enabled(true);
        assert_eq!(h.settle(|s| s.is_open()), MicState::Open);
    }

    #[test]
    fn re_granting_permission_does_not_open_a_device_nobody_asked_for() {
        let h = Harness::new(false, None);
        h.handle.set_enabled(true);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(h.handle.state(), MicState::Closed);
        assert_eq!(h.starts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_device_failure_is_reported_not_silently_swallowed() {
        let h = Harness::new(true, Some("device busy"));
        h.handle.open();
        let s = h.settle(|s| matches!(s, MicState::Failed(_)));
        match s {
            MicState::Failed(e) => assert!(e.contains("busy"), "{e}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn subscribers_read_normalised_audio_from_the_shared_ring() {
        let h = Harness::new(true, None);
        h.handle.open();
        h.settle(|s| s.is_open());
        assert_eq!(h.handle.shared().recent(3), vec![0.1, 0.2, 0.3]);
        assert!(h.handle.shared().recent_rms(3) > 0.0);
    }

    /// Stale audio must not leak across a turn boundary.
    #[test]
    fn clear_drops_buffered_audio_without_closing() {
        let h = Harness::new(true, None);
        h.handle.open();
        h.settle(|s| s.is_open());
        assert!(!h.handle.shared().recent(10).is_empty());

        let before = h.handle.shared().tick();
        h.handle.clear();
        for _ in 0..100 {
            if h.handle.shared().tick() > before {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(h.handle.shared().recent(10).is_empty());
        assert!(
            h.handle.state().is_open(),
            "clear must not close the device"
        );
    }

    /// `recent` readers (the wake-word window, its lookback) would otherwise splice the last
    /// session's audio onto this one's.
    #[test]
    fn reopening_the_device_starts_from_an_empty_ring() {
        let h = Harness::new(true, None);
        h.handle.open();
        h.settle(|s| s.is_open());
        h.handle.close();
        h.settle(|s| *s == MicState::Closed);
        assert_eq!(h.handle.shared().recent(10).len(), 3, "kept while closed");

        let mut reader = MicReader::new(h.handle.shared().clone());
        h.handle.open();
        h.settle(|s| s.is_open());
        assert_eq!(
            h.handle.shared().recent(10),
            vec![0.1, 0.2, 0.3],
            "only this session's audio"
        );
        assert_eq!(reader.drain(), vec![0.1, 0.2, 0.3]);
        assert_eq!(
            reader.dropped(),
            0,
            "a clear before the reader's audio loses none of it"
        );
    }

    #[test]
    fn shutdown_releases_the_device_and_joins() {
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let open = Arc::new(AtomicBool::new(false));
        let dev = FakeDevice {
            starts: starts.clone(),
            stops: stops.clone(),
            open: open.clone(),
            fail_with: None,
            ..Default::default()
        };
        let (handle, join) = spawn(Box::new(dev), 16_000, 1_000, true);
        handle.open();
        assert!(handle.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));

        handle.shutdown();
        join.join().expect("owner thread must join, not detach");
        assert!(!open.load(Ordering::SeqCst), "device released on shutdown");
        assert_eq!(handle.state(), MicState::Closed);
    }

    #[test]
    fn a_dropped_channel_releases_the_device() {
        let open = Arc::new(AtomicBool::new(false));
        let dev = FakeDevice {
            open: open.clone(),
            ..Default::default()
        };
        let (handle, join) = spawn(Box::new(dev), 16_000, 1_000, true);
        handle.open();
        assert!(handle.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));

        drop(handle);
        join.join().expect("thread exits when the channel closes");
        assert!(!open.load(Ordering::SeqCst));
    }

    #[test]
    fn opening_twice_does_not_stack_devices() {
        let h = Harness::new(true, None);
        h.handle.open();
        h.settle(|s| s.is_open());
        h.handle.open();
        h.settle(|s| s.is_open());
        // A Close must still fully release, leaving no second stream live.
        h.handle.close();
        h.settle(|s| *s == MicState::Closed);
        assert!(!h.open.load(Ordering::SeqCst));
        assert!(h.stops.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn wait_for_times_out_rather_than_hanging() {
        let h = Harness::new(true, None);
        let t0 = std::time::Instant::now();
        assert!(
            !h.handle
                .wait_for(|s| s.is_open(), std::time::Duration::from_millis(80)),
            "nothing opened it, so this must time out"
        );
        assert!(t0.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_stale_owner_cannot_close_a_device_somebody_else_claimed() {
        let h = Harness::new(true, None);

        let detector = h.handle.open_session();
        h.settle(|s| matches!(s, MicState::Open));

        let capture = h.handle.open_session();
        assert_ne!(detector, capture, "each claim needs its own generation");
        h.settle(|s| matches!(s, MicState::Open));

        h.handle.close_session(detector);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            matches!(h.handle.state(), MicState::Open),
            "a cancelled capture closed the device out from under its successor"
        );

        // Vacuity control: the current owner's release still works.
        h.handle.close_session(capture);
        h.settle(|s| matches!(s, MicState::Closed));
    }

    #[test]
    fn releasing_a_newer_claim_leaves_an_older_live_claim_open() {
        // A capture is recording when a reply's barge-in hold claims the mic on top of it;
        // the hold is released first. The capture must keep hearing.
        let h = Harness::new(true, None);
        let capture = h.handle.open_session();
        h.settle(|s| matches!(s, MicState::Open));
        let hold = h.handle.open_session();

        h.handle.close_session(hold);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            matches!(h.handle.state(), MicState::Open),
            "releasing the newer hold closed the device under a live capture"
        );

        // A second release of the same claim changes nothing either.
        h.handle.close_session(hold);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(matches!(h.handle.state(), MicState::Open));

        h.handle.close_session(capture);
        h.settle(|s| matches!(s, MicState::Closed));
    }

    /// Every device the fake opened, in order.
    type Opened = Arc<Mutex<Vec<Option<String>>>>;

    fn spawn_fake() -> (MicHandle, std::thread::JoinHandle<()>, Opened) {
        let opened_on = Arc::new(Mutex::new(Vec::new()));
        let dev = FakeDevice {
            opened_on: opened_on.clone(),
            ..Default::default()
        };
        let (handle, join) = spawn(Box::new(dev), 16_000, 1_000, true);
        (handle, join, opened_on)
    }

    #[test]
    fn retargeting_an_open_capture_moves_it_to_the_new_device() {
        let (h, join, opened_on) = spawn_fake();
        let claim = h.open_session();
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        h.retarget(Some("USB mic".into()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while opened_on.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            *opened_on.lock().unwrap(),
            [None, Some("USB mic".to_string())]
        );
        assert!(h.state().is_open());
        h.close_session(claim);
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn retargeting_a_closed_capture_opens_nothing_until_it_is_claimed() {
        let (h, join, opened_on) = spawn_fake();
        h.retarget(Some("USB mic".into()));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            opened_on.lock().unwrap().is_empty(),
            "the privacy of a closed mic"
        );
        h.open();
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        assert_eq!(*opened_on.lock().unwrap(), [Some("USB mic".to_string())]);
        h.shutdown();
        join.join().unwrap();
    }

    /// A device lost mid-capture reports `Failed`; pointing the owner at a working one recovers.
    #[test]
    fn a_lost_device_is_reopened_by_a_retarget_even_to_the_same_name() {
        let (h, join, opened_on) = spawn_fake();
        h.open();
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        h.shared()
            .set_state(MicState::Failed("input device lost".into()));
        h.retarget(None);
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        assert_eq!(opened_on.lock().unwrap().len(), 2);
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn reopen_restarts_a_wanted_capture_and_leaves_a_closed_one_closed() {
        let (h, join, opened_on) = spawn_fake();
        h.reopen();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            opened_on.lock().unwrap().is_empty(),
            "nobody wanted the mic"
        );
        h.open();
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        h.reopen();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while opened_on.lock().unwrap().len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(opened_on.lock().unwrap().len(), 2);
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn an_exclusive_job_sees_the_open_device_and_runs_between_commands() {
        let (h, join, _) = spawn_fake();
        assert_eq!(
            h.run_exclusive(std::time::Duration::from_secs(2), |v| v.open_device.clone()),
            Some(None),
            "nothing is open yet"
        );
        h.retarget(Some("USB mic".into()));
        h.open();
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        let seen = h.run_exclusive(std::time::Duration::from_secs(2), |v| {
            (v.open_device.clone(), v.shared.recent(3))
        });
        assert_eq!(
            seen,
            Some((Some("USB mic".to_string()), vec![0.1, 0.2, 0.3]))
        );
        h.shutdown();
        join.join().unwrap();
    }

    /// A claim sent while a probe runs waits for it: the owner never opens beside a probe.
    #[test]
    fn a_claim_during_an_exclusive_job_waits_for_it_to_finish() {
        let (h, join, opened_on) = spawn_fake();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let prober = h.clone();
        let job = std::thread::spawn(move || {
            prober.run_exclusive(std::time::Duration::from_secs(5), move |v| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                v.open_device.clone()
            })
        });
        started_rx.recv().unwrap();
        let claim = h.open_session();
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(
            opened_on.lock().unwrap().is_empty(),
            "the owner opened the device while a probe held it"
        );
        release_tx.send(()).unwrap();
        assert_eq!(job.join().unwrap(), Some(None));
        assert!(h.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        h.close_session(claim);
        h.shutdown();
        join.join().unwrap();
    }

    #[test]
    fn an_exclusive_job_on_a_dead_owner_answers_none_instead_of_hanging() {
        let (h, join, _) = spawn_fake();
        h.shutdown();
        join.join().unwrap();
        assert_eq!(
            h.run_exclusive(std::time::Duration::from_millis(200), |_| 1),
            None
        );
    }
}
