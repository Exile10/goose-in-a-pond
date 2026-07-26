//! Tracing initialisation for Goose In A Pond.
//!
//! Three sinks are registered on startup:
//!
//! 1. **stdout** — human-readable coloured output (same as before).
//! 2. **Rolling file** — plain-text log, one file per day under `<data_dir>/logs/`.
//!    Older files are kept on disk; the OS or the user prunes them as needed.
//! 3. **SQLite event log** — routed to the `event_log` table in `pond_logs.db`
//!    via an async drain task.  Two categories of events reach the DB:
//!    - WARN and above from any target (incident history).
//!    - INFO from targets starting with `giap::trace` (correlated turn events:
//!      turn start/end, tool calls, inference, provider swaps, outbound HTTP).
//!    Structured key-value fields are serialised as JSON into the `metadata` column,
//!    enabling `WHERE json_extract(metadata,'$.session_id') = ?` queries.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::field::Visit;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use pond_core::security::ports::event_log::OperationalLogRepository;

// ── Internal channel entry ────────────────────────────────────────────────────

struct ChannelEntry {
    level: String,
    /// tracing target (e.g. `"giap::trace"`, `"pond_server"`)
    source: String,
    message: String,
    /// JSON blob of all structured key-value fields except `message`.
    metadata: Option<String>,
}

// ── Visitor: captures message + all key-value fields ─────────────────────────

#[derive(Default)]
struct EventVisitor {
    message: String,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields.insert(field.name().to_string(), value.into());
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}").into());
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_string(), value.into());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.insert(field.name().to_string(), value.into());
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.insert(field.name().to_string(), value.into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name().to_string(), value.into());
    }
}

// ── Custom per-layer filter ───────────────────────────────────────────────────

/// Routes events to the SQLite event log when:
/// - Level is WARN or above (incident/error history), OR
/// - Level is INFO and the tracing target starts with `giap::trace`
///   (structured turn-level event stream for session correlation).
struct TraceFilter;

impl<S: Subscriber> tracing_subscriber::layer::Filter<S> for TraceFilter {
    fn enabled(&self, meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        *meta.level() <= Level::WARN
            || (*meta.level() == Level::INFO && meta.target().starts_with("giap::trace"))
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::INFO)
    }
}

// ── Custom tracing Layer ──────────────────────────────────────────────────────

struct EventLogLayer {
    tx: UnboundedSender<ChannelEntry>,
}

impl<S: Subscriber> Layer<S> for EventLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        if visitor.message.is_empty() && visitor.fields.is_empty() {
            return;
        }
        let metadata = if visitor.fields.is_empty() {
            None
        } else {
            serde_json::to_string(&visitor.fields).ok()
        };
        let meta = event.metadata();
        let _ = self.tx.send(ChannelEntry {
            level: meta.level().to_string(),
            source: meta.target().to_string(),
            message: visitor.message,
            metadata,
        });
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Keeps the rolling-file background writer thread alive.
/// Dropped at server shutdown to flush buffered bytes to disk.
pub struct FileWriterGuard {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Returned by [`init_tracing`].
///
/// Keep this value alive for the duration of the process.  Call
/// [`drain_into`](Self::drain_into) after the database is ready to start
/// persisting events to the SQLite event log.
pub struct LogDrainHandle {
    rx: UnboundedReceiver<ChannelEntry>,
    file_guard: tracing_appender::non_blocking::WorkerGuard,
}

impl LogDrainHandle {
    /// Spawn the async drain task that writes buffered log events into the
    /// SQLite event log.  If `repo` is `None` the channel is closed and events
    /// are discarded (file logging still works).
    ///
    /// Returns a [`FileWriterGuard`] that must be held until the process exits.
    pub fn drain_into(self, repo: Option<Arc<dyn OperationalLogRepository>>) -> FileWriterGuard {
        let LogDrainHandle { rx, file_guard } = self;
        if let Some(repo) = repo {
            let mut rx = rx;
            tokio::spawn(async move {
                while let Some(entry) = rx.recv().await {
                    let _ = repo
                        .insert(
                            &entry.level,
                            &entry.source,
                            &entry.message,
                            entry.metadata.as_deref(),
                        )
                        .await;
                }
            });
        }
        FileWriterGuard { _guard: file_guard }
    }
}

/// Initialise the global tracing subscriber.
///
/// **Call this once**, early in `main`, before any tracing macros fire.
/// The returned [`LogDrainHandle`] must be kept alive; call
/// [`drain_into`](LogDrainHandle::drain_into) once the database pool is ready.
pub fn init_tracing(debug: bool, data_dir: &Path) -> LogDrainHandle {
    // Non-interactive commands (serve, setup, status, …) keep the normal INFO
    // console. The interactive `chat` path opts into a quiet console directly.
    init_tracing_with_console(debug, data_dir, ConsoleSink::Stdout, false)
}

/// Where the human-readable console log layer writes.
///
/// `pond-server chat --json-events` uses `Stderr` so stdout carries NOTHING
/// but the NDJSON contract lines; every other command keeps `Stdout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleSink {
    Stdout,
    Stderr,
}

