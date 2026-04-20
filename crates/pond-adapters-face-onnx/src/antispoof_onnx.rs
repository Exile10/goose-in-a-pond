//! ONNX-backed anti-spoofing — drop-in upgrade for the heuristic gate.
//!
//! Wraps a Silent-Face-Anti-Spoofing (MiniFASNetV2 / V1SE) ONNX model and
//! exposes the same [`AntispoofReport`] shape as the heuristic
//! [`crate::antispoof`] module so the embedding pipeline can switch between
//! the two without further code changes.
//!
//! # Why both paths exist
//!
//! The heuristic gate (saturation variance + highlight density + gradient
//! skew) is fast, dependency-free, and catches the obvious presentation
//! attacks (printed photos, flat phone screens) that we observed in
//! testing.  It misses higher-quality attacks — high-DPI prints, tablet
//! displays at the right brightness, video replays — and over-rejects in
//! some lighting (very even ring lights look "too uniform").
//!
//! The ONNX model is trained on a much wider attack catalogue and gives a
//! calibrated probability rather than a hand-tuned score.  When a model
//! file is present at `$POND_FACE_ANTISPOOF_PATH` the embedding extractor
//! prefers it; otherwise it falls back to the heuristic path.  Both paths
//! share the same threshold env var (`POND_FACE_ANTISPOOF_THRESHOLD`).
//!
//! # Pipeline shape
//!
//! Silent-Face takes an 80×80 BGR (yes, BGR — the open-source weights ship
//! that way) face crop and outputs a 3-class softmax over `[live, fake_2D,
//! fake_3D]`.  We collapse `fake_*` into a single "spoof probability" and
//! return the fraction in `[0, 1]`.  The crop must come from the same
//! aligned 112×112 we feed ArcFace; we down-sample with a Triangle filter.
//!
//! # Status
//!
//! This module compiles and runs end-to-end against any ONNX file that
//! matches the Silent-Face graph signature.  Until a real model is
//! deposited at the configured path, [`OnnxAntispoof::try_from_env`]
//! returns `Ok(None)` and the embedding pipeline keeps using the
//! heuristic.

