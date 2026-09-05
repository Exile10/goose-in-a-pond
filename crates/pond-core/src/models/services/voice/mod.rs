//! Voice-pipeline services: [`fallback_voice_output`] keeps a TTS failure from
//! aborting a turn, [`instant_activation`] activates immediately for stdin loops
//! and deterministic tests, and [`spoken_time`] renders `HH:MM` as words so a
//! small model never has to.
pub mod fallback_voice_output;
pub mod instant_activation;
pub mod spoken_time;
