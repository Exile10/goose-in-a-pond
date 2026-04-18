//! ONNX-based face embedding adapter.
//!
//! Implements [`FaceEmbeddingExtractor`] by running an ONNX embedding model
//! (ArcFace-512 by default, MobileFaceNet-128 supported) via the `ort` crate.
//!
//! # Pipeline
//!
//! 1. Decode image bytes (any format supported by the `image` crate).
//! 2. Resize to 112×112 RGB (the canonical input size for both ArcFace and
//!    MobileFaceNet variants).
//! 3. Normalise per-channel: `(pixel/255 - 0.5) / 0.5`.
//! 4. Feed through the ONNX model (NCHW layout).
//! 5. L2-normalise the resulting embedding so cosine-similarity reduces to
//!    a plain dot product at match time.
//!
//! # Face detection
//!
//! For Phase 2 the adapter expects a caller-supplied face crop (matches the
//! typical enrollment UX where the user frames their face in a guide).
//! Wiring in mtCNN face detection is tracked as a follow-up — the port
//! already returns `Ok(None)` on empty inputs so detection can be added
//! without breaking the API.
//!
//! # Runtime linkage
//!
//! The `ort` crate is built with `load-dynamic`.  The ONNX Runtime shared
//! library is resolved at startup via `dlopen`/`LoadLibrary` and can be
//! overridden with `ORT_DYLIB_PATH`.  On Jetson, point this at a TensorRT-
//! enabled ORT build to get CUDA acceleration for free.

pub mod detector;
pub use detector::UltraFaceDetector;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use image::imageops::FilterType;
use image::GenericImageView;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use pond_core::domain::face_recognition::BoundingBox;
use pond_core::ports::face_embedding_extractor::FaceEmbeddingExtractor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

/// Supported embedding model families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingModel {
    /// ArcFace (ResNet-100 / R50 / etc.) — 112×112 input, 512-d output.
    ArcFace512,
    /// MobileFaceNet — 112×112 input, 128-d output.
    MobileFaceNet128,
}

impl EmbeddingModel {
    pub fn input_size(&self) -> u32 {
        112
    }
    pub fn dims(&self) -> u32 {
        match self {
            Self::ArcFace512 => 512,
            Self::MobileFaceNet128 => 128,
        }
    }
}

/// Per-channel mean/std for both ArcFace and MobileFaceNet: normalise
/// `(pixel/255 - 0.5) / 0.5` → range \[-1, 1\].
const MEAN: f32 = 0.5;
const SCALE: f32 = 1.0 / 0.5;

pub struct OnnxFaceEmbeddingExtractor {
    // `Session::run` takes `&mut self`; we guard with a std::sync::Mutex
    // because the entire inference call already runs inside `spawn_blocking`.
    session: Arc<Mutex<Session>>,
    model: EmbeddingModel,
    model_path: PathBuf,
}

impl OnnxFaceEmbeddingExtractor {
    /// Build a new extractor backed by the ONNX model at `model_path`.
    ///
    /// Fails if the file is missing or the ONNX Runtime shared library
    /// cannot be loaded from the system library path / `ORT_DYLIB_PATH`.
    pub fn new(model_path: impl Into<PathBuf>, model: EmbeddingModel) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "face embedding model not found at {}",
                path.display()
            ));
        }

        let session = Session::builder()
            .context(
                "failed to create ort session builder — is the ONNX Runtime library available?",
            )?
            .commit_from_file(&path)
            .with_context(|| format!("failed to load ONNX model at {}", path.display()))?;

        info!(model = ?model, path = %path.display(), "face embedding ONNX model loaded");

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            model,
            model_path: path,
        })
    }

    /// Path the adapter is serving embeddings from (diagnostic).
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }
}

/// Clamp a client/detector bbox to the image dimensions.  Returns `None`
/// if the box has zero area after clamping.
fn clamp_bbox(bbox: BoundingBox, img_w: u32, img_h: u32) -> Option<(u32, u32, u32, u32)> {
    if bbox.x >= img_w || bbox.y >= img_h || bbox.width == 0 || bbox.height == 0 {
        return None;
    }
    let w = bbox.width.min(img_w - bbox.x);
    let h = bbox.height.min(img_h - bbox.y);
    Some((bbox.x, bbox.y, w, h))
}

/// Fallback when no bbox is supplied: take the largest square centred on
/// the image.  Works reasonably for headshot-style framings; a real face
/// detector (mtCNN) should be wired in front for general photos.
fn center_square(img_w: u32, img_h: u32) -> (u32, u32, u32, u32) {
    let side = img_w.min(img_h);
    let x = (img_w - side) / 2;
    let y = (img_h - side) / 2;
    (x, y, side, side)
}

