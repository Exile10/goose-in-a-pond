//! SQLite-backed [`ProposalRepository`] — PAI-7 P3.
//!
//! Proposals are rows in the `drafts` table (migration 0041), not a table of
//! their own. See the port's module docs for why, and 0041's header for what
//! the three added columns are worth.
//!
//! # The one thing to understand before editing a query here
//!
//! **Every read filters expiry in SQL.** PAI-7 invariant 7 is not enforced by a
//! sweeper: [`expire_due`](ProposalRepository::expire_due) exists, nothing calls
//! it yet (PAI-7 P4 owns the loop), and the invariant holds anyway because a
//! proposal past its `expires_at` is not returned by anything here. If you find
//! yourself adding a read that skips the filter, you are removing the
//! invariant, not optimising a query.
//!
//! The comparison is `datetime(expires_at) > datetime(?)` rather than a string
//! compare. That is what makes it robust to the two RFC 3339 spellings of the
//! same instant (`...Z` and `...+00:00`, and `created_at` on this table is
//! already written in the second), and it fails closed: `datetime()` returns
//! NULL for anything it cannot parse, `NULL > x` is NULL, and NULL is not true,
//! so an unreadable expiry means expired rather than immortal.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::domain::proposal::{
    Proposal, ProposalAudience, ProposalPayload, PROPOSAL_DRAFT_KIND, PROPOSAL_ORIGIN,
    PROPOSAL_SESSION_ID,
};
use pond_core::user_data::ports::proposal::ProposalRepository;
use sqlx::{Pool, Sqlite};

/// The columns a proposal is rebuilt from. Stated once so a column added to one
/// query and not the others shifts a tuple field at compile time rather than at
/// runtime -- the same reason `sqlite_draft.rs` has `DRAFT_COLUMNS`.
const PROPOSAL_COLUMNS: &str = "id, profile_id, payload, rationale, created_at, expires_at";

/// `(id, profile_id, payload, rationale, created_at, expires_at)`.
type ProposalRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    Option<String>,
);

/// The `WHERE` every read shares: proactive, pending, and still live.
///
/// One constant rather than three copies, because a read that drops a clause is
/// exactly the defect this file is guarding against and three copies is how
/// that happens. The `?` is the caller's `now`.
const LIVE_PREDICATE: &str = "origin = ? AND status = 'pending' \
     AND expires_at IS NOT NULL AND datetime(expires_at) > datetime(?)";

pub struct SqliteProposalRepository {
    pool: Pool<Sqlite>,
}

impl SqliteProposalRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

/// The wire format for the two timestamp columns this file writes.
///
/// Seconds precision, UTC, `Z`-suffixed. Pinned by
/// `the_stored_expiry_format_is_the_one_sql_compares`, because 0041's triggers
/// compare these values with `datetime()` and a format change that `datetime()`
/// cannot parse would silently turn every proposal expired -- which narrows, so
/// nothing would break loudly.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("unreadable proposal timestamp: {raw}"))
}

/// Rebuild a proposal from its row, through the same validating constructor
/// production uses.
///
/// There is no "trust the database" path on purpose. A row whose rationale was
/// emptied out of band, whose confidence is 9.0, or whose profile id was
/// released by 0038's profile-delete trigger does not become a `Proposal` -- it
/// becomes an error, and every caller here turns that into "no proposal". On
/// failure, access narrows.
fn row_to_proposal(row: ProposalRow) -> Result<Proposal> {
    let (id, profile_id, payload, rationale, created_at, expires_at) = row;

    let audience = ProposalAudience::for_member(profile_id.unwrap_or_default())?;
    let payload: ProposalPayload = serde_json::from_str(&payload)
        .with_context(|| format!("unreadable proposal payload for {id}"))?;
    let expires_at = expires_at.ok_or_else(|| anyhow::anyhow!("proposal {id} has no expiry"))?;

    Ok(Proposal::from_parts(
        id,
        payload.trigger,
        rationale.unwrap_or_default(),
        payload.proposed_action,
        audience,
        payload.confidence,
        parse_ts(&created_at)?,
        parse_ts(&expires_at)?,
    )?)
}

