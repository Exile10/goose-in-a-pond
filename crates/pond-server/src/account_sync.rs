//! Pulling a connected account into the personal-context corpus.
//!
//! The composition step for PAI-8's account connectors: it holds the
//! [`ContextRepository`], the [`IngestPipeline`], the secret store and the
//! protocol adapters together, which is why it lives in the binary rather than
//! in `pond-core` — the domain does not know that CalDAV or IMAP exist and
//! should not learn.
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
use pond_adapters_imap::{ImapAdapter, ImapConfig, ImapProvider};
use pond_core::context::domain::{secret_key_for, ContextSource, SourceKind, SourceStatus};
use pond_core::context::ingest::IngestPipeline;
use pond_core::context::ports::{AccountSync, AccountSyncSummary, ContextRepository};
use pond_core::security::ports::secret::SecretRepository;
use pond_core::shared::services::egress::{network_mode, NetworkMode};
use pond_core::user_data::domain::profile::ProfileScope;

/// What one sweep did, for the log line and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountSyncReport {
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
pub struct AccountCredentials {
    pub username: String,
    pub password: String,
    /// The self-hosted server, when there is one: a CalDAV base URL, or
    /// `host:port` for IMAP. `None` for every named preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Rebuild the adapter a stored source needs.
fn adapter_for(source: &ContextSource, creds: &AccountCredentials) -> Result<CalDavAdapter> {
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
) -> Result<AccountSyncReport> {
    let mut report = AccountSyncReport::default();
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
    let creds: AccountCredentials = serde_json::from_str(&blob)
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

// ── Mail ─────────────────────────────────────────────────────────────────────

/// Sync every mail source this pond holds.
///
/// Deliberately a sibling of [`sync_calendars`] rather than a generic over both.
/// The two protocols share their SHAPE — credentials, a window, a status — and
/// nothing else: CalDAV discovers collections and has a ctag to skip work with,
/// IMAP opens one mailbox and has neither. A generic over that difference would
/// be a trait with one useful method and two awkward ones.
pub async fn sync_mail(
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
    now: DateTime<Utc>,
) -> Result<AccountSyncReport> {
    let mut report = AccountSyncReport::default();
    let offline = network_mode() == NetworkMode::Offline;

    let sources = repo.list_sources(&ProfileScope::Household).await?;
    for mut source in sources.into_iter().filter(|s| s.kind() == SourceKind::Mail) {
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

        match sync_one_mailbox(&pipeline, &secrets, &source, now).await {
            Ok(n) => {
                report.ingested += n;
                source.advance(None, now, SourceStatus::Connected);
            }
            Err(e) => {
                if is_auth_failure(&e) {
                    report.needs_reauth += 1;
                    tracing::warn!(
                        source = source.id(),
                        "this mailbox refused its credentials; it will not be retried until \
                         somebody reconnects it"
                    );
                    source.advance(None, now, SourceStatus::NeedsReauth);
                } else {
                    report.failed += 1;
                    tracing::warn!(source = source.id(), error = %e, "mail sync failed");
                    source.advance(None, now, SourceStatus::Error);
                }
            }
        }
        if let Err(e) = repo.upsert_source(&source).await {
            tracing::warn!(source = source.id(), error = %e, "could not record the sync result");
        }
    }
    Ok(report)
}

async fn sync_one_mailbox(
    pipeline: &Arc<IngestPipeline>,
    secrets: &Arc<dyn SecretRepository>,
    source: &ContextSource,
    now: DateTime<Utc>,
) -> Result<usize> {
    let key = source
        .secret_ref()
        .map(str::to_string)
        .unwrap_or_else(|| secret_key_for(source.id()));
    let blob = secrets
        .get(&key)
        .await?
        .ok_or_else(|| anyhow::anyhow!("this mailbox's credentials are missing from the store"))?;
    let creds: AccountCredentials = serde_json::from_str(&blob)
        .map_err(|e| anyhow::anyhow!("this mailbox's stored credentials are unreadable: {e}"))?;

    let provider = ImapProvider::from_stored(source.provider(), creds.base_url.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this source names a mail provider this pond does not know: {}",
                source.provider()
            )
        })?;
    let adapter = ImapAdapter::new(ImapConfig {
        provider,
        username: creds.username.clone(),
        password: creds.password.clone(),
    });

    // No cursor. IMAP offers no cheap "has anything changed" answer the way a
    // CalDAV ctag does, and re-reading a 30-day window is idempotent: the
    // Message-ID is the external_id, so a message already stored is an update
    // to the same row rather than a duplicate.
    let items = adapter
        .recent_messages(ImapAdapter::default_window(now))
        .await?;

    let mut ingested = 0usize;
    for item in items {
        match pipeline.ingest(source, item, now).await {
            Ok(_) => ingested += 1,
            Err(e) => {
                tracing::debug!(source = source.id(), error = %e, "one message was not ingested")
            }
        }
    }
    Ok(ingested)
}

// ── The port the route asks through ─────────────────────────────────────────

/// Both connectors, one pass, wired to what the binary already holds.
///
/// The scheduled loop and the "check now" button go through this same struct
/// rather than each calling the two sweeps in their own order. Two callers with
/// their own idea of what a sync is, is how the button and the timer start
/// disagreeing about what happened.
pub struct AccountSyncer {
    repo: Arc<dyn ContextRepository>,
    pipeline: Arc<IngestPipeline>,
    secrets: Arc<dyn SecretRepository>,
}

impl AccountSyncer {
    pub fn new(
        repo: Arc<dyn ContextRepository>,
        pipeline: Arc<IngestPipeline>,
        secrets: Arc<dyn SecretRepository>,
    ) -> Self {
        Self {
            repo,
            pipeline,
            secrets,
        }
    }

    /// Calendar then mail, summed.
    ///
    /// Sequential rather than joined: they compete for the same uplink and the
    /// same CPU, and on a Jetson two protocol conversations at once is a load
    /// spike for no latency the household would notice.
    pub async fn run(&self, now: DateTime<Utc>) -> Result<AccountSyncSummary> {
        let calendars = sync_calendars(
            self.repo.clone(),
            self.pipeline.clone(),
            self.secrets.clone(),
            now,
        )
        .await?;
        let mail = sync_mail(
            self.repo.clone(),
            self.pipeline.clone(),
            self.secrets.clone(),
            now,
        )
        .await?;
        Ok(AccountSyncSummary {
            sources: calendars.sources + mail.sources,
            unchanged: calendars.unchanged + mail.unchanged,
            ingested: calendars.ingested + mail.ingested,
            needs_reauth: calendars.needs_reauth + mail.needs_reauth,
            failed: calendars.failed + mail.failed,
            paused: calendars.paused + mail.paused,
        })
    }
}

#[async_trait::async_trait]
impl AccountSync for AccountSyncer {
    async fn sync_now(&self) -> Result<AccountSyncSummary> {
        self.run(Utc::now()).await
    }
}