/// Decode + (optionally crop) + resize + normalise an image into an
/// NCHW `[1, 3, 112, 112]` f32 tensor.
fn preprocess(
    image_bytes: &[u8],
    model: EmbeddingModel,
    bbox: Option<BoundingBox>,
) -> Result<Array4<f32>> {
    let img = image::load_from_memory(image_bytes).context("failed to decode image bytes")?;
    let (img_w, img_h) = img.dimensions();

    // Pick the crop rectangle: client-supplied bbox clamped to image, or
    // a center-square fallback when absent.
    let (cx, cy, cw, ch) = bbox
        .and_then(|b| clamp_bbox(b, img_w, img_h))
        .unwrap_or_else(|| center_square(img_w, img_h));

    let cropped = img.crop_imm(cx, cy, cw, ch);

    let size = model.input_size();
    let resized = cropped
        .resize_exact(size, size, FilterType::Triangle)
        .to_rgb8();

    let h = size as usize;
    let w = size as usize;
    let mut tensor = Array4::<f32>::zeros((1, 3, h, w));
    for y in 0..h {
        for x in 0..w {
            let pixel = resized.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                let v = (pixel[c] as f32) / 255.0;
                tensor[[0, c, y, x]] = (v - MEAN) * SCALE;
            }
        }
    }
    Ok(tensor)
}

/// L2-normalise the raw ONNX output.  Validates dimensionality.
fn postprocess(raw: &[f32], expected_dims: u32) -> Result<Vec<f32>> {
    if raw.len() != expected_dims as usize {
        return Err(anyhow!(
            "embedding model returned {} values, expected {}",
            raw.len(),
            expected_dims
        ));
    }
    let norm: f32 = raw.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 {
        return Err(anyhow!("embedding has zero norm — likely a degenerate input"));
    }
    Ok(raw.iter().map(|v| v / norm).collect())
}

