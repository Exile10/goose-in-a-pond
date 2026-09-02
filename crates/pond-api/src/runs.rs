//! Agent turns that outlive the connection that asked for them.
//!
//! A turn used to *be* the SSE response body: `chat_stream_inner` opened the
//! agent stream inside `async_stream!` and yielded frames straight out of it.
//! That made the connection the turn's owner, and dropping the connection its
//! cancellation — deliberately so, and documented as such in
//! `pond-adapters-goose`'s `chat_stream`, where a `DropGuard` cancels the token
//! handed to the agent loop.
//!
//! It also meant a reload killed the answer mid-sentence. The user's message is
//! persisted before inference starts and the assistant's only after the token
//! loop drains, so a client that went away between the two left the question
//! stored and the answer nowhere.
//!
//! Here the turn is a task instead, driving a [`RunHandle`]. The handle keeps a
//! sequenced ring of frames it has already produced plus a broadcast channel for
//! the live tail, so every SSE body — the original POST and every reattach — is
//! a *subscriber*: replay what you missed, then follow along. This is the shape
//! `notifications_stream` already uses (replay the undelivered queue, then tail
//! the broadcast), for the same reason.
//!
//! # Two policies, because "nobody is listening" does not always mean "stop"
//!
//! [`RunPolicy::Ephemeral`] reproduces the old contract exactly: when the last
//! subscriber leaves, cancel. [`RunPolicy::Detached`] ignores the subscriber
//! count. The default is `Ephemeral` and that default is load-bearing — see
//! `RunPolicy`'s own documentation for the voice path that depends on it.
//!
//! # What this does NOT survive
//!
//! A restart of *this process*. The registry is in memory, and so is everything
//! a run needs: the agent's state, its cancellation token, its authority lease,
//! its device claim. [`RunSupervisor::epoch`] exists so a client is told that
//! plainly instead of being handed an indistinguishable 404 — see
//! `epoch`'s documentation.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Identifier for one agent turn. A UUID, string-shaped because it is only ever
/// compared, logged, and put in a URL.
pub type RunId = String;

/// How many frames a run keeps for replay.
///
/// Sized against the failure this exists for: a desktop reload is seconds, not
/// minutes. Two thousand frames is a long answer's worth of tokens plus every
/// tool and thinking frame around it, and it bounds what one abandoned run can
/// hold when nobody ever comes back for it.
const MAX_FRAMES: usize = 2048;

/// Byte ceiling for the same ring, because frame COUNT is a poor proxy for
/// memory once a tool result arrives carrying a base64 image.
const MAX_BYTES: usize = 1024 * 1024;

/// Live fan-out depth.
///
/// Deliberately smaller than [`MAX_FRAMES`], and that relationship is the whole
/// reason a lagging subscriber is recoverable: anything the broadcast queue
/// drops is still in the ring, so `RecvError::Lagged` becomes "re-read from
/// where you actually are" rather than a hole in the transcript.
const BROADCAST_CAP: usize = 256;

const _: () = assert!(
    BROADCAST_CAP < MAX_FRAMES,
    "the replay ring must hold strictly more than the broadcast queue can drop, \
     or a lagging subscriber has no way back"
);

/// Where a finished run stops being reattachable.
///
/// Long enough to cover what it is for — a reload is one to three seconds, a
/// full restart with re-authentication perhaps twenty — with an order of
/// magnitude spare, and short enough that a pond does not accumulate finished
/// turns nobody asked for.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(120);

/// How many detached runs may be in flight at once.
pub const DEFAULT_MAX_RUNS: usize = 8;

// ── State ─────────────────────────────────────────────────────────────────────

/// What a run is doing, or what it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    /// Drained normally. Says nothing about whether the answer was any good.
    Finished,
    /// The agent stream errored, or the turn hit its idle timeout.
    Failed,
    /// Somebody asked it to stop, or an `Ephemeral` run was abandoned.
    Cancelled,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, RunState::Running)
    }
}

