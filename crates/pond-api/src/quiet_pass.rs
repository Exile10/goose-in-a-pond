//! The quiet-time compaction pass, and the turns that arrive while it runs.
//!
//! A quiet pass summarises a conversation near the edge of its window between turns, so that the
//! summary is paid while nobody is waiting. A turn that arrives while it runs usually stops it: a
//! turn for another conversation needs the model, and a pass that was only getting ahead of its
//! conversation can try again at the next quiet. The exception is a turn for the conversation
//! being compacted when that turn would start by compacting it anyway. Stopping the pass then
//! throws the summary away for the turn to write it again from the start, so the turn waits for
//! the pass instead, and is answered from the summary the pass wrote.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

/// The one quiet pass that may be running; the inference lane runs them one at a time.
#[derive(Default)]
pub struct QuietPasses {
    running: Mutex<Option<Running>>,
    next_id: AtomicU64,
}

struct Running {
    id: u64,
    session_id: String,
    its_turn_waits: bool,
    stop: Arc<AtomicBool>,
    waiting: Arc<AtomicUsize>,
    ended: watch::Receiver<()>,
}

/// A running pass. Dropping it ends the pass and releases every turn waiting on it, so drop it
/// only once the compaction has finished or been abandoned.
pub struct QuietPass<'a> {
    passes: &'a QuietPasses,
    id: u64,
    stop: Arc<AtomicBool>,
    waiting: Arc<AtomicUsize>,
    _ended: watch::Sender<()>,
}

impl QuietPasses {
    /// A pass starts compacting `session_id`. `its_turn_waits`: the conversation's next turn would
    /// start by compacting it, so that turn waits for this pass rather than stopping it.
    pub fn begin(&self, session_id: &str, its_turn_waits: bool) -> QuietPass<'_> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let waiting = Arc::new(AtomicUsize::new(0));
        let (ended_tx, ended_rx) = watch::channel(());
        *self.running.lock().unwrap_or_else(|e| e.into_inner()) = Some(Running {
            id,
            session_id: session_id.to_string(),
            its_turn_waits,
            stop: stop.clone(),
            waiting: waiting.clone(),
            ended: ended_rx,
        });
        QuietPass {
            passes: self,
            id,
            stop,
            waiting,
            _ended: ended_tx,
        }
    }

    /// Every turn path calls this with its conversation before it reaches the model. `None`: no
    /// pass stands in its way. `Some`: wait on it before going on. A pass the turn would have to
    /// repeat runs to its end; any other is told to stop, and ends within one of its polls.
    pub fn turn_arrives(&self, session_id: &str) -> Option<PassWait> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let pass = running.as_ref()?;
        if !(pass.its_turn_waits && pass.session_id == session_id) {
            pass.stop.store(true, Ordering::SeqCst);
        }
        // Counted before this returns, so the pass never sees this turn in flight but unaccounted.
        pass.waiting.fetch_add(1, Ordering::SeqCst);
        Some(PassWait {
            kept: !pass.stop.load(Ordering::SeqCst),
            _waiting: Waiting(pass.waiting.clone()),
            ended: pass.ended.clone(),
        })
    }
}

/// A turn waiting on a pass.
pub struct PassWait {
    kept: bool,
    _waiting: Waiting,
    ended: watch::Receiver<()>,
}

impl PassWait {
    /// The pass is summarising this turn's own conversation, and the turn waits for the summary
    /// rather than writing it again; worth telling the person why the answer is late.
    pub fn compacting_for_this_turn(&self) -> bool {
        self.kept
    }

    /// Returns once the pass has ended, however it ended.
    pub async fn until_the_pass_ends(mut self) {
        // An error is the signal: the pass dropped its sender.
        while self.ended.changed().await.is_ok() {}
    }
}

/// One turn waiting on a pass; uncounted when the wait ends or is abandoned.
struct Waiting(Arc<AtomicUsize>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl QuietPass<'_> {
    /// A turn that must not wait for this pass has arrived: end it now.
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// How many turns are waiting on this pass; a turn in flight that is not one of them has
    /// not announced itself.
    pub fn turns_waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

impl Drop for QuietPass<'_> {
    fn drop(&mut self) {
        let mut running = self
            .passes
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if running.as_ref().is_some_and(|r| r.id == self.id) {
            *running = None;
        }
        // `_ended` drops after this, which wakes every waiting turn.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn ends_within(wait: PassWait, ms: u64) -> bool {
        tokio::time::timeout(Duration::from_millis(ms), wait.until_the_pass_ends())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn no_pass_means_no_wait() {
        let passes = QuietPasses::default();
        assert!(passes.turn_arrives("s1").is_none());
    }

    #[tokio::test]
    async fn the_turn_a_pass_is_doing_the_work_for_waits_for_it() {
        let passes = QuietPasses::default();
        let pass = passes.begin("s1", true);
        let wait = passes.turn_arrives("s1").expect("the turn waits");
        assert!(wait.compacting_for_this_turn());
        assert!(!pass.stop_requested(), "the turn keeps the pass going");
        assert_eq!(pass.turns_waiting(), 1);
        let waiter = tokio::spawn(wait.until_the_pass_ends());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "released before the pass ended");
        drop(pass);
        tokio::time::timeout(Duration::from_millis(1000), waiter)
            .await
            .expect("released when the pass ended")
            .unwrap();
        assert!(passes.turn_arrives("s1").is_none(), "the pass is gone");
    }

    #[tokio::test]
    async fn a_turn_for_another_conversation_stops_the_pass() {
        let passes = QuietPasses::default();
        let pass = passes.begin("s1", true);
        let wait = passes
            .turn_arrives("s2")
            .expect("waits for the stopped pass to end");
        assert!(!wait.compacting_for_this_turn());
        assert!(pass.stop_requested());
        drop(pass);
        assert!(ends_within(wait, 1000).await);
    }

    #[tokio::test]
    async fn a_pass_that_was_only_getting_ahead_is_stopped_by_its_own_turn() {
        let passes = QuietPasses::default();
        let pass = passes.begin("s1", false);
        let wait = passes
            .turn_arrives("s1")
            .expect("waits for the stopped pass to end");
        assert!(
            pass.stop_requested(),
            "nothing is lost: the turn would not compact"
        );
        drop(pass);
        assert!(ends_within(wait, 1000).await);
    }

    #[tokio::test]
    async fn an_abandoned_wait_is_no_longer_counted() {
        let passes = QuietPasses::default();
        let pass = passes.begin("s1", true);
        let wait = passes.turn_arrives("s1").expect("the turn waits");
        assert_eq!(pass.turns_waiting(), 1);
        // A turn whose client went away drops its wait.
        drop(wait);
        assert_eq!(pass.turns_waiting(), 0);
    }

    #[tokio::test]
    async fn an_old_pass_ending_does_not_end_a_newer_one() {
        let passes = QuietPasses::default();
        let old = passes.begin("s1", true);
        let new = passes.begin("s2", true);
        drop(old);
        let wait = passes
            .turn_arrives("s2")
            .expect("the newer pass still stands");
        assert!(!new.stop_requested());
        drop(new);
        assert!(ends_within(wait, 1000).await);
    }
}
