//! The single microphone owner.
//!
//! Before this, three subsystems each called `default_input_device()`
//! independently — the wake-word detector, the capture path, and piper's
//! barge-in listener — and `run_loop` could have all three live at once, since
//! it starts the detector *while* TTS and the barge-in listener are running.
//! Three concurrent streams on one device is undefined at best; under ALSA
//! without dmix the second `build_input_stream` simply fails, which is the
//! "wake word works once, then stops" shape.
//!
//! There is now exactly one owner. Everything else subscribes.
//!
//! ## Why a thread and not a tokio task
//!
//! `cpal::Stream` is `!Send` on CoreAudio — the crate says so itself, and
//! `pond-adapters-piper` already carries the workaround (`AudioKeeper`, a
//! named thread that owns a `rodio::OutputStream` and hands out only the
//! `Send` handle). The same constraint applies to input, so the owner is an OS
//! thread with an mpsc command channel. An async actor would not compile
//! without the same `unsafe impl Send` dance, and would buy nothing.
//!
//! ## Privacy
//!
//! `mic_enabled = false` **closes the device**. It does not capture and
//! discard: that would leave the OS microphone indicator lit, and a user who
//! sees that indicator is right not to believe the setting.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::ring::Ring;

/// What the microphone is doing, as far as the rest of the system is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicState {
    /// No device open. The resting state.
    Closed,
    /// Capturing.
    Open,
    /// Refused because the user turned the microphone off. Distinct from
    /// `Failed` so the UI can say "mic off by your setting" rather than
    /// reporting a fault.
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
    /// Open the device and begin filling the ring.
    Open,
    /// Stop capturing and release the device, unconditionally.
    Close,
    /// Stop capturing, but only if `generation` is still the current one.
    ///
    /// The mic has one device and many users, several of which run on detached
    /// threads: a cancelled wake-word detector, for instance, releases the mic
    /// from a blocking thread that the orchestrator has already stopped waiting
    /// on, and by then the follow-up capture has usually opened the same
    /// handle. An unconditional `Close` from that thread closes the device out
    /// from under the capture, which presents as a conversation that hears
    /// nothing after the wake word — the failure that is hardest to attribute,
    /// because the component that caused it is already gone.
    CloseIfGeneration(u64),
    /// Apply the privacy setting. `false` closes an open device immediately.
    SetEnabled(bool),
    /// Drop buffered audio without closing — used at turn boundaries so stale
    /// audio does not leak into the next capture.
    Clear,
    /// Stop the thread. The owner is joinable, unlike `AudioKeeper`, because a
    /// device handoff needs to know the device is actually released.
    Shutdown,
}

/// Shared state a subscriber can read without going through the channel.
#[derive(Debug)]
pub struct MicShared {
    pub ring: Mutex<Ring>,
    state: Mutex<MicState>,
    /// Bumped on every state change and every ring write, so a subscriber can
    /// cheaply tell whether anything happened since it last looked.
    tick: Mutex<u64>,
    enabled: AtomicBool,
    /// Which capture "owns" the device right now.
    ///
    /// The mic is single-owner but the handle is shared, and its users run on
    /// detached threads that can outlive their own cancellation. Without a
    /// generation, a `Close` sent by a component that has already given up
    /// lands on whoever opened next — see `close_session`.
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

    /// The generation of the capture that currently owns the device.
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

    /// RMS of the most recent `n` samples — the cheap primitive barge-in and
    /// the energy gate both need, without copying the whole window.
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

/// A cursor into the shared ring.
///
/// `recent(n)` answers "the last n samples", which suits an energy gate. It
/// cannot answer "everything said since I started listening" — the question
/// both capture paths actually ask, for an utterance that may run to 30 s
/// against a ring sized for a fraction of that. `n` is not knowable up front,
/// and guessing high silently returns audio from before the turn began.
///
/// A reader tracks its own position instead, and counts what it lost rather
/// than quietly returning a shorter clip that still sounds plausible.
pub struct MicReader {
    shared: Arc<MicShared>,
    cursor: u64,
    dropped: u64,
}

impl MicReader {
    /// Begin reading from *now*. Audio already buffered is not delivered, so a
    /// new capture never opens with the tail of the previous turn.
    pub fn new(shared: Arc<MicShared>) -> Self {
        let cursor = shared.written();
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
        // Cannot return more than the ring still holds: if this reader fell
        // more than a window behind, that audio is genuinely gone.
        let take = pending.min(ring.len() as u64);
        let out = ring.recent(take as usize);
        drop(ring);
        self.dropped += pending - take;
        self.cursor = written;
        out
    }

    /// Samples lost to falling behind, or to the ring being cleared underneath.
    /// Non-zero means the capture has a hole and the caller should say so.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Handle to the owner. Cloneable; dropping every clone does not stop the
/// thread — call [`MicHandle::shutdown`] for that.
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

    /// Claim the device and get a token identifying this capture.
    ///
    /// Pair with [`close_session`](Self::close_session) so a component that is
    /// cancelled cannot release a device somebody else has since claimed.
    pub fn open_session(&self) -> u64 {
        let generation = self.shared.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.tx.send(MicCommand::Open);
        generation
    }

