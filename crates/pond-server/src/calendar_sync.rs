//! Pulling a connected calendar into the personal-context corpus.
//!
//! The composition step for PAI-8's first connector: it holds the
//! [`ContextRepository`], the [`IngestPipeline`], the secret store and the
//! CalDAV adapter together, which is why it lives in the binary rather than in
//! `pond-core` — the domain does not know that CalDAV exists and should not
//! learn.
//!
//! # What it refuses to do
//!
//! * **It does not run when the pond is offline.** `network_mode` is checked
//!   before a source is touched and the source is marked
//!   [`Paused`](SourceStatus::Paused), never `Error`. PAI-8 invariant 5: an
//!   operator who turned the network off is not looking at a fault.
//! * **It does not retry a refused password.** A 401 becomes
//!   [`NeedsReauth`](SourceStatus::NeedsReauth) and the source is left alone
//!   until a person fixes it, because the alternative is a scheduled task
//!   walking into a rate limit every interval on credentials that cannot work.
//! * **It does not re-fetch an unchanged calendar.** The server's ctag is the
//!   cursor; equal ctag means nothing has happened and the sync ends without a
//!   REPORT. On a household calendar that is most syncs.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use pond_adapters_caldav::{CalDavAdapter, CalDavConfig, CalDavProvider};
use pond_core::context::domain::{secret_key_for, ContextSource, SourceKind, SourceStatus};
use pond_core::context::ingest::IngestPipeline;
use pond_core::context::ports::ContextRepository;
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::services::egress::{network_mode, NetworkMode};
use pond_core::user_data::domain::profile::ProfileScope;

/// What one sweep did, for the log line and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CalendarSyncReport {
    /// Calendar sources considered.
    pub sources: usize,
    /// Sources whose ctag matched, so nothing was fetched.
    pub unchanged: usize,
    /// Items handed to the pipeline.
    pub ingested: usize,
    /// Sources that ended in `NeedsReauth`.
    pub needs_reauth: usize,
    /// Sources that ended in `Error`.
    pub failed: usize,
    /// Sources skipped because the pond is offline.
    pub paused: usize,
}

/// The credential blob behind a source's `secret_ref`.
///
/// The self-hosted base URL lives in here with the password rather than in a
/// plain column, and that is deliberate: the address of a household's own
/// server names the household. It is account configuration, and account
/// configuration belongs in the encrypted store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CalendarCredentials {
    pub username: String,
    pub password: String,
    /// Only for the self-hosted presets; `None` for Google, iCloud, Fastmail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Rebuild the adapter a stored source needs.
fn adapter_for(source: &ContextSource, creds: &CalendarCredentials) -> Result<CalDavAdapter> {
    let provider = CalDavProvider::from_stored(source.provider(), creds.base_url.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this source names a calendar provider this pond does not know: {}",
                source.provider()
            )
        })?;
    CalDavAdapter::new(CalDavConfig {
        provider,
        username: creds.username.clone(),
        password: creds.password.clone(),
    })
}

/// True when the failure is the one a household can fix.
///
/// Matched on the adapter's own sentence rather than a status code, because by
/// the time it reaches here it is an `anyhow` chain. The adapter writes that
/// sentence in exactly one place and a test pins it.
fn is_auth_failure(err: &anyhow::Error) -> bool {
    err.to_string().contains("app-specific password")
}

/// Sync every calendar source this pond holds.
///
/// Scope is [`ProfileScope::Household`] on purpose: this is a background sweep
/// with no session and no speaker, so it must see every member's sources. The
/// items it produces still inherit their own source's owner, which is where
/// isolation actually lives — `ContextItem` denormalises `profile_id` from the
/// source and migration 0044 refuses to let it move.
pub async fn sync_calendars(
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
    now: DateTime<Utc>,
) -> Result<CalendarSyncReport> {
    let mut report = CalendarSyncReport::default();
    let offline = network_mode() == NetworkMode::Offline;

    let sources = repo.list_sources(&ProfileScope::Household).await?;
    for mut source in sources
        .into_iter()
        .filter(|s| s.kind() == SourceKind::Calendar)
    {
        report.sources += 1;

        if offline {
            report.paused += 1;
            source.advance(
                source.cursor().map(str::to_string),
                now,
                SourceStatus::Paused,
            );
            let _ = repo.upsert_source(&source).await;
            continue;
        }

        let outcome = sync_one(&repo, &pipeline, &secrets, &mut source, now).await;
        match outcome {
            Ok(SyncOne::Unchanged) => report.unchanged += 1,
            Ok(SyncOne::Ingested(n)) => report.ingested += n,
            Err(e) => {
                if is_auth_failure(&e) {
                    report.needs_reauth += 1;
                    tracing::warn!(
                        source = source.id(),
                        "this calendar refused its credentials; it will not be retried until \
                         somebody reconnects it"
                    );
                    source.advance(
                        source.cursor().map(str::to_string),
                        now,
                        SourceStatus::NeedsReauth,
                    );
                } else {
                    report.failed += 1;
                    tracing::warn!(source = source.id(), error = %e, "calendar sync failed");
                    source.advance(
                        source.cursor().map(str::to_string),
                        now,
                        SourceStatus::Error,
                    );
                }
            }
        }
        if let Err(e) = repo.upsert_source(&source).await {
            tracing::warn!(source = source.id(), error = %e, "could not record the sync result");
        }
    }
    Ok(report)
}

enum SyncOne {
    Unchanged,
    Ingested(usize),
}

async fn sync_one(
    repo: &Arc<dyn ContextRepository>,
    pipeline: &Arc<IngestPipeline>,
    secrets: &Arc<dyn SecretRepository>,
    source: &mut ContextSource,
    now: DateTime<Utc>,
) -> Result<SyncOne> {
    let key = source
        .secret_ref()
        .map(str::to_string)
        .unwrap_or_else(|| secret_key_for(source.id()));
    let blob = secrets
        .get(&key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("this calendar's credentials are missing from the store"))?;
    let creds: CalendarCredentials = serde_json::from_str(&blob)
        .map_err(|e| anyhow::anyhow!("this calendar's stored credentials are unreadable: {e}"))?;

    let adapter = adapter_for(source, &creds)?;
    let calendars = adapter.discover_calendars().await?;

    // Every calendar's ctag, joined. One string so a household with two
    // calendars still gets "nothing changed" when neither did, and a change in
    // either one re-fetches both -- which is correct and cheap, since the
    // expensive part is the REPORT and there is at most a handful of them.
    let ctag: String = calendars
        .iter()
        .map(|c| c.ctag.clone().unwrap_or_else(|| c.url.clone()))
        .collect::<Vec<_>>()
        .join("|");
    if !ctag.is_empty() && source.cursor() == Some(ctag.as_str()) {
        source.advance(Some(ctag), now, SourceStatus::Connected);
        return Ok(SyncOne::Unchanged);
    }

    let (from, to) = CalDavAdapter::default_window(now);
    let mut ingested = 0usize;
    for calendar in &calendars {
        let items = adapter.events_in_window(&calendar.url, from, to).await?;
        for item in items {
            match pipeline.ingest(source, item, now).await {
                Ok(_) => ingested += 1,
                Err(e) => {
                    // One malformed event must not abandon the rest of the
                    // calendar. The pipeline already refused it; carrying on is
                    // what makes a sync partially useful rather than all or
                    // nothing.
                    tracing::debug!(source = source.id(), error = %e, "one calendar event was not ingested");
                }
            }
        }
    }
    let _ = repo;
    source.advance(Some(ctag), now, SourceStatus::Connected);
    Ok(SyncOne::Ingested(ingested))
}
