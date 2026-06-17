//! Tracing initialisation for Goose In A Pond.
//!
//! Three sinks are registered on startup:
//!
//! 1. **stdout** — human-readable coloured output (same as before).
//! 2. **Rolling file** — plain-text log, one file per day under `<data_dir>/logs/`.
//!    Older files are kept on disk; the OS or the user prunes them as needed.
//! 3. **SQLite event log** — WARN-and-above events are forwarded to the
//!    `event_log` table in `pond_logs.db` via an async drain task so they can
//!    be queried from the UI. The drain is started by calling
//!    [`LogDrainHandle::drain_into`] once the database is available.
//!
//! The rolling-file writer runs on a background thread managed by
//! `tracing-appender`. The [`FileWriterGuard`] returned by `drain_into` keeps
//! that thread alive and flushes pending writes when dropped.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::field::Visit;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use pond_core::ports::event_log::EventLogRepository;

// ── Internal channel entry ────────────────────────────────────────────────────

struct ChannelEntry {
    level: String,
    source: String,
    message: String,
}

// ── Custom tracing Layer ──────────────────────────────────────────────────────

struct EventLogLayer {
    tx: UnboundedSender<ChannelEntry>,
}

/// Extracts the `message` field from a tracing event's field set.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for EventLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        if visitor.message.is_empty() {
            return;
        }
        let meta = event.metadata();
        // Level::to_string() produces uppercase ("ERROR", "WARN", …)
        let _ = self.tx.send(ChannelEntry {
            level: meta.level().to_string(),
            source: meta.target().to_string(),
            message: visitor.message,
        });
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Keeps the rolling-file background writer thread alive.
/// When this is dropped the background thread is joined and all buffered
/// bytes are flushed to disk.
pub struct FileWriterGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Returned by [`init_tracing`].
///
/// Keep this value alive for the duration of the process.  Call
/// [`drain_into`](Self::drain_into) after the database is ready to start
/// persisting WARN+ events to the SQLite event log.
pub struct LogDrainHandle {
    rx: UnboundedReceiver<ChannelEntry>,
    file_guard: tracing_appender::non_blocking::WorkerGuard,
}

impl LogDrainHandle {
    /// Spawn the async drain task that writes buffered log events into the
    /// SQLite event log.  If `repo` is `None` the channel is simply closed
    /// and events are discarded (file logging still works).
    ///
    /// Returns a [`FileWriterGuard`] that must be held until the process exits.
    pub fn drain_into(self, repo: Option<Arc<dyn EventLogRepository>>) -> FileWriterGuard {
        let LogDrainHandle { rx, file_guard } = self;
        if let Some(repo) = repo {
            let mut rx = rx;
            tokio::spawn(async move {
                while let Some(entry) = rx.recv().await {
                    let _ = repo
                        .insert(&entry.level, &entry.source, &entry.message, None)
                        .await;
                }
            });
        }
        // If repo is None, dropping rx closes the channel; senders silently
        // discard further events via the `let _ = tx.send(...)` pattern.
        FileWriterGuard { _guard: file_guard }
    }
}

/// Initialise the global tracing subscriber.
///
/// **Call this once**, early in `main`, before any tracing macros fire.
/// The returned [`LogDrainHandle`] must be kept alive; call
/// [`drain_into`](LogDrainHandle::drain_into) once the database pool is ready.
pub fn init_tracing(debug: bool, data_dir: &Path) -> LogDrainHandle {
    let filter_str = if debug {
        "debug,sqlx=warn,hyper=warn,tower=warn,reqwest=warn,hyper_util=warn,rustls=warn"
    } else {
        "info"
    };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| filter_str.into());

    // ── Rolling file appender ────────────────────────────────────────────
    // Produces daily files: <data_dir>/logs/pond.log.YYYY-MM-DD
    let log_dir = data_dir.join("logs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "pond.log");
    let (non_blocking_file, file_guard) = tracing_appender::non_blocking(file_appender);

    // ── Event-log channel ────────────────────────────────────────────────
    // WARN+ only — keeps the event_log table a useful incident history rather
    // than a verbose mirror of the debug stream.
    let (tx, rx) = mpsc::unbounded_channel();
    let db_layer = EventLogLayer { tx }.with_filter(LevelFilter::WARN);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stdout))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_file)
                .with_ansi(false),
        )
        .with(db_layer)
        .init();

    LogDrainHandle { rx, file_guard }
}