    /// Release the device only if `generation` still owns it.
    ///
    /// A no-op when somebody else has claimed it since, which is exactly the
    /// case an unconditional `close()` gets wrong.
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

    /// Read access to the buffer and state.
    pub fn shared(&self) -> &Arc<MicShared> {
        &self.shared
    }

    pub fn state(&self) -> MicState {
        self.shared.state()
    }

    /// Block until the state satisfies `pred`, or the timeout elapses.
    ///
    /// Exists for the device handoff: `voice_cmd.rs` already had to bolt a
    /// 1-second `wait_for_wake_thread_exit` onto the old design because
    /// "asked it to stop" is not "the device is free".
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

/// How the owner obtains audio. Abstracted so the command protocol, the privacy
/// gate and the state machine are testable without a sound card — the previous
/// capture code had zero tests for exactly this reason.
pub trait CaptureDevice: Send {
    /// Begin delivering normalised 16 kHz mono f32 into `shared`.
    fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String>;
    /// Release the device. Idempotent.
    fn stop(&mut self);
}

/// Run the owner loop. Returns when `Shutdown` is received or the channel dies.
pub fn run(mut device: Box<dyn CaptureDevice>, shared: Arc<MicShared>, rx: Receiver<MicCommand>) {
    let mut want_open = false;

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
            MicCommand::Open => {
                // Idempotent. The owner is shared, so a second subscriber
                // asking for a device that is already streaming must not
                // punch a hole in the audio the first one is mid-read of —
                // `CpalCapture::start` opens with a `self.stop()`, so a
                // re-apply would drop and rebuild the stream.
                if want_open && shared.state().is_open() {
                    continue;
                }
                want_open = true;
                apply(&mut device, want_open);
            }
            MicCommand::Close => {
                want_open = false;
                apply(&mut device, want_open);
            }
            MicCommand::CloseIfGeneration(generation) => {
                let current = shared.generation();
                if current != generation {
                    tracing::debug!(
                        stale = generation,
                        current,
                        "mic: ignoring a close from a capture that no longer owns the device"
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
        }
    }
    // Channel closed: release the device rather than holding it forever.
    device.stop();
    shared.set_state(MicState::Closed);
}

/// Spawn an owner on its own thread.
///
/// `window_ms` sizes the shared ring — it must cover the longest window any
/// subscriber needs (the wake-word detector's is the largest).
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
    }

    impl CaptureDevice for FakeDevice {
        fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = &self.fail_with {
                return Err(e.clone());
            }
            self.open.store(true, Ordering::SeqCst);
            shared.ring.lock().unwrap().push(&[0.1, 0.2, 0.3]);
            Ok(())
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.open.store(false, Ordering::SeqCst);
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

    /// The privacy contract: refuse to OPEN, not capture-and-discard. A device
    /// that is open leaves the OS indicator lit.
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

    /// Revoking permission must take effect now, not at the next open — a
    /// privacy control you have to restart to apply is not one.
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

    /// Re-granting while nothing wanted capture must NOT open the device.
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
        };
        let (handle, join) = spawn(Box::new(dev), 16_000, 1_000, true);
        handle.open();
        assert!(handle.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));

        handle.shutdown();
        join.join().expect("owner thread must join, not detach");
        assert!(!open.load(Ordering::SeqCst), "device released on shutdown");
        assert_eq!(handle.state(), MicState::Closed);
    }

    /// Dropping every handle must not strand the device open.
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
        // Each Open re-applies; the invariant that matters is that a Close
        // still fully releases rather than leaving a second stream live.
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

    /// A capture that has lost the device cannot release it.
    ///
    /// The mic has one device and several users, and some of them run on
    /// detached threads that outlive their own cancellation: the wake-word
    /// detector releases the mic from a blocking thread the orchestrator has
    /// already stopped waiting on, by which time the follow-up conversational
    /// capture has usually opened the same handle. An unconditional `close()`
    /// there shuts the device under the capture, and the symptom -- a
    /// conversation that hears nothing after the wake word -- points at a
    /// component that has already exited.
    #[test]
    fn a_stale_owner_cannot_close_a_device_somebody_else_claimed() {
        let h = Harness::new(true, None);

        // The detector claims the device...
        let detector = h.handle.open_session();
        h.settle(|s| matches!(s, MicState::Open));

        // ...is cancelled, and the follow-up capture claims it before the
        // detector's thread gets around to releasing.
        let capture = h.handle.open_session();
        assert_ne!(detector, capture, "each claim needs its own generation");
        h.settle(|s| matches!(s, MicState::Open));

        // The detector's late release must do nothing.
        h.handle.close_session(detector);
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            matches!(h.handle.state(), MicState::Open),
            "a cancelled capture closed the device out from under its successor"
        );

        // ...while the current owner's release still works, so the guard is not
        // simply ignoring every close.
        h.handle.close_session(capture);
        h.settle(|s| matches!(s, MicState::Closed));
    }
}