use crate::antispoof::AntispoofReport;
use anyhow::{anyhow, Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

/// Silent-Face input geometry.  Don't change unless you also retrain.
const INPUT_SIDE: u32 = 80;

pub struct OnnxAntispoof {
    session:    Arc<Mutex<Session>>,
    model_path: PathBuf,
}

impl OnnxAntispoof {
    pub fn new(model_path: impl Into<PathBuf>) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "Silent-Face anti-spoof model not found at {}",
                path.display()
            ));
        }
        let session = Session::builder()
            .context("failed to create ort session builder for anti-spoof")?
            .commit_from_file(&path)
            .with_context(|| {
                format!("failed to load anti-spoof ONNX model at {}", path.display())
            })?;
        info!(path = %path.display(), "Silent-Face anti-spoof model loaded");
        Ok(Self {
            session:    Arc::new(Mutex::new(session)),
            model_path: path,
        })
    }

    /// Construct from `$POND_FACE_ANTISPOOF_PATH`, or return `Ok(None)` when
    /// the env var is unset / the file is missing.  Intentionally lenient:
    /// a missing model is the *expected* state on installs that haven't
    /// downloaded one yet — we don't want server boot to fail for it.
    pub fn try_from_env() -> Result<Option<Self>> {
        Self::try_from_env_var("POND_FACE_ANTISPOOF_PATH")
    }

    /// Load from an arbitrary env var.  Used to wire the secondary
    /// ensemble model (`POND_FACE_ANTISPOOF_PATH_2`).
    pub fn try_from_env_var(var: &str) -> Result<Option<Self>> {
        let Ok(raw) = std::env::var(var) else {
            return Ok(None);
        };
        let path = PathBuf::from(raw);
        if !path.exists() {
            warn!(
                "{} set to {} but file does not exist; skipping",
                var, path.display()
            );
            return Ok(None);
        }
        Ok(Some(Self::new(path)?))
    }

    /// Score an aligned 112×112 RGB crop and return a synthetic
    /// [`AntispoofReport`] whose `spoof_score` is the model's combined
    /// fake probability.  The diagnostic fields (saturation_var etc.)
    /// are zeroed because they are not produced by the ONNX path.
    pub fn analyse(&self, img: &RgbImage) -> Result<AntispoofReport> {
        // Down-sample 112×112 → 80×80 with a Triangle filter (same as the
        // detector preprocess paths; cheap and consistent).
        let small = DynamicImage::ImageRgb8(img.clone())
            .resize_exact(INPUT_SIDE, INPUT_SIDE, FilterType::Triangle)
            .to_rgb8();

        // Pack as NCHW float32 in BGR order.
        //
        // Pixel scaling differs between the two known Silent-Face ONNX
        // exports:
        //
        //   * yakhyo / minivision-ai MiniFASNetV2 (2-class [spoof, live]) —
        //     expects raw `[0, 255]` float32 (OpenCV default; the repo
        //     preprocess just does `face.astype(np.float32)` with no
        //     normalisation at all).  Feeding `[0, 1]` saturates the model
        //     and every frame scores ~0.9997.
        //   * Other 3-class Silent-Face exports — accept `[0, 1]`.
        //
        // We pick the convention based on the env var
        // `POND_FACE_ANTISPOOF_PIXEL_SCALE={auto,raw,unit}`.  Default
        // `auto` tries `raw` first (the common yakhyo export we ship
        // instructions for) and can be forced to `unit` for the minivision
        // 3-class variant.
        let scale = pixel_scale_from_env();
        let divisor: f32 = match scale {
            PixelScale::Raw => 1.0,
            PixelScale::Unit => 255.0,
        };
        let side = INPUT_SIDE as usize;
        let mut tensor = Array4::<f32>::zeros((1, 3, side, side));
        for (x, y, px) in small.enumerate_pixels() {
            let [r, g, b] = px.0;
            // Channel ordering: 0 = B, 1 = G, 2 = R.
            tensor[[0, 0, y as usize, x as usize]] = b as f32 / divisor;
            tensor[[0, 1, y as usize, x as usize]] = g as f32 / divisor;
            tensor[[0, 2, y as usize, x as usize]] = r as f32 / divisor;
        }

        let input = Tensor::from_array(tensor).context("failed to wrap anti-spoof input tensor")?;
        let mut sess = self
            .session
            .lock()
            .map_err(|_| anyhow!("anti-spoof session mutex poisoned"))?;
        let outputs = sess
            .run(ort::inputs![input])
            .context("Silent-Face anti-spoof inference failed")?;
        let (_name, first) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow!("anti-spoof model returned no outputs"))?;
        let (_shape, data) = first
            .try_extract_tensor::<f32>()
            .context("failed to extract anti-spoof output tensor")?;

        // Most Silent-Face exports return raw logits; some return softmax.
        // Apply a defensive softmax — it's idempotent on already-normalised
        // distributions up to FP error.
        //
        // Class-ordering convention varies by export:
        //
        //   * minivision-ai originals (3-class): [live, fake_2D, fake_3D]
        //   * yakhyo MiniFASNetV2 export (2-class): [spoof, live]  ← inverted!
        //
        // We try to auto-detect, and let the operator force either convention
        // via `POND_FACE_ANTISPOOF_LIVE_INDEX={auto,0,1}`.  Default `auto` picks
        // the last class for 2-class outputs and index 0 for 3-class outputs,
        // which matches both checkpoints we've tested against.
        let logits = data;
        if logits.len() < 2 {
            return Err(anyhow!(
                "Silent-Face anti-spoof returned {} values; expected ≥2",
                logits.len()
            ));
        }
        let max = logits.iter().cloned().fold(f32::MIN, f32::max);
        let exps: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
        let sum: f32 = exps.iter().sum();
        let probs: Vec<f32> = if sum > 0.0 {
            exps.iter().map(|e| e / sum).collect()
        } else {
            logits.to_vec()
        };

        let live_index = live_index_from_env(probs.len());
        let live = probs[live_index].clamp(0.0, 1.0);
        let spoof = (1.0 - live).clamp(0.0, 1.0);

        // Surface the full probability vector at info! so operators can
        // actually see what the model is saying — critical for diagnosing
        // "the score is stuck" when the class ordering or preprocess is
        // off for a particular ONNX export.
        info!(
            spoof, live, live_index, n_classes = probs.len(),
            probs = ?probs, logits = ?logits,
            "Silent-Face score"
        );
        Ok(AntispoofReport {
            spoof_score:       spoof,
            saturation_var:    0.0,
            highlight_density: 0.0,
            gradient_skew:     0.0,
        })
    }

    pub fn model_path(&self) -> &std::path::Path {
        &self.model_path
    }
}

