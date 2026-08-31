//! Silero VAD as a [`SpeechDetector`].
//!
//! ## What this buys over the threshold it replaces
//!
//! The detector this sits beside calls anything louder than `0.005` speech.
//! Measured against the real model, white noise at 0.01 amplitude — a fan, a
//! fridge, a laptop under load — scores **0.08** here and is correctly silence,
//! while the RMS gate sees a level of 0.01, calls it speech, and holds the
//! microphone open until the hard cap. That is the failure this exists to fix,
//! and it is not fixable by moving the threshold: lowering it clips quiet
//! speech, raising it deafens the assistant in a quiet room.
//!
//! On the same measurement, clear speech scores 0.945 on average with 44 of 46
//! windows over the threshold, and digital silence peaks at 0.044. The margin
//! is wide enough that the exact threshold barely matters.
//!
//! ## The window is not negotiable
//!
//! The model takes exactly 512 samples at 16 kHz and carries an LSTM state
//! between calls. The capture loop hands over whatever arrived since it last
//! looked — about 480 samples at a 30 ms poll, but only about. So the frames
//! are re-cut by [`Windower`], which keeps the leftover between calls so no
//! sample is seen twice or skipped. Feeding an LSTM overlapping audio does not
//! error; it just carries a slightly wrong state forward forever, and the
//! detector is quietly worse than the one that was benchmarked.
//!
//! ## The first window after a reset is not trustworthy
//!
//! With a zeroed state the model scores 0.27 on a window of unambiguous speech
//! — it needs a window or two of context. That is harmless in the role it is
//! used for here, which is deciding when speech has *stopped*: a false silence
//! at the very start of an utterance is overruled by the speech that follows.
//! It would be actively wrong for onset detection, which is one reason onset
//! stays on the energy gate.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ndarray::Array;
use ort::session::Session;
use ort::value::Tensor;
use pond_voice::dsp::{SpeechDetector, Windower};

/// Samples per inference. Fixed by the model.
pub const WINDOW: usize = 512;
/// The model is trained at 16 kHz and the capture path already runs there.
pub const SAMPLE_RATE: i64 = 16_000;
/// Shape of the recurrent state the model threads between windows.
const STATE_SHAPE: (usize, usize, usize) = (2, 1, 128);

/// Probability above which a window counts as speech.
///
/// Silero's own default, and the measured margin is wide enough that it is not
/// a tuning knob: speech averages 0.945 and steady noise peaks below 0.09.
pub const DEFAULT_THRESHOLD: f32 = 0.5;

pub struct SileroDetector {
    session: Session,
    windower: Windower,
    state: Array<f32, ndarray::Ix3>,
    threshold: f32,
    /// The most recent verdict, returned when a call completed no window.
    ///
    /// A 480-sample read cannot always fill a 512-sample window, so roughly one
    /// call in nine decides nothing new. Holding the last answer is right:
    /// the alternative — defaulting to silence — would inject a spurious
    /// silence frame at a fixed beat and drag the endpoint in early.
    last: bool,
}

impl SileroDetector {
    /// Load the model. Fails if the file is missing so the caller can fall back
    /// to the energy gate rather than run with a detector that never fires.
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        Self::with_threshold(model_path, DEFAULT_THRESHOLD)
    }

    pub fn with_threshold(model_path: impl AsRef<Path>, threshold: f32) -> Result<Self> {
        let path: PathBuf = model_path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(anyhow!("silero VAD model not found at {}", path.display()));
        }
        let session = Session::builder()
            .context("failed to create ort session builder")?
            // One thread, deliberately. The measured cost is ~0.5 ms per 32 ms
            // window — about 1.6% of a core on a Jetson — so there is nothing
            // to parallelise, and on a board sharing six cores with an LLM an
            // unbounded pool is a way to make everything else slower for no
            // gain.
            .with_intra_threads(1)
            // ort's builder errors carry the builder itself, which anyhow's
            // `context` cannot absorb; flatten to a message.
            .map_err(|e| anyhow!("failed to pin ort intra-op threads: {e}"))?
            .commit_from_file(&path)
            .with_context(|| format!("failed to load silero VAD model at {}", path.display()))?;

        tracing::info!(path = %path.display(), threshold, "silero VAD loaded");
        Ok(Self {
            session,
            windower: Windower::new(WINDOW),
            state: Array::zeros(STATE_SHAPE),
            threshold,
            last: false,
        })
    }

    /// Run one 512-sample window, advancing the recurrent state.
    fn infer(&mut self, window: &[f32]) -> Result<f32> {
        let input = Array::from_shape_vec((1, WINDOW), window.to_vec())
            .context("silero: window was not 512 samples")?;
        let sr = Array::from_shape_vec((), vec![SAMPLE_RATE]).expect("scalar shape is valid");

        let outputs = self
            .session
            .run(ort::inputs![
                "input" => Tensor::from_array(input)?,
                "state" => Tensor::from_array(self.state.clone())?,
                "sr"    => Tensor::from_array(sr)?,
            ])
            .context("silero VAD inference failed")?;

        // The new state must be carried forward or the model is reset every
        // window, which turns a sequence model into a much worse frame model
        // and reports nothing louder than an error.
        let (shape, next) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .context("silero: could not read the recurrent state back")?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        self.state = Array::from_shape_vec(ndarray::IxDyn(&dims), next.to_vec())
            .context("silero: returned state had an unexpected shape")?
            .into_dimensionality::<ndarray::Ix3>()
            .context("silero: returned state was not 3-dimensional")?;

        let (_, prob) = outputs["output"]
            .try_extract_tensor::<f32>()
            .context("silero: could not read the speech probability")?;
        prob.first()
            .copied()
            .ok_or_else(|| anyhow!("silero: empty probability output"))
    }
}