#[async_trait]
impl FaceEmbeddingExtractor for OnnxFaceEmbeddingExtractor {
    async fn extract_embedding(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
    ) -> Result<Option<Vec<f32>>> {
        if image_bytes.is_empty() {
            return Ok(None);
        }

        let model = self.model;
        let session = self.session.clone();
        let bytes = image_bytes.to_vec();

        // ONNX inference is CPU/GPU-bound; run on the blocking pool so we
        // don't stall the tokio reactor.
        let embedding = tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
            let tensor = preprocess(&bytes, model, bbox)?;
            let input =
                Tensor::from_array(tensor).context("failed to wrap input as ort tensor")?;

            let mut session = session
                .lock()
                .map_err(|_| anyhow!("face embedding session mutex was poisoned"))?;
            let outputs = session
                .run(ort::inputs![input])
                .context("ONNX inference failed")?;
            let (_name, first) = outputs
                .iter()
                .next()
                .ok_or_else(|| anyhow!("ONNX model returned no outputs"))?;
            let (_shape, data) = first
                .try_extract_tensor::<f32>()
                .context("failed to extract f32 tensor from ONNX output")?;

            postprocess(data, model.dims())
        })
        .await
        .context("face inference task panicked")??;

        debug!(dims = embedding.len(), "face embedding extracted");
        Ok(Some(embedding))
    }

    fn embedding_dims(&self) -> u32 {
        self.model.dims()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_model_dims() {
        assert_eq!(EmbeddingModel::ArcFace512.dims(), 512);
        assert_eq!(EmbeddingModel::MobileFaceNet128.dims(), 128);
        assert_eq!(EmbeddingModel::ArcFace512.input_size(), 112);
    }

    #[test]
    fn new_fails_when_model_missing() {
        let result = OnnxFaceEmbeddingExtractor::new(
            "/nonexistent/arcface.onnx",
            EmbeddingModel::ArcFace512,
        );
        let err = match result {
            Ok(_) => panic!("expected error for missing model"),
            Err(e) => e,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("not found"), "unexpected error: {}", msg);
    }

    #[test]
    fn postprocess_l2_normalises() {
        // 3-4-5 triangle: norm = 5, result = (0.6, 0.8), ‖result‖₂ = 1.0
        let raw = vec![3.0_f32, 4.0];
        let out = postprocess(&raw, 2).unwrap();
        let norm: f32 = out.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn postprocess_rejects_dimension_mismatch() {
        let raw = vec![1.0_f32; 100];
        assert!(postprocess(&raw, 512).is_err());
    }

    #[test]
    fn postprocess_rejects_zero_vector() {
        let raw = vec![0.0_f32; 512];
        assert!(postprocess(&raw, 512).is_err());
    }

    #[test]
    fn preprocess_produces_correct_shape() {
        let img = image::RgbImage::from_pixel(1, 1, image::Rgb([255, 0, 0]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        let tensor = preprocess(&bytes, EmbeddingModel::ArcFace512, None).unwrap();
        assert_eq!(tensor.shape(), &[1, 3, 112, 112]);
        // Pure red image should map to (1, -1, -1) per channel after normalisation.
        assert!((tensor[[0, 0, 0, 0]] - 1.0).abs() < 1e-5);
        assert!((tensor[[0, 1, 0, 0]] + 1.0).abs() < 1e-5);
        assert!((tensor[[0, 2, 0, 0]] + 1.0).abs() < 1e-5);
    }

    /// Helper: build a solid-colour RGB PNG.
    fn solid_png(w: u32, h: u32, px: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(px));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn center_square_picks_largest_centered_square() {
        // wider-than-tall
        assert_eq!(center_square(200, 100), (50, 0, 100, 100));
        // taller-than-wide
        assert_eq!(center_square(100, 200), (0, 50, 100, 100));
        // square
        assert_eq!(center_square(100, 100), (0, 0, 100, 100));
    }

    #[test]
    fn clamp_bbox_trims_to_image_bounds() {
        let bbox = BoundingBox { x: 90, y: 90, width: 50, height: 50 };
        // 100×100 image → clamped box is 10×10 at (90,90).
        assert_eq!(clamp_bbox(bbox, 100, 100), Some((90, 90, 10, 10)));
    }

    #[test]
    fn clamp_bbox_rejects_out_of_bounds_origin() {
        let bbox = BoundingBox { x: 150, y: 0, width: 10, height: 10 };
        assert!(clamp_bbox(bbox, 100, 100).is_none());
    }

    #[test]
    fn clamp_bbox_rejects_zero_area() {
        assert!(clamp_bbox(
            BoundingBox { x: 10, y: 10, width: 0, height: 20 },
            100, 100
        ).is_none());
    }

    #[test]
    fn preprocess_honours_client_bbox() {
        // 20×10 image, left half is red, right half is blue.
        let mut img = image::RgbImage::new(20, 10);
        for y in 0..10 {
            for x in 0..20 {
                let px = if x < 10 { [255, 0, 0] } else { [0, 0, 255] };
                img.put_pixel(x, y, image::Rgb(px));
            }
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();

        // Crop to the blue half — the resulting tensor should have B channel ≈ 1
        // and R channel ≈ -1 at every pixel.
        let bbox = BoundingBox { x: 10, y: 0, width: 10, height: 10 };
        let tensor = preprocess(&bytes, EmbeddingModel::ArcFace512, Some(bbox)).unwrap();
        assert!((tensor[[0, 0, 50, 50]] + 1.0).abs() < 1e-5, "R should be -1");
        assert!((tensor[[0, 2, 50, 50]] - 1.0).abs() < 1e-5, "B should be +1");
    }

    #[test]
    fn preprocess_center_square_fallback_when_no_bbox() {
        // 200×100 image: centre-square crop is x=50..150. Paint left third red,
        // middle third green, right third blue; the green band fills most of
        // the cropped window.
        let mut img = image::RgbImage::new(200, 100);
        for y in 0..100 {
            for x in 0..200 {
                let px = if x < 66 {
                    [255, 0, 0]
                } else if x < 133 {
                    [0, 255, 0]
                } else {
                    [0, 0, 255]
                };
                img.put_pixel(x, y, image::Rgb(px));
            }
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();

        let tensor = preprocess(&bytes, EmbeddingModel::ArcFace512, None).unwrap();
        // Middle of the tensor corresponds to x≈100 in the source — green band.
        assert!(tensor[[0, 1, 55, 55]] > 0.5, "G channel should dominate center");
    }

    #[test]
    fn preprocess_out_of_bounds_bbox_falls_back_to_center() {
        // With an invalid bbox we should still produce a sane tensor (fallback).
        let bytes = solid_png(10, 10, [128, 128, 128]);
        let bbox = BoundingBox { x: 1000, y: 1000, width: 10, height: 10 };
        let tensor = preprocess(&bytes, EmbeddingModel::ArcFace512, Some(bbox)).unwrap();
        assert_eq!(tensor.shape(), &[1, 3, 112, 112]);
    }
}