/// Pixel-scale convention for the Silent-Face input tensor.
///
/// See the block comment in [`OnnxAntispoof::analyse`] for why this matters:
/// the yakhyo MiniFASNetV2 export expects raw `[0, 255]` floats, while the
/// minivision-ai 3-class originals expect `[0, 1]`.
#[derive(Clone, Copy, Debug)]
enum PixelScale {
    /// Feed pixels as-is, in `[0, 255]`.  Divisor = 1.
    Raw,
    /// Feed pixels as `[0, 1]`.  Divisor = 255.
    Unit,
}

/// Resolve the pixel-scale convention from
/// `POND_FACE_ANTISPOOF_PIXEL_SCALE={auto,raw,unit}`.  Auto → `Raw`
/// because the yakhyo MiniFASNetV2 weights we ship install instructions
/// for are the common case and want raw `[0, 255]` input.  Operators
/// using a 3-class minivision export should set `unit` explicitly.
fn pixel_scale_from_env() -> PixelScale {
    let raw = std::env::var("POND_FACE_ANTISPOOF_PIXEL_SCALE").ok();
    match raw.as_deref().unwrap_or("auto") {
        "auto" | "" | "raw" => PixelScale::Raw,
        "unit" => PixelScale::Unit,
        other => {
            warn!(
                raw = %other,
                "invalid POND_FACE_ANTISPOOF_PIXEL_SCALE; falling back to raw"
            );
            PixelScale::Raw
        }
    }
}

/// Resolve which softmax slot holds the "live" class.
///
/// Honours `POND_FACE_ANTISPOOF_LIVE_INDEX`:
///
///   * `auto` (default) — last slot for 2-class outputs (matches the yakhyo
///     MiniFASNetV2 export), first slot for 3-class outputs (matches the
///     original minivision-ai checkpoints).
///   * A literal integer (`0`, `1`, …) — force that slot.
///
/// Clamped to a valid range so a bad env value can't panic the adapter.
fn live_index_from_env(n_classes: usize) -> usize {
    let raw = std::env::var("POND_FACE_ANTISPOOF_LIVE_INDEX").ok();
    match raw.as_deref().unwrap_or("auto") {
        "auto" | "" => {
            if n_classes == 2 {
                1 // [spoof, live]
            } else {
                0 // [live, fake_2D, fake_3D]
            }
        }
        other => match other.parse::<usize>() {
            Ok(i) if i < n_classes => i,
            _ => {
                warn!(
                    n_classes, raw = %other,
                    "invalid POND_FACE_ANTISPOOF_LIVE_INDEX; falling back to auto"
                );
                if n_classes == 2 { 1 } else { 0 }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_from_env_with_unset_var_returns_none() {
        // SAFETY: we only mutate POND_FACE_ANTISPOOF_PATH for this test,
        // and the var should not be set in the default test environment.
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_PATH"); }
        let r = OnnxAntispoof::try_from_env().expect("should not error");
        assert!(r.is_none());
    }

    #[test]
    fn try_from_env_with_missing_file_returns_none() {
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_PATH", "/does/not/exist.onnx"); }
        let r = OnnxAntispoof::try_from_env().expect("should not error");
        assert!(r.is_none());
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_PATH"); }
    }

    #[test]
    fn new_fails_on_missing_path() {
        assert!(OnnxAntispoof::new("/no/such/file.onnx").is_err());
    }
}