impl SpeechDetector for SileroDetector {
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        // The trait returns a verdict, not a `Result`, and that is the right
        // shape for a caller deciding whether someone is still talking — but it
        // means an inference failure has to become one of the two answers.
        //
        // It becomes *speech*. A detector that has stopped working must not be
        // able to end an utterance: reporting silence would truncate whatever
        // the user was saying and hand the recogniser half a sentence, which
        // reads as the model mishearing rather than the VAD dying. Reporting
        // speech degrades to the hard recording cap instead — late, but whole —
        // and the log says why.
        let mut latest = None;
        let mut failure = None;

        // `session` and `state` are borrowed inside the closure, so the windows
        // are collected first rather than inferred in place.
        let mut windows: Vec<[f32; WINDOW]> = Vec::new();
        self.windower.push(frame, |w| {
            let mut owned = [0.0f32; WINDOW];
            owned.copy_from_slice(w);
            windows.push(owned);
        });

        for window in &windows {
            match self.infer(window) {
                Ok(p) => latest = Some(p >= self.threshold),
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = failure {
            tracing::warn!(error = %e, "silero VAD failed; treating as speech so the turn is not cut short");
            self.last = true;
            return true;
        }

        if let Some(verdict) = latest {
            self.last = verdict;
        }
        self.last
    }

    fn reset(&mut self) {
        // All three, or the next utterance inherits this one's tail: a stale
        // LSTM state biases the first windows, a stale leftover splices the end
        // of the last turn onto the start of the next, and a stale verdict is
        // returned verbatim until a window completes.
        self.windower.reset();
        self.state = Array::zeros(STATE_SHAPE);
        self.last = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model is not in the repo, so everything here is either about the
    /// pieces that do not need it, or is `#[ignore]` and needs `SILERO_MODEL`.
    fn model_path() -> Option<PathBuf> {
        std::env::var("SILERO_MODEL").ok().map(PathBuf::from)
    }

    #[test]
    fn a_missing_model_is_an_error_not_a_panic() {
        // The caller falls back to the energy gate on this, so it must be a
        // value it can match on.
        // `.map(drop)` because `SileroDetector` holds an ort `Session`, which is
        // not `Debug` — and giving it a `Debug` impl to satisfy a test would be
        // the test dictating the type.
        let err = SileroDetector::new("/nonexistent/silero.onnx")
            .map(drop)
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    #[ignore = "needs the model; set SILERO_MODEL"]
    fn speech_scores_high_and_steady_noise_does_not() {
        let Some(path) = model_path() else {
            panic!("set SILERO_MODEL to onnx/model.onnx from onnx-community/silero-vad")
        };
        let mut d = SileroDetector::new(&path).expect("load");

        // Steady low-level noise at 0.01 amplitude: twice the RMS gate's
        // threshold, so the detector this replaces calls it speech.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..WINDOW * 20)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.01
            })
            .collect();
        assert!(
            !d.is_speech(&noise),
            "steady noise at twice the RMS threshold must not read as speech"
        );

        d.reset();
        // Digital silence, unambiguously.
        assert!(!d.is_speech(&vec![0.0; WINDOW * 10]));
    }

    #[test]
    #[ignore = "needs the model; set SILERO_MODEL"]
    fn a_short_read_holds_the_previous_verdict() {
        let Some(path) = model_path() else { return };
        let mut d = SileroDetector::new(&path).expect("load");
        // Fewer than 512 samples completes no window, so the answer must be the
        // last one rather than a default.
        let before = d.is_speech(&vec![0.0; 100]);
        assert!(!before, "the initial verdict is silence");
        assert_eq!(d.is_speech(&vec![0.0; 100]), before);
    }
}
