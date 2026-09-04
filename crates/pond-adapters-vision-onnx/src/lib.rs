//! ONNX vision classifier (#130 follow-up): labels motion as person/pet/package
//! on-device with YOLOX-Nano (Apache-2.0) from `<data_dir>/models/vision/`,
//! degrading to unlabelled motion when absent. Ultralytics YOLO is avoided, its
//! AGPL-3.0 being incompatible with Apache-2.0. Runtime is `ort` `load-dynamic`.

mod classifier;
mod decode;
mod labels;
mod preprocess;

pub use classifier::OnnxVisionClassifier;