/// Who may reattach to a run.
///
/// Captured from the ORIGINAL request, so a reattach is checked against whoever
/// actually started the turn rather than whoever happens to be asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOwner {
    /// The bearer token named a paired device.
    Device(String),
    /// The pond could not name a device — the loopback development bypass, or a
    /// token the handshake could not attribute. Reattach then needs only a valid
    /// token, which is the same rung the original request was granted. This
    /// neither widens nor narrows anything; there is simply no finer rule
    /// available for a caller that was never named.
    Unattributed,
}

/// Whether being abandoned ends a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPolicy {
    /// The historical contract: last subscriber out cancels the turn.
    ///
    /// The voice path depends on this and must keep it. `WebVoiceBackend` fires
    /// a *speculative* `/chat/stream` the moment trailing silence begins —
    /// before the pause is confirmed — and aborts it when speech resumes. Under
    /// `Detached` that speculative turn would run to completion and persist a
    /// question-and-answer pair for a half-sentence nobody finished saying.
    Ephemeral,
    /// Nobody listening is not a reason to stop.
    Detached,
}

// ── Frames ────────────────────────────────────────────────────────────────────

/// One SSE frame, exactly as a client receives it.
///
/// `payload` is the `data:` line the existing frame builders already produce and
/// is never re-parsed. The sequence number rides in the SSE `id:` field instead,
/// which every SSE parser already surfaces — so no existing frame shape changes,
/// and a per-token JSON round-trip is not added to the hot path of a device that
/// may be a Jetson.
#[derive(Clone, Debug)]
pub struct RunFrame {
    pub seq: u64,
    pub payload: Arc<str>,
    /// The last frame this run will ever send. A subscriber that sees one is
    /// done and may close.
    pub terminal: bool,
}

/// What a subscriber gets when it attaches or recovers.
#[derive(Debug)]
pub struct Snapshot {
    pub frames: Vec<RunFrame>,
    /// `Some(first_still_held)` when the caller asked for a position the ring
    /// has already evicted. The caller has genuinely lost frames and needs to
    /// reload the session rather than pretend it is caught up.
    pub gap: Option<u64>,
    pub state: RunState,
    pub last_seq: u64,
    /// Whether the terminal frame is among `frames`.
    pub saw_terminal: bool,
}

struct RunBuffer {
    frames: VecDeque<RunFrame>,
    bytes: usize,
    next_seq: u64,
    /// Lowest sequence still held. Rises as the ring evicts, and is what turns
    /// "you asked for 41" into an honest gap rather than a silent skip.
    first_seq: u64,
    state: RunState,
    finished_at: Option<Instant>,
}

// ── The handle ────────────────────────────────────────────────────────────────

/// One agent turn, and everything anyone needs to follow it.
pub struct RunHandle {
    pub run_id: RunId,
    pub session_id: String,
    pub owner: RunOwner,
    pub policy: RunPolicy,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Fired by an explicit cancel, and by the last subscriber leaving an
    /// `Ephemeral` run. The turn task selects on it; dropping the agent stream
    /// afterwards is what fires the adapter's own `DropGuard`.
    pub cancel: CancellationToken,
    tx: broadcast::Sender<RunFrame>,
    attached: AtomicUsize,
    buf: Mutex<RunBuffer>,
}

impl RunHandle {
    pub fn new(session_id: String, owner: RunOwner, policy: RunPolicy) -> Arc<Self> {
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        Arc::new(Self {
            run_id: uuid::Uuid::new_v4().to_string(),
            session_id,
            owner,
            policy,
            started_at: chrono::Utc::now(),
            cancel: CancellationToken::new(),
            tx,
            attached: AtomicUsize::new(0),
            buf: Mutex::new(RunBuffer {
                frames: VecDeque::new(),
                bytes: 0,
                // Sequences are 1-based, so `after_seq = 0` unambiguously means
                // "from the beginning". With 0-based frames it would have meant
                // both that AND "I have seen frame 0", and a reattach that was
                // already current would replay the first frame forever.
                next_seq: 1,
                first_seq: 1,
                state: RunState::Running,
                finished_at: None,
            }),
        })
    }

