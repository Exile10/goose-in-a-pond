//! [`OnnxVisionClassifier`] — the [`VisionClassifier`] port over a local
//! YOLOX ONNX model. Loaded once at startup; the pipeline calls it with the
//! single frame that triggered a motion event (never the full stream), so
//! even CPU inference comfortably fits the event rate.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use pond_core::user_data::domain::vision::{Detection, Frame};
use pond_core::user_data::ports::vision::VisionClassifier;

use crate::decode::{decode_yolox, nms};
use crate::labels::coco_to_giap_label;
use crate::preprocess::letterbox_bgr_chw;

/// YOLOX-Nano's released input resolution.
const INPUT_SIZE: usize = 416;
const NUM_CLASSES: usize = 80;
/// Pre-NMS score floor. The pipeline applies its own final confidence gate
/// (`MIN_CLASSIFIER_CONFIDENCE`) on top of this.
const SCORE_THRESH: f32 = 0.25;
const IOU_THRESH: f32 = 0.45;

pub struct OnnxVisionClassifier {
    session: Arc<Mutex<Session>>,
}

impl OnnxVisionClassifier {
    /// Load the model at `model_path` (e.g. `yolox_nano.onnx`). Fails fast at
    /// startup if the file is missing or the runtime can't load it — the
    /// caller degrades to unlabelled `"motion"` events.
    pub fn new(model_path: impl Into<PathBuf>) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "vision classifier model not found at {}",
                path.display()
            ));
        }
        let session = Session::builder()
            .context("failed to create ort session builder")?
            .commit_from_file(&path)
            .with_context(|| format!("failed to load vision ONNX model at {}", path.display()))?;
        tracing::info!(path = %path.display(), "vision classifier loaded (YOLOX)");
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
        })
    }
}

#[async_trait]
impl VisionClassifier for OnnxVisionClassifier {
    async fn classify(&self, frame: &Frame) -> Result<Vec<Detection>> {
        let frame = frame.clone();
        let session = Arc::clone(&self.session);

        // ONNX inference is CPU/GPU-bound — keep it off the async runtime.
        tokio::task::spawn_blocking(move || -> Result<Vec<Detection>> {
            let input = letterbox_bgr_chw(&frame, INPUT_SIZE)?;

            let mut sess = session
                .lock()
                .map_err(|_| anyhow!("vision classifier session mutex poisoned"))?;
            let tensor = Tensor::from_array(input)?;
            let outputs = sess
                .run(ort::inputs![tensor])
                .context("vision classifier inference failed")?;

            // Single output: [1, N, 85] raw predictions.
            let (_name, value) = outputs
                .iter()
                .next()
                .ok_or_else(|| anyhow!("vision model produced no outputs"))?;
            let (_shape, data) = value
                .try_extract_tensor::<f32>()
                .context("failed to extract vision model output tensor")?;

            let raw = decode_yolox(data, NUM_CLASSES, INPUT_SIZE, SCORE_THRESH);
            let kept = nms(raw, IOU_THRESH);

            // Collapse boxes to per-label best confidence: GIAP events carry
            // "what was seen", not where.
            let mut best: Vec<Detection> = Vec::new();
            for det in kept {
                let Some(label) = coco_to_giap_label(det.class_idx) else {
                    continue;
                };
                match best.iter_mut().find(|d| d.label == label) {
                    Some(existing) => {
                        existing.confidence = existing.confidence.max(det.score as f64)
                    }
                    None => best.push(Detection {
                        label: label.to_string(),
                        confidence: det.score as f64,
                    }),
                }
            }
            Ok(best)
        })
        .await
        .context("vision classifier task panicked")?
    }
}