#[async_trait]
impl ProposalRepository for SqliteProposalRepository {
    async fn save(&self, proposal: &Proposal) -> Result<()> {
        let payload = serde_json::to_string(&proposal.payload())?;
        sqlx::query(
            "INSERT INTO drafts \
             (id, session_id, profile_id, identification_source, kind, summary, payload, \
              status, created_at, origin, expires_at, rationale) \
             VALUES (?, ?, ?, NULL, ?, ?, ?, 'pending', ?, ?, ?, ?)",
        )
        .bind(proposal.id())
        // A proposal has no engine session. The sentinel is namespaced so it
        // cannot collide with one -- `list_drafts` scopes by session, and a
        // collision would put a proposal in a stranger's draft list.
        .bind(PROPOSAL_SESSION_ID)
        // Invariant 4, at the row level: the audience is a profile id and there
        // is no value of `ProposalAudience` that means "everybody".
        .bind(proposal.audience().profile_id())
        .bind(PROPOSAL_DRAFT_KIND)
        .bind(proposal.summary())
        .bind(payload)
        .bind(sql_ts(proposal.created_at()))
        .bind(PROPOSAL_ORIGIN)
        .bind(sql_ts(proposal.expires_at()))
        .bind(proposal.rationale())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_live_for(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Proposal>> {
        let sql = format!(
            "SELECT {PROPOSAL_COLUMNS} FROM drafts \
             WHERE {LIVE_PREDICATE} AND profile_id = ? ORDER BY created_at DESC"
        );
        let rows: Vec<ProposalRow> = sqlx::query_as(&sql)
            .bind(PROPOSAL_ORIGIN)
            .bind(sql_ts(now))
            .bind(profile_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .filter_map(|row| match row_to_proposal(row) {
                Ok(p) => Some(p),
                Err(e) => {
                    // Skipped, not surfaced: one unreadable row must not hide
                    // the member's other proposals, and showing it is not an
                    // option -- it failed the constructor.
                    tracing::warn!(
                        target: "giap::trace",
                        kind = "proposal_row_unreadable",
                        error = %e,
                        "a stored proposal did not survive validation and was not shown"
                    );
                    None
                }
            })
            .collect())
    }

    async fn get_live(&self, id: &str, now: DateTime<Utc>) -> Result<Option<Proposal>> {
        let sql =
            format!("SELECT {PROPOSAL_COLUMNS} FROM drafts WHERE {LIVE_PREDICATE} AND id = ?");
        let row: Option<ProposalRow> = sqlx::query_as(&sql)
            .bind(PROPOSAL_ORIGIN)
            .bind(sql_ts(now))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some(r) => Ok(Some(row_to_proposal(r)?)),
            None => Ok(None),
        }
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64> {
        // The complement of LIVE_PREDICATE's expiry half, written out rather
        // than negated: a proposal with no expiry, or with one SQLite cannot
        // read, is due. Both are unreachable through `save`, and both must
        // resolve to "expired" rather than "immortal" if they ever happen.
        let result = sqlx::query(
            "UPDATE drafts SET status = 'expired' \
             WHERE origin = ? AND status = 'pending' \
             AND (expires_at IS NULL OR datetime(expires_at) IS NULL \
                  OR datetime(expires_at) <= datetime(?))",
        )
        .bind(PROPOSAL_ORIGIN)
        .bind(sql_ts(now))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use pond_core::user_data::domain::proposal::BusEventRef;
    use pond_core::user_data::domain::schedule::TaskKind;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_785_000_000 + secs, 0).unwrap()
    }

    fn proposal(id: &str, owner: &str, created: DateTime<Utc>, ttl: Duration) -> Proposal {
        Proposal::expiring_after(
            id,
            BusEventRef::new(
                "camera",
                Some("front-door".into()),
                Some("person".into()),
                created,
            )
            .unwrap(),
            "a parcel has been at the door for an hour",
            TaskKind::AgentPrompt {
                prompt: "remind me about the parcel".into(),
            },
            ProposalAudience::for_member(owner).unwrap(),
            0.72,
            created,
            ttl,
        )
        .unwrap()
    }

    async fn db() -> (tempfile::TempDir, Pool<Sqlite>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        for id in ["liz", "jerry"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        (tmp, pool)
    }

    #[tokio::test]
    async fn a_proposal_round_trips_through_the_drafts_table() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        let p = proposal("prop-1", "liz", created, Duration::hours(2));
        repo.save(&p).await.unwrap();

        let back = repo.get_live("prop-1", created).await.unwrap().unwrap();
        assert_eq!(back, p, "every field must survive the row, not just the id");
        assert_eq!(
            back.rationale(),
            "a parcel has been at the door for an hour"
        );
        assert_eq!(back.audience().profile_id(), "liz");

        // It really is a draft row, which is the whole point of 3.2.
        let (kind, session, origin): (String, String, String) =
            sqlx::query_as("SELECT kind, session_id, origin FROM drafts WHERE id = 'prop-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, PROPOSAL_DRAFT_KIND);
        assert_eq!(session, PROPOSAL_SESSION_ID);
        assert_eq!(origin, PROPOSAL_ORIGIN);
    }

    /// Invariant 4. There is no method that lists every proposal, and the one
    /// that lists a member's does not leak another member's.
    #[tokio::test]
    async fn a_proposal_is_visible_only_to_the_member_it_is_addressed_to() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool);
        let created = at(0);
        repo.save(&proposal("for-liz", "liz", created, Duration::hours(2)))
            .await
            .unwrap();
        repo.save(&proposal("for-jerry", "jerry", created, Duration::hours(2)))
            .await
            .unwrap();

        let hers = repo.list_live_for("liz", created).await.unwrap();
        assert_eq!(hers.len(), 1);
        assert_eq!(hers[0].id(), "for-liz");
        assert!(repo
            .list_live_for("nobody", created)
            .await
            .unwrap()
            .is_empty());
    }

