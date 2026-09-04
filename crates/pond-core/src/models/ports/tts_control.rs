//! Driven Port: TtsControl — reconfiguring the running speech engine so a voice change needs no
//! restart. Deliberately separate from [`VoiceOutput`](super::voice_output::VoiceOutput), which
//! the chat loop holds: nothing in the speaking path depends on this port. Implementations own
//! whatever fetching a change implies, so selecting an uninstalled voice downloads it here.

use anyhow::Result;
use async_trait::async_trait;

/// What actually happened when settings were applied. Returned rather than assumed, because the
/// three cases differ: a voice swap is instant, a first-time voice fetches half a megabyte, and a
/// quality change fetches up to 326 MB and reloads the engine.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TtsApplied {
    /// The voice now in use.
    pub voice: String,
    /// The pace now in use, as the engine's multiplier.
    pub speed_milli: u32,
    /// The quality tier now in use.
    pub quality: String,
    /// A voice file was fetched to satisfy this call.
    pub downloaded_voice: bool,
    /// Engine weights were fetched to satisfy this call.
    pub downloaded_weights: bool,
    /// The session was dropped and will reload on the next utterance.
    pub engine_reloaded: bool,
}

/// Driven Port: TtsControl
#[async_trait]
pub trait TtsControl: Send + Sync {
    /// Bring the live engine in line with `voice`, `speed` and `quality`, fetching whatever is
    /// missing first. Implementations must be safe to call while the engine is speaking: the
    /// settings screen calls this on every slider release.
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied>;

    /// Voices installed and ready to speak right now. The catalogue lists what *could* be used;
    /// this says what can be used without waiting, so a picker can name the choices that cost a
    /// download.
    async fn installed_voices(&self) -> Vec<String>;
}
