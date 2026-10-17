use crate::owner::MicHandle;
use crate::ring::CAPTURE_RATE_HZ;
use pond_voice::barge::{MicHold, SpeechEnergy};

pub struct MicEnergy(MicHandle);

impl MicEnergy {
    pub fn new(handle: &MicHandle) -> Self {
        Self(handle.clone())
    }
}

impl SpeechEnergy for MicEnergy {
    fn recent_rms(&self, window_ms: u64) -> Option<f32> {
        let shared = self.0.shared();
        if !shared.state().is_open() {
            return None;
        }
        let samples = (window_ms * CAPTURE_RATE_HZ as u64 / 1000) as usize;
        Some(shared.recent_rms(samples))
    }

    /// Claims a generation even when the mic is already open, so a capture that started
    /// earlier (the wake-word detector) cannot close it on the way out.
    fn hold_mic(&self) -> MicHold {
        let handle = self.0.clone();
        if !handle.shared().enabled() {
            return MicHold::none();
        }
        if !handle.state().is_open() {
            // The ring still holds the last capture's tail, which is not this reply's audio.
            handle.clear();
        }
        let generation = handle.open_session();
        MicHold::new(move || handle.close_session(generation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::MicState;
    use crate::testing::ScriptedCapture;
    use std::time::Duration;

    fn settle(h: &MicHandle, pred: impl Fn(&MicState) -> bool) {
        assert!(
            h.wait_for(pred, Duration::from_secs(2)),
            "state never settled; stuck at {:?}",
            h.state()
        );
    }

    /// Without a wake-word detector nothing opens the mic mid-reply, so barge-in heard nothing.
    #[test]
    fn a_hold_opens_a_closed_mic_so_the_reply_can_be_interrupted() {
        let (h, join) = crate::spawn(
            Box::new(ScriptedCapture::utterance(1_000, 0, 20)),
            CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        let energy = MicEnergy::new(&h);
        assert_eq!(energy.recent_rms(100), None, "closed: nothing to read");

        let hold = energy.hold_mic();
        settle(&h, |s| s.is_open());
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            energy.recent_rms(100).is_some_and(|rms| rms > 0.01),
            "a held mic must report the speech it hears"
        );

        drop(hold);
        settle(&h, |s| *s == MicState::Closed);
        h.shutdown();
        let _ = join.join();
    }

    #[test]
    fn a_hold_outlives_the_close_of_a_capture_that_opened_first() {
        let (h, join) = crate::spawn(
            Box::new(ScriptedCapture::silence(0, 20)),
            CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        let detector = h.open_session();
        settle(&h, |s| s.is_open());

        let hold = MicEnergy::new(&h).hold_mic();
        h.close_session(detector);
        std::thread::sleep(Duration::from_millis(100));
        assert!(h.state().is_open(), "the earlier capture closed a held mic");

        drop(hold);
        settle(&h, |s| *s == MicState::Closed);
        h.shutdown();
        let _ = join.join();
    }

    #[test]
    fn a_hold_releases_nothing_a_later_capture_claimed() {
        let (h, join) = crate::spawn(
            Box::new(ScriptedCapture::silence(0, 20)),
            CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        let hold = MicEnergy::new(&h).hold_mic();
        settle(&h, |s| s.is_open());

        let capture = h.open_session();
        drop(hold);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            h.state().is_open(),
            "the hold closed the follow-up capture's mic"
        );

        h.close_session(capture);
        settle(&h, |s| *s == MicState::Closed);
        h.shutdown();
        let _ = join.join();
    }

    #[test]
    fn a_disabled_mic_is_not_held_open() {
        let (h, join) = crate::spawn(
            Box::new(ScriptedCapture::silence(0, 20)),
            CAPTURE_RATE_HZ,
            5_000,
            false,
        );
        let _hold = MicEnergy::new(&h).hold_mic();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(h.state(), MicState::Closed, "the privacy setting must win");
        h.shutdown();
        let _ = join.join();
    }
}
