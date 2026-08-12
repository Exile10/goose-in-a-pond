//! Voice-pipeline services (ASR/TTS/wake-word helpers).
//!
//! - [`fallback_voice_output`] — wraps a primary [`crate::models::ports::voice_output`]
//!   with a fallback (e.g. silent/log sink) so a TTS failure never aborts a turn.
//! - [`instant_activation`] — a no-op `StreamingWakeWordDetector` that activates
//!   immediately, used for keyboard/stdin-driven loops and deterministic tests.
//! - [`spoken_time`] — converts digit `HH:MM` time into a natural spoken
//!   phrase so voice-mode prompts never ask a small model to do that
//!   conversion itself.
pub mod fallback_voice_output;
pub mod instant_activation;
pub mod spoken_time;