pub fn init_tracing_with_console(
    debug: bool,
    data_dir: &Path,
    console: ConsoleSink,
    console_quiet: bool,
) -> LogDrainHandle {
    // ggml / llama.cpp / whisper.cpp route their verbose per-load and per-token
    // logs into tracing at INFO (via llama-cpp-2's send_logs_to_tracing). On the
    // interactive voice-chat path that dumps the model-load banner and tensor
    // tables straight onto the console, drowning the voice UI. Carve those
    // targets down to WARN so only genuine errors survive; inference is surfaced
    // as a clean one-line summary instead (see ChatService turn completion).
    // `RUST_LOG` still overrides everything for a full-verbosity debug session.
    // The goose crates log the whole agent loop (extension init, session
    // writes, reply spans) at INFO/WARN on every turn. GIAP owns its own
    // telemetry (giap::trace events, the [turn] summary line, turn_metrics),
    // so goose's narration is pure per-turn formatting and I/O cost — carve
    // it down to ERROR. Real goose failures still surface, and `RUST_LOG`
    // restores full goose verbosity for a debug session.
    let filter_str = if debug {
        "debug,sqlx=warn,hyper=warn,tower=warn,reqwest=warn,hyper_util=warn,rustls=warn,\
         llama-cpp-2=warn,ggml=warn,whisper=warn,\
         goose=error,goose_providers=error,goose_local_inference=error,rmcp=error"
    } else {
        "info,llama-cpp-2=error,ggml=error,whisper=error,\
         goose=error,goose_providers=error,goose_local_inference=error,rmcp=error"
    };
    let rust_log_set = std::env::var("RUST_LOG").is_ok();

    // Per-layer filters so the console can be quieter than the file (a single
    // shared filter would couple them). The FILE always gets the full detail;
    // the CONSOLE, on the interactive voice-chat path (`console_quiet`), is
    // dropped to WARN so only real warnings/errors reach it — the curated,
    // human-facing turn lines are printed directly (via `diag!`/`out!`, not
    // tracing) and are unaffected. `RUST_LOG`, when set, wins for BOTH so a
    // debug session sees everything on the console too.
    let file_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter_str.into());
    let console_filter = if console_quiet && !rust_log_set {
        tracing_subscriber::EnvFilter::new("warn")
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter_str.into())
    };

    // ── Rolling file appender ────────────────────────────────────────────
    // Produces daily files: <data_dir>/logs/pond.log.YYYY-MM-DD
    let log_dir = data_dir.join("logs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "pond.log");
    let (non_blocking_file, file_guard) = tracing_appender::non_blocking(file_appender);

    // ── Event-log channel (with selective filter) ────────────────────────
    let (tx, rx) = mpsc::unbounded_channel();
    let db_layer = EventLogLayer { tx }.with_filter(TraceFilter);

    // Route the console fmt layer to stdout or stderr. In `--json-events` mode
    // stdout is reserved for NDJSON, so diagnostics go to stderr.
    let console_layer = match console {
        ConsoleSink::Stdout => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout as fn() -> std::io::Stdout)
            .with_filter(console_filter)
            .boxed(),
        ConsoleSink::Stderr => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr as fn() -> std::io::Stderr)
            .with_filter(console_filter)
            .boxed(),
    };

    tracing_subscriber::registry()
        .with(console_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking_file)
                .with_ansi(false)
                .with_filter(file_filter),
        )
        .with(db_layer)
        .init();

    LogDrainHandle { rx, file_guard }
}