    /// Record a frame and hand it to whoever is listening.
    ///
    /// Returns the sequence assigned. A send error means nobody is attached,
    /// which is the normal case this module exists for and not a failure.
    pub fn push(&self, payload: impl Into<Arc<str>>, terminal: bool) -> u64 {
        let payload: Arc<str> = payload.into();
        let frame = {
            let mut buf = self.buf.lock().expect("run buffer poisoned");
            let seq = buf.next_seq;
            buf.next_seq += 1;
            let frame = RunFrame { seq, payload, terminal };
            buf.bytes += frame.payload.len();
            buf.frames.push_back(frame.clone());
            while buf.frames.len() > MAX_FRAMES || (buf.bytes > MAX_BYTES && buf.frames.len() > 1) {
                if let Some(dropped) = buf.frames.pop_front() {
                    buf.bytes -= dropped.payload.len();
                    buf.first_seq = dropped.seq + 1;
                }
            }
            frame
        };
        let seq = frame.seq;
        let _ = self.tx.send(frame);
        seq
    }

    /// Everything after `after_seq` that is still held.
    pub fn snapshot(&self, after_seq: u64) -> Snapshot {
        let buf = self.buf.lock().expect("run buffer poisoned");
        // `after_seq` is exclusive and sequences start at 1, so 0 asks for
        // everything and needs no special case.
        let want_from = after_seq + 1;
        let gap = (want_from < buf.first_seq).then_some(buf.first_seq);
        let frames: Vec<RunFrame> = buf
            .frames
            .iter()
            .filter(|f| f.seq >= want_from)
            .cloned()
            .collect();
        Snapshot {
            saw_terminal: frames.iter().any(|f| f.terminal),
            gap,
            state: buf.state,
            last_seq: buf.next_seq.saturating_sub(1),
            frames,
        }
    }

    /// Mark the run over. Idempotent: the first terminal state wins, so a cancel
    /// that lands while the tail is already running does not relabel it.
    pub fn finish(&self, state: RunState) {
        let mut buf = self.buf.lock().expect("run buffer poisoned");
        if buf.state.is_terminal() {
            return;
        }
        buf.state = state;
        buf.finished_at = Some(Instant::now());
    }

    pub fn state(&self) -> RunState {
        self.buf.lock().expect("run buffer poisoned").state
    }

    fn finished_at(&self) -> Option<Instant> {
        self.buf.lock().expect("run buffer poisoned").finished_at
    }

    pub fn last_seq(&self) -> u64 {
        self.buf
            .lock()
            .expect("run buffer poisoned")
            .next_seq
            .saturating_sub(1)
    }

    pub fn first_seq(&self) -> u64 {
        self.buf.lock().expect("run buffer poisoned").first_seq
    }

    pub fn attached(&self) -> usize {
        self.attached.load(Ordering::SeqCst)
    }

    /// Register a subscriber. The returned guard decrements on drop, and is what
    /// cancels an `Ephemeral` run when the last one leaves.
    pub fn attach(self: &Arc<Self>) -> AttachGuard {
        let now = self.attached.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::debug!(
            target: "giap::runs",
            run_id = %self.run_id,
            attached = now,
            "client attached to run"
        );
        AttachGuard { run: self.clone() }
    }

    /// Subscribe to the live tail. Take this BEFORE reading a snapshot, or a
    /// frame produced between the two is lost by both paths.
    pub fn subscribe(&self) -> broadcast::Receiver<RunFrame> {
        self.tx.subscribe()
    }
}

/// Drops a subscriber's registration, and with it an `Ephemeral` run.
pub struct AttachGuard {
    run: Arc<RunHandle>,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        let left = self.run.attached.fetch_sub(1, Ordering::SeqCst) - 1;
        tracing::info!(
            target: "giap::runs",
            run_id = %self.run.run_id,
            attached_remaining = left,
            policy = ?self.run.policy,
            at_seq = self.run.last_seq(),
            "client detached from run"
        );
        if left == 0 && self.run.policy == RunPolicy::Ephemeral && !self.run.state().is_terminal() {
            tracing::info!(
                target: "giap::runs",
                run_id = %self.run.run_id,
                "last subscriber left an ephemeral run; cancelling"
            );
            self.run.cancel.cancel();
        }
    }
}

