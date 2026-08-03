use crate::owner::{MicHandle, MicShared};
use crate::ring::CAPTURE_RATE_HZ;
use pond_voice::barge::SpeechEnergy;
use std::sync::Arc;

pub struct MicEnergy(Arc<MicShared>);

impl MicEnergy {
    pub fn new(handle: &MicHandle) -> Self {
        Self(handle.shared().clone())
    }
}

impl SpeechEnergy for MicEnergy {
    fn recent_rms(&self, window_ms: u64) -> Option<f32> {
        if !self.0.state().is_open() {
            return None;
        }
        let samples = (window_ms * CAPTURE_RATE_HZ as u64 / 1000) as usize;
        Some(self.0.recent_rms(samples))
    }
}
