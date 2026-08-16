//! Driven Port: TtsControl
//!
//! Reconfiguring the *running* speech engine, so a voice change does not need
//! the pond restarted.
//!
//! Separate from [`VoiceOutput`](super::voice_output::VoiceOutput) on purpose.
//! `VoiceOutput` is what the chat loop holds: say this, stop, play a tone. It
//! has no business knowing that voices have files behind them or that a
//! quality tier is a different set of weights. This port is the other half —
//! the one the settings screen drives — and nothing in the speaking path
//! depends on it.
//!
//! Implementations own whatever fetching a change implies. Selecting a voice
//! the household does not have yet is a download; the caller asks for the
//! voice and waits, rather than being told to go and install something first.

use anyhow::Result;
use async_trait::async_trait;

/// What actually happened when settings were applied.
///
/// Returned rather than assumed, because the three cases feel different to a
/// person: a voice swap is instant, a first-time voice pauses to fetch half a
/// megabyte, and a quality change fetches up to 326 MB and then reloads the
/// engine. A UI that cannot tell them apart has to either lie or spin.
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
    /// Bring the live engine in line with `voice`, `speed` and `quality`,
    /// fetching whatever is missing first.
    ///
    /// Implementations must be safe to call while the engine is speaking: the
    /// settings screen calls this on every slider release.
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied>;

    /// Voices installed and ready to speak right now.
    ///
    /// The catalogue lists what *could* be used; this says what can be used
    /// without waiting, which is what a picker needs in order to be honest
    /// about which choices cost a download.
    async fn installed_voices(&self) -> Vec<String>;
}