// ── The registry ──────────────────────────────────────────────────────────────

/// Why a run could not be registered.
#[derive(Debug, PartialEq, Eq)]
pub enum RegistryFull {
    AtCap { active: usize, max: usize },
}

struct RegistryInner {
    runs: HashMap<RunId, Arc<RunHandle>>,
    /// Most recent run per session — the discovery index. A restarted client
    /// knows its session id and nothing else, so lookup by session is not a
    /// convenience, it is the only way back in.
    by_session: HashMap<String, RunId>,
}

/// Every run this process is driving, plus the ones it has recently finished.
///
/// `std::sync::Mutex<HashMap>` rather than a concurrent map: this is touched
/// once per run start, per reattach and per sweep — never per token — so
/// contention is not a consideration, and `std`'s mutex is the one that makes
/// holding a lock across an `.await` a compile error rather than a silent stall.
/// Nothing here awaits inside the lock.
pub struct RunRegistry {
    inner: Mutex<RegistryInner>,
    max_runs: usize,
    retention: Duration,
}

impl RunRegistry {
    pub fn new(max_runs: usize, retention: Duration) -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                runs: HashMap::new(),
                by_session: HashMap::new(),
            }),
            max_runs,
            retention,
        }
    }

    /// Take ownership of a run, sweeping expired ones first so a pond that has
    /// been idle does not refuse a turn on the strength of runs that ended
    /// minutes ago.
    pub fn insert(&self, handle: Arc<RunHandle>) -> Result<(), RegistryFull> {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention);
        if inner.runs.len() >= self.max_runs {
            return Err(RegistryFull::AtCap {
                active: inner.runs.len(),
                max: self.max_runs,
            });
        }
        inner
            .by_session
            .insert(handle.session_id.clone(), handle.run_id.clone());
        inner.runs.insert(handle.run_id.clone(), handle);
        Ok(())
    }

    pub fn get(&self, run_id: &str) -> Option<Arc<RunHandle>> {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention);
        inner.runs.get(run_id).cloned()
    }

    pub fn for_session(&self, session_id: &str) -> Option<Arc<RunHandle>> {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention);
        let run_id = inner.by_session.get(session_id)?.clone();
        inner.runs.get(&run_id).cloned()
    }

    /// Evict finished runs past their retention. Returns how many went.
    pub fn sweep(&self) -> usize {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention)
    }

    fn sweep_locked(inner: &mut RegistryInner, retention: Duration) -> usize {
        let now = Instant::now();
        let expired: Vec<RunId> = inner
            .runs
            .iter()
            .filter(|(_, h)| {
                h.finished_at()
                    .is_some_and(|at| now.duration_since(at) >= retention)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(handle) = inner.runs.remove(id) {
                tracing::info!(
                    target: "giap::runs",
                    run_id = %handle.run_id,
                    state = ?handle.state(),
                    attached = handle.attached(),
                    "run evicted from the registry"
                );
                // Only clear the session index if it still points at this run;
                // a newer turn on the same session has already replaced it.
                if inner.by_session.get(&handle.session_id) == Some(id) {
                    inner.by_session.remove(&handle.session_id);
                }
            }
        }
        expired.len()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("run registry poisoned").runs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Everything the API layer needs to own detached runs.
///
/// One field on `AppState` rather than three, because roughly thirty
/// integration-test fixtures spell `AppState` out as a struct literal and each
/// extra field is thirty more mechanical edits.
pub struct RunSupervisor {
    pub registry: RunRegistry,
    /// Bounds concurrent DETACHED runs.
    ///
    /// Deliberately not `sse_semaphore`, for the reason already written on
    /// `notification_sse_semaphore`: one counter cannot mean both "clients
    /// reading" and "runs in flight". A detached run outlives its connection, so
    /// sharing the small interactive pool would let a handful of abandoned runs
    /// starve chat entirely.
    pub permits: Arc<tokio::sync::Semaphore>,
    /// Identifies THIS process.
    ///
    /// A run id minted under a different epoch names a run that died with the
    /// last process. Saying so is the difference between an honest "the server
    /// restarted, reload the session" and a bare 404 the client cannot tell
    /// apart from "your run finished and aged out".
    pub epoch: String,
}

impl RunSupervisor {
    pub fn new(max_runs: usize, retention: Duration) -> Self {
        Self {
            registry: RunRegistry::new(max_runs, retention),
            permits: Arc::new(tokio::sync::Semaphore::new(max_runs)),
            epoch: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl Default for RunSupervisor {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_RUNS, DEFAULT_RETENTION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(policy: RunPolicy) -> Arc<RunHandle> {
        RunHandle::new("sess-1".into(), RunOwner::Unattributed, policy)
    }

    #[test]
    fn a_frame_pushed_with_nobody_listening_is_not_an_error() {
        let run = handle(RunPolicy::Detached);
        assert_eq!(run.push("data", false), 1);
        assert_eq!(run.push("more", false), 2);
        assert_eq!(run.snapshot(0).frames.len(), 2);
    }

    #[test]
    fn a_snapshot_returns_only_what_came_after() {
        let run = handle(RunPolicy::Detached);
        for i in 0..5 {
            run.push(format!("f{i}"), false);
        }
        let snap = run.snapshot(2);
        assert_eq!(
            snap.frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
            vec![3, 4, 5],
            "after_seq is exclusive"
        );
        assert_eq!(snap.gap, None);
        assert_eq!(snap.last_seq, 5);
    }

    #[test]
    fn the_ring_evicts_and_says_so_rather_than_skipping_silently() {
        let run = handle(RunPolicy::Detached);
        for i in 0..(MAX_FRAMES + 10) {
            run.push(format!("f{i}"), false);
        }
        assert!(run.first_seq() > 1, "the ring should have evicted");
        let snap = run.snapshot(0);
        assert_eq!(
            snap.gap,
            Some(run.first_seq()),
            "a caller asking for an evicted position must be told, not quietly \
             handed a later frame as though it were the next one"
        );
    }

    #[test]
    fn a_caller_already_current_gets_nothing_and_no_gap() {
        let run = handle(RunPolicy::Detached);
        run.push("only", false);
        let snap = run.snapshot(0);
        assert_eq!(snap.frames.len(), 1);
        let snap = run.snapshot(snap.last_seq);
        assert!(snap.frames.is_empty());
        assert_eq!(snap.gap, None);
    }

    #[test]
    fn the_terminal_frame_is_visible_to_a_late_subscriber() {
        let run = handle(RunPolicy::Detached);
        run.push("text", false);
        run.push("{\"done\":true}", true);
        run.finish(RunState::Finished);
        let snap = run.snapshot(0);
        assert!(snap.saw_terminal);
        assert_eq!(snap.state, RunState::Finished);
    }

    #[test]
    fn the_first_terminal_state_wins() {
        let run = handle(RunPolicy::Detached);
        run.finish(RunState::Cancelled);
        run.finish(RunState::Finished);
        assert_eq!(
            run.state(),
            RunState::Cancelled,
            "a tail that completes after a cancel must not relabel the turn as \
             an ordinary finish"
        );
    }

    #[test]
    fn abandoning_an_ephemeral_run_cancels_it() {
        let run = handle(RunPolicy::Ephemeral);
        let guard = run.attach();
        assert!(!run.cancel.is_cancelled());
        drop(guard);
        assert!(
            run.cancel.is_cancelled(),
            "the last subscriber leaving is what used to drop the response body"
        );
    }

    #[test]
    fn abandoning_a_detached_run_leaves_it_running() {
        let run = handle(RunPolicy::Detached);
        drop(run.attach());
        assert!(!run.cancel.is_cancelled());
    }

    #[test]
    fn one_of_several_subscribers_leaving_does_not_cancel() {
        let run = handle(RunPolicy::Ephemeral);
        let first = run.attach();
        let second = run.attach();
        drop(first);
        assert!(!run.cancel.is_cancelled());
        drop(second);
        assert!(run.cancel.is_cancelled());
    }

    #[test]
    fn a_finished_ephemeral_run_is_not_cancelled_on_the_way_out() {
        let run = handle(RunPolicy::Ephemeral);
        let guard = run.attach();
        run.finish(RunState::Finished);
        drop(guard);
        assert!(
            !run.cancel.is_cancelled(),
            "cancelling a turn that already finished would mislabel it in every \
             log that reads the token"
        );
    }

    #[test]
    fn the_registry_finds_a_run_by_its_session() {
        let reg = RunRegistry::new(4, DEFAULT_RETENTION);
        let run = handle(RunPolicy::Detached);
        reg.insert(run.clone()).unwrap();
        let found = reg
            .for_session("sess-1")
            .expect("a restarted client knows only the session id");
        assert_eq!(found.run_id, run.run_id);
    }

    #[test]
    fn the_registry_refuses_past_its_cap() {
        let reg = RunRegistry::new(1, DEFAULT_RETENTION);
        reg.insert(handle(RunPolicy::Detached)).unwrap();
        let err = reg
            .insert(RunHandle::new(
                "sess-2".into(),
                RunOwner::Unattributed,
                RunPolicy::Detached,
            ))
            .unwrap_err();
        assert_eq!(err, RegistryFull::AtCap { active: 1, max: 1 });
    }

    #[test]
    fn a_sweep_evicts_finished_runs_and_never_a_running_one() {
        let reg = RunRegistry::new(4, Duration::ZERO);
        let running = handle(RunPolicy::Detached);
        let done = RunHandle::new("sess-2".into(), RunOwner::Unattributed, RunPolicy::Detached);
        reg.insert(running.clone()).unwrap();
        reg.insert(done.clone()).unwrap();
        done.finish(RunState::Finished);

        assert_eq!(reg.sweep(), 1);
        assert!(reg.get(&running.run_id).is_some());
        assert!(reg.get(&done.run_id).is_none());
        assert!(reg.for_session("sess-2").is_none());
    }

    #[test]
    fn a_swept_run_does_not_take_a_newer_turns_session_index_with_it() {
        let reg = RunRegistry::new(4, Duration::ZERO);
        let old = handle(RunPolicy::Detached);
        reg.insert(old.clone()).unwrap();
        old.finish(RunState::Finished);
        // A second turn on the SAME session, which now owns the index.
        let new = handle(RunPolicy::Detached);
        reg.insert(new.clone()).unwrap();

        reg.sweep();
        assert_eq!(
            reg.for_session("sess-1").map(|h| h.run_id.clone()),
            Some(new.run_id.clone()),
            "sweeping the old run must not orphan the live one"
        );
    }

    #[tokio::test]
    async fn a_subscriber_hears_frames_pushed_after_it_subscribed() {
        let run = handle(RunPolicy::Detached);
        let mut rx = run.subscribe();
        run.push("hello", false);
        let frame = rx.recv().await.unwrap();
        assert_eq!(&*frame.payload, "hello");
        assert_eq!(frame.seq, 1);
    }

    #[tokio::test]
    async fn a_lagging_subscriber_can_recover_everything_it_missed() {
        let run = handle(RunPolicy::Detached);
        let mut rx = run.subscribe();
        // Overrun the broadcast queue without overrunning the ring — the
        // relationship BROADCAST_CAP < MAX_FRAMES is what makes this possible.
        for i in 0..(BROADCAST_CAP + 50) {
            run.push(format!("f{i}"), false);
        }
        let err = rx.recv().await.unwrap_err();
        assert!(
            matches!(err, broadcast::error::RecvError::Lagged(_)),
            "expected the queue to have dropped frames"
        );
        let snap = run.snapshot(0);
        assert_eq!(snap.gap, None, "the ring still holds everything");
        assert_eq!(snap.frames.len(), BROADCAST_CAP + 50);
    }
}