    /// Invariant 7's load-bearing half: the READ refuses an expired proposal,
    /// with no sweeper having run. Delete the expiry clause from
    /// `LIVE_PREDICATE` and this fails.
    #[tokio::test]
    async fn an_expired_proposal_is_not_returned_even_though_nothing_swept_it() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("prop-1", "liz", created, Duration::hours(1)))
            .await
            .unwrap();

        let just_before = created + Duration::minutes(59);
        assert!(repo
            .get_live("prop-1", just_before)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            repo.list_live_for("liz", just_before).await.unwrap().len(),
            1
        );

        let after = created + Duration::hours(1) + Duration::seconds(1);
        assert!(
            repo.get_live("prop-1", after).await.unwrap().is_none(),
            "an expired proposal must not be readable: invariant 7 does not depend on a sweeper"
        );
        assert!(repo.list_live_for("liz", after).await.unwrap().is_empty());

        // And the row is still pending -- proving the refusal came from the
        // read filter and not from a status that something had already changed.
        let status: String = sqlx::query_scalar("SELECT status FROM drafts WHERE id = 'prop-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "pending");
    }

    /// The format contract between this file and 0041's triggers. If `sql_ts`
    /// ever emits something `datetime()` cannot parse, every proposal silently
    /// becomes expired -- a narrowing failure, so nothing else would notice.
    #[tokio::test]
    async fn the_stored_expiry_format_is_the_one_sql_compares() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("prop-1", "liz", created, Duration::hours(2)))
            .await
            .unwrap();

        let parsed: Option<String> =
            sqlx::query_scalar("SELECT datetime(expires_at) FROM drafts WHERE id = 'prop-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            parsed.is_some(),
            "SQLite cannot parse the expiry this repository writes, so 0041's triggers \
             and every read filter compare against NULL"
        );
        // Vacuity control: the same assertion over a value SQLite genuinely
        // cannot read must fail, or `datetime()` is not discriminating here.
        let nonsense: Option<String> = sqlx::query_scalar("SELECT datetime('not-a-time')")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(nonsense.is_none());
    }

    #[tokio::test]
    async fn sweeping_gives_a_due_proposal_a_terminal_status_and_leaves_live_ones_alone() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("short", "liz", created, Duration::hours(1)))
            .await
            .unwrap();
        repo.save(&proposal("long", "liz", created, Duration::hours(6)))
            .await
            .unwrap();

        let swept = repo.expire_due(created + Duration::hours(2)).await.unwrap();
        assert_eq!(swept, 1);

        let statuses: Vec<(String, String)> =
            sqlx::query_as("SELECT id, status FROM drafts ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            vec![
                ("long".to_string(), "pending".to_string()),
                ("short".to_string(), "expired".to_string())
            ]
        );
    }

    /// 0041's insert trigger, against a real migrated database. The Rust
    /// constructor refuses a blank rationale; this is the layer under it, for a
    /// row that reaches the table another way.
    #[tokio::test]
    async fn sqlite_refuses_to_store_a_proposal_without_a_rationale() {
        let (_tmp, pool) = db().await;
        for blank in [None, Some(""), Some("   ")] {
            let err = sqlx::query(
                "INSERT INTO drafts (id, session_id, kind, summary, payload, status, \
                 created_at, origin, expires_at, rationale) \
                 VALUES ('x', 'giap:proactive', 'proposal', 's', '{}', 'pending', \
                 '2026-08-10T00:00:00Z', 'proactive', '2026-08-10T06:00:00Z', ?)",
            )
            .bind(blank)
            .execute(&pool)
            .await
            .expect_err("a rationale-less proposal must not be storable");
            assert!(
                err.to_string().contains("must carry a rationale"),
                "the refusal must name the defect, got: {err}"
            );
        }

        // Vacuity control: the same INSERT with a rationale succeeds, so the
        // three refusals above are about the rationale and not about the shape
        // of the statement.
        sqlx::query(
            "INSERT INTO drafts (id, session_id, kind, summary, payload, status, \
             created_at, origin, expires_at, rationale) \
             VALUES ('x', 'giap:proactive', 'proposal', 's', '{}', 'pending', \
             '2026-08-10T00:00:00Z', 'proactive', '2026-08-10T06:00:00Z', 'because')",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    /// 0041's update trigger. An expired row may not be approved, whoever asks
    /// and through whichever repository -- `DraftRepository::update_status`
    /// knows nothing about proposals and goes through this.
    #[tokio::test]
    async fn sqlite_refuses_to_approve_an_expired_row() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        // Expired an hour ago in wall-clock terms: the trigger compares against
        // SQLite's own `datetime('now')`, not against an injected clock.
        let created = Utc::now() - Duration::hours(3);
        repo.save(&proposal("stale", "liz", created, Duration::hours(2)))
            .await
            .unwrap();
        repo.save(&proposal("fresh", "liz", Utc::now(), Duration::hours(2)))
            .await
            .unwrap();

        let err = sqlx::query("UPDATE drafts SET status = 'approved' WHERE id = 'stale'")
            .execute(&pool)
            .await
            .expect_err("an expired proposal must not be approvable");
        assert!(
            err.to_string().contains("expired"),
            "the refusal must name the defect, got: {err}"
        );

        // Rejecting one must stay possible -- 3.5's feedback loop writes a
        // rejection back as a memory, and 0038's profile-delete trigger sets
        // 'expired' on rows this trigger must not block.
        sqlx::query("UPDATE drafts SET status = 'rejected' WHERE id = 'stale'")
            .execute(&pool)
            .await
            .unwrap();

        // Vacuity control: a live proposal approves cleanly, so the refusal
        // above is about expiry and not about the UPDATE itself.
        sqlx::query("UPDATE drafts SET status = 'approved' WHERE id = 'fresh'")
            .execute(&pool)
            .await
            .unwrap();
    }

    /// 0038's profile-delete trigger already covers proposals, because a
    /// proposal's audience is stored in `drafts.profile_id`. Worth a test
    /// rather than a claim: a departed member's pending suggestion must not
    /// outlive them.
    #[tokio::test]
    async fn a_departed_members_proposal_is_expired_and_released() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("hers", "liz", created, Duration::hours(6)))
            .await
            .unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .unwrap();

        assert!(repo.get_live("hers", created).await.unwrap().is_none());
        let (status, owner): (String, Option<String>) =
            sqlx::query_as("SELECT status, profile_id FROM drafts WHERE id = 'hers'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "expired");
        assert_eq!(owner, None);
    }

    /// The read path goes through the same validating constructor production
    /// does, so a row that could not have been built cannot be shown either.
    #[tokio::test]
    async fn a_row_that_fails_validation_is_not_returned() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("prop-1", "liz", created, Duration::hours(2)))
            .await
            .unwrap();
        // A confidence no constructor would accept, written straight into the
        // payload column.
        sqlx::query("UPDATE drafts SET payload = ? WHERE id = 'prop-1'")
            .bind(r#"{"trigger":{"kind":"camera","observed_at":"2026-08-10T00:00:00Z"},"proposed_action":{"type":"agent_prompt","prompt":"x"},"confidence":9.0}"#)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            repo.get_live("prop-1", created).await.is_err(),
            "get_live must not hand back a proposal that failed validation"
        );
        assert!(
            repo.list_live_for("liz", created).await.unwrap().is_empty(),
            "list_live_for must skip a row that failed validation"
        );
    }
}
