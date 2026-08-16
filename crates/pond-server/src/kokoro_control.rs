//! Reconfiguring the running Kokoro engine, downloads included.
//!
//! This is the `TtsControl` implementation, and it lives in `pond-server`
//! rather than in the adapter for one reason: fetching is the server's job.
//! `pond-adapters-kokoro` knows how to load a voice off disk and how to speak;
//! it has no HTTP client and should not grow one. `pond-api` in turn only sees
//! the port, so the route that applies a voice change does not know Kokoro
//! exists.
//!
//! ## Why a live apply exists at all
//!
//! Everything the settings screen changes used to take effect on the next
//! start: `ensure_kokoro_engine` ran at boot and `KokoroOutput` was built once.
//! Choosing a voice therefore did nothing you could hear until the pond was
//! restarted — which, for a device that lives on a shelf, means the setting
//! looked broken.
//!
//! All three changes are cheap enough to do live:
//!
//! | change | cost |
//! |---|---|
//! | pace | a tensor value |
//! | voice | a 522 KB style table, plus a download the first time |
//! | quality | drops the session; the next utterance loads the new weights |

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_adapters_kokoro::KokoroOutput;
use pond_core::models::ports::tts_control::{TtsApplied, TtsControl};

use crate::model_download;

/// The same map the Models page polls through `/models/download/progress`.
type Tracker = Arc<tokio::sync::RwLock<std::collections::HashMap<String, pond_api::DownloadEntry>>>;

pub struct KokoroTtsControl {
    engine: Arc<KokoroOutput>,
    data_dir: PathBuf,
    tracker: Tracker,
}

impl KokoroTtsControl {
    pub fn new(engine: Arc<KokoroOutput>, data_dir: PathBuf, tracker: Tracker) -> Self {
        Self {
            engine,
            data_dir,
            tracker,
        }
    }

    /// A progress sink that writes into the shared download tracker.
    ///
    /// Reported there rather than through a channel of its own so the picker
    /// reads the same feed as every other transfer on the page — one idea of
    /// "downloading", not two that can disagree.
    ///
    /// Chunks arrive from a blocking-ish loop, so the write is scheduled onto
    /// the runtime rather than awaited: a progress callback that blocks the
    /// transfer to update a UI is a bad trade.
    fn reporter(&self, category: &str) -> model_download::DlProgress {
        let tracker = self.tracker.clone();
        let category = category.to_string();
        Arc::new(move |filename: &str, downloaded: u64, total: u64| {
            let tracker = tracker.clone();
            let filename = filename.to_string();
            let category = category.clone();
            tokio::spawn(async move {
                let mut t = tracker.write().await;
                let e = t
                    .entry(filename.clone())
                    .or_insert_with(|| pond_api::DownloadEntry {
                        filename: filename.clone(),
                        category: category.clone(),
                        downloaded_bytes: 0,
                        total_bytes: None,
                        status: "downloading".into(),
                        finished_at: None,
                        control: Default::default(),
                        // Resume is handled by re-running `apply`, which
                        // re-derives the URL from the tier and voice — so this
                        // entry does not need to carry one.
                        url: None,
                    });
                e.downloaded_bytes = downloaded;
                e.total_bytes = Some(total);
                if downloaded >= total && total > 0 {
                    e.status = "done".into();
                    e.finished_at = Some(std::time::Instant::now());
                } else {
                    e.status = "downloading".into();
                    e.finished_at = None;
                }
            });
        })
    }

    fn voices_dir(&self) -> PathBuf {
        model_download::kokoro_dir(&self.data_dir).join("voices")
    }
}

#[async_trait]
impl TtsControl for KokoroTtsControl {
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied> {
        // Resolve the tier before anything reads it. `q8f16` returns digital
        // silence on aarch64 Linux, and `speak()` cannot tell a silent buffer
        // from a quiet one — so a household that picked it would get a pond
        // that appears to answer and makes no sound, with nothing in the log.
        //
        // Substituting here rather than at the point of use keeps the tier that
        // is downloaded, loaded, persisted and shown back to the household one
        // string instead of four that can disagree.
        let requested = quality;
        let quality = pond_adapters_kokoro::usable_quality(requested);
        if quality != requested {
            tracing::warn!(
                requested,
                using = quality,
                "requested voice quality does not produce audio on this machine; \
                 substituting the nearest tier that does"
            );
        }

        let mut out = TtsApplied {
            speed_milli: (speed * 1000.0).round().max(0.0) as u32,
            quality: quality.to_string(),
            ..Default::default()
        };

        // ── Weights for the requested tier ──
        //
        // Checked before the voice: a tier change is the expensive one, and
        // failing after a voice download would leave the household having paid
        // for a fetch that changed nothing.
        let kdir = model_download::kokoro_dir(&self.data_dir);
        let weights = kdir.join(pond_adapters_kokoro::model_filename(quality));
        if !weights.exists() {
            model_download::ensure_kokoro_engine_reporting(
                &self.data_dir,
                quality,
                voice,
                Some(self.reporter("tts_kokoro")),
            )
            .await;
            out.downloaded_weights = weights.exists();
            if !out.downloaded_weights {
                anyhow::bail!(
                    "could not fetch the {quality} voice engine; the current one is unchanged"
                );
            }
        }
        // Only reload when the file actually changed — `set_model` drops the
        // session, and dropping it for a tier that is already loaded would
        // make every save cost a reload.
        if self.engine.model_path().await != weights {
            self.engine.set_model(weights).await?;
            out.engine_reloaded = true;
        }

        // ── The voice ──
        let wanted = voice.trim();
        let name = if wanted.is_empty() {
            pond_adapters_kokoro::DEFAULT_VOICE
        } else {
            wanted
        };
        let path = pond_adapters_kokoro::voices::voice_path(&self.voices_dir(), name)
            .with_context(|| format!("{name:?} is not a usable Kokoro voice id"))?;
        if !path.exists() {
            // Selecting a voice you do not have is a download, not an error —
            // the picker offers every voice Kokoro publishes, and the fetch is
            // half a megabyte.
            model_download::ensure_kokoro_engine_reporting(
                &self.data_dir,
                quality,
                name,
                Some(self.reporter("tts_kokoro")),
            )
            .await;
            out.downloaded_voice = path.exists();
            if !out.downloaded_voice {
                anyhow::bail!("could not fetch the voice {name:?}; the current one is unchanged");
            }
        }
        self.engine.set_voice(name).await?;
        out.voice = self.engine.voice().await;

        // ── Pace ──
        out.speed_milli = (self.engine.set_speed(speed) * 1000.0).round() as u32;

        tracing::info!(
            voice = %out.voice,
            quality,
            reloaded = out.engine_reloaded,
            fetched_voice = out.downloaded_voice,
            fetched_weights = out.downloaded_weights,
            "TTS settings applied to the running engine"
        );
        Ok(out)
    }

    async fn installed_voices(&self) -> Vec<String> {
        self.engine.installed_voices()
    }
}
