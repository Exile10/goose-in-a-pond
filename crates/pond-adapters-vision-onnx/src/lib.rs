//! ONNX vision classifier (#130 follow-up) — upgrades the vision pipeline's
//! plain `"motion"` events into `"person"` / `"pet"` / `"package"` using a
//! small local detector. All inference is on-device.
//!
//! ```text
//! motion frame ─► letterbox 416² BGR ─► YOLOX-Nano (ort, load-dynamic)
//!                                          │ [1, 3549, 85]
//!                         decode + NMS ◄───┘
//!                              │ COCO classes → person / pet / package
//!                              ▼
//!                     Vec<Detection> → pipeline picks best ≥ confidence floor
//! ```
//!
//! Model: **YOLOX-Nano** (Apache-2.0, ~0.9M params). Drop `yolox_nano.onnx`
//! into `<data_dir>/models/vision/` and set `vision_classifier_model` — the
//! adapter loads it at startup and degrades to unlabelled motion when absent.
//! NanoDet-Plus (Apache-2.0) is the planned alternative; it needs its own
//! decoder, tracked as a follow-up. Ultralytics YOLO models are deliberately
//! avoided: AGPL-3.0 is incompatible with GIAP's Apache-2.0 license.
//!
//! Runtime: `ort` with `load-dynamic`, exactly like `pond-adapters-face-onnx`
//! — the system ONNX Runtime is linked at startup (`ORT_DYLIB_PATH`), so
//! Jetson deployments can use a CUDA/TensorRT build without recompiling.

mod classifier;
mod decode;
mod labels;
mod preprocess;

pub use classifier::OnnxVisionClassifier;
