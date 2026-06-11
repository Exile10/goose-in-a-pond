//! Voice-pipeline services (ASR/TTS/wake-word helpers).
//!
//! - [`fallback_voice_output`] — wraps a primary [`crate::models::ports::voice_output`]
//!   with a fallback (e.g. silent/log sink) so a TTS failure never aborts a turn.
//! - [`instant_activation`] — a no-op `StreamingWakeWordDetector` that activates
//!   immediately, used for keyboard/stdin-driven loops and deterministic tests.
pub mod fallback_voice_output;
pub mod instant_activation;
