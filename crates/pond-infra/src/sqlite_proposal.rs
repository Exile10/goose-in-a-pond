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

    /// A raw INSERT into `drafts`, for the triggers -- the layer that has to
    /// hold for a row that reaches the table some way other than through
    /// [`SqliteProposalRepository::save`].
    ///
    /// It BINDS the three constants rather than repeating their values as SQL
    /// literals, and that is the whole reason it exists as a helper. The
    /// trigger tests used to write `'proactive'`, `'proposal'` and
    /// `'giap:proactive'` by hand, which tested 0041 against a shape production
    /// does not produce: changing `PROPOSAL_ORIGIN` disabled both rationale
    /// triggers for every row the repository writes, and pond-core and
    /// pond-infra both stayed green. See
    /// `the_origin_constant_is_the_literal_the_migrations_hardcode` for the
    /// other half of that tie.
    async fn insert_raw(
        pool: &Pool<Sqlite>,
        id: &str,
        expires_at: Option<&str>,
        rationale: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO drafts (id, session_id, kind, summary, payload, status, \
             created_at, origin, expires_at, rationale) \
             VALUES (?, ?, ?, 's', '{}', 'pending', '2026-08-10T00:00:00Z', ?, ?, ?)",
        )
        .bind(id)
        .bind(PROPOSAL_SESSION_ID)
        .bind(PROPOSAL_DRAFT_KIND)
        .bind(PROPOSAL_ORIGIN)
        .bind(expires_at)
        .bind(rationale)
        .execute(pool)
        .await
        .map(|_| ())
    }

    /// A live expiry for [`insert_raw`], in the spelling `sql_ts` writes.
    fn raw_expiry() -> String {
        sql_ts(Utc::now() + Duration::hours(2))
    }

    const MIGRATION_0041: &str = include_str!("../migrations/system/0041_proposals.sql");
    const MIGRATION_0042: &str = include_str!("../migrations/system/0042_proposal_expiry.sql");

    /// 0042's two triggers, named once so the upgrade test drops exactly what
    /// the file creates.
    const EXPIRY_TRIGGERS: [&str; 2] = [
        "trg_drafts_proposal_needs_an_expiry_on_insert",
        "trg_drafts_proposal_needs_an_expiry_on_update",
    ];

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

    /// The other half of `LIVE_PREDICATE`, which had no guard at all: widening
    /// `status = 'pending'` to admit `'approved'` and `'rejected'` left the
    /// whole pond-infra suite green, so the port's own promise that `get_live`
    /// "returns `Ok(None)` for expired, decided and absent alike" was prose
    /// with nothing behind it.
    ///
    /// A decided proposal resurfacing is invariant 7's other failure mode --
    /// "an assistant that surfaces yesterday's suggestion has failed twice" --
    /// and it is the worse one, because the row is not merely stale, it is one
    /// the member already answered.
    ///
    /// Driven over both terminal decisions and over `'expired'`, and with the
    /// read clock held at creation time throughout, so nothing here can pass
    /// because the expiry clause caught it instead.
    ///
    /// Creation is `Utc::now()` rather than the frozen `at(0)`, and that is not
    /// cosmetic: 0041's approve trigger compares against SQLite's own
    /// `datetime('now')`, not against the injected clock the read path uses, so
    /// a fixture created in 2026-07 is already unapprovable and the `'approved'`
    /// arm would abort before this test ever reached its assertion. The two
    /// clocks are deliberately different -- a trigger cannot be handed one --
    /// and a test that straddles both has to satisfy each.
    #[tokio::test]
    async fn a_proposal_the_member_already_decided_is_never_shown_again() {
        for decided in ["approved", "rejected", "expired"] {
            let (_tmp, pool) = db().await;
            let repo = SqliteProposalRepository::new(pool.clone());
            let created = Utc::now();
            repo.save(&proposal("decided", "liz", created, Duration::hours(6)))
                .await
                .unwrap();
            repo.save(&proposal(
                "still-pending",
                "liz",
                created,
                Duration::hours(6),
            ))
            .await
            .unwrap();

            // Vacuity control, taken BEFORE the status moves: both are visible
            // while both are pending, so the assertions below are about the
            // status filter and not about a fixture that was never readable.
            assert_eq!(
                repo.list_live_for("liz", created).await.unwrap().len(),
                2,
                "{decided}: both proposals must be live before one is decided, or this \
                 test proves nothing"
            );

            sqlx::query("UPDATE drafts SET status = ? WHERE id = 'decided'")
                .bind(decided)
                .execute(&pool)
                .await
                .unwrap();

            assert!(
                repo.get_live("decided", created).await.unwrap().is_none(),
                "a proposal already marked '{decided}' must not be readable: invariant 7 \
                 is broken by re-surfacing a suggestion the member has answered, not only \
                 by re-surfacing a stale one"
            );
            let live = repo.list_live_for("liz", created).await.unwrap();
            assert_eq!(
                live.iter().map(|p| p.id()).collect::<Vec<_>>(),
                vec!["still-pending"],
                "list_live_for must drop the '{decided}' proposal and keep the pending one"
            );
        }
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
        let expiry = raw_expiry();
        for blank in [None, Some(""), Some("   ")] {
            let err = insert_raw(&pool, "x", Some(expiry.as_str()), blank)
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
        insert_raw(&pool, "x", Some(expiry.as_str()), Some("because"))
            .await
            .unwrap();
    }

    /// 0042. Invariant 7 at the storage layer, which 0041 gave to the rationale
    /// and not to the expiry: a row with `origin = 'proactive'` and a NULL
    /// `expires_at` was storable, and 0041's approve trigger keys on
    /// `OLD.expires_at IS NOT NULL`, so that row was approvable forever.
    ///
    /// Both doors are driven. The UPDATE one is the half that does the work: an
    /// INSERT-only trigger is walked straight past by
    /// `UPDATE drafts SET origin = 'proactive'` against a legacy row, and every
    /// legacy row has no expiry -- which is the argument 0041 makes for its own
    /// pair of rationale triggers.
    #[tokio::test]
    async fn sqlite_refuses_to_store_a_proposal_without_an_expiry() {
        let (_tmp, pool) = db().await;

        let err = insert_raw(&pool, "no-expiry", None, Some("because"))
            .await
            .expect_err("a proposal with no expiry must not be storable: it can never expire");
        assert!(
            err.to_string().contains("must carry an expiry"),
            "the refusal must name the defect, got: {err}"
        );

        // The other door: a legacy draft -- no origin, no expiry, which is every
        // draft in every pond today -- turned proactive by an UPDATE.
        sqlx::query(
            "INSERT INTO drafts (id, session_id, kind, summary, payload, status, created_at) \
             VALUES ('legacy', 'sess-a', 'shell_command', 's', '{}', 'pending', \
             '2026-08-10T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("a legacy draft is exactly what save_draft writes and must stay storable");

        // The rationale is supplied in the same statement on purpose: without
        // it 0041's rationale trigger also matches, SQLite does not promise
        // which of two eligible triggers aborts first, and this test would then
        // pass or fail on the message of a rule it is not about.
        let err = sqlx::query(
            "UPDATE drafts SET origin = ?, rationale = 'because' WHERE id = 'legacy'",
        )
        .bind(PROPOSAL_ORIGIN)
        .execute(&pool)
        .await
        .expect_err(
            "relabelling an expiry-less row as proactive must not mint an immortal proposal",
        );
        assert!(
            err.to_string().contains("must carry an expiry"),
            "the refusal must name the defect, got: {err}"
        );

        // Vacuity control: the same UPDATE that also supplies an expiry and a
        // rationale succeeds, so the refusals above are about the missing
        // expiry and not about relabelling a row at all.
        sqlx::query("UPDATE drafts SET origin = ?, expires_at = ?, rationale = 'because' WHERE id = 'legacy'")
            .bind(PROPOSAL_ORIGIN)
            .bind(raw_expiry())
            .execute(&pool)
            .await
            .unwrap();
    }

    /// A migration must work against a database that ALREADY HAS ROWS, and the
    /// row that decides it here is one only an UPGRADE can be holding:
    /// proactive, with no expiry. A fresh database cannot produce it, because
    /// every migration is applied at once and 0042's own INSERT trigger refuses
    /// it -- so a test that only ever sees a fresh database cannot see this at
    /// all.
    ///
    /// The defect it guards is not the refusal, it is the FREEZE. 0042's rule
    /// is stated over a row's state rather than over the transition into it, so
    /// without the data fix at the top of the file a pre-existing row in the
    /// forbidden state can never be updated again -- not rejected, not swept,
    /// and not released by the two UPDATEs inside 0038's `BEFORE DELETE ON
    /// profiles` trigger. That last one aborts the DELETE, so removing a
    /// household member fails on a row nobody can see. Confirmed by removing
    /// the data fix: this test's final `DELETE FROM profiles` then fails with
    /// SQLITE error 19.
    ///
    /// The migration is re-run from the FILE's own text, not from a copy of its
    /// statements. A fixture that restated the fix would be testing a migration
    /// that does not exist.
    #[tokio::test]
    async fn migration_0042_does_not_freeze_a_row_that_predates_it() {
        let (_tmp, pool) = db().await;

        // Rewind to the 0041 world.
        for trigger in EXPIRY_TRIGGERS {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("0042 must have created {trigger}: {e}"));
        }
        insert_raw(&pool, "immortal", None, Some("because"))
            .await
            .expect("with 0042's triggers dropped, the pre-0042 row must be writable");
        sqlx::query("UPDATE drafts SET profile_id = 'liz' WHERE id = 'immortal'")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(MIGRATION_0042)
            .execute(&pool)
            .await
            .expect("0042 must apply to a database that already holds rows");

        let (status, expires_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, expires_at FROM drafts WHERE id = 'immortal'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "expired");
        assert!(
            expires_at.is_some(),
            "the row must come out of the upgrade with an expiry, or every later write to it \
             aborts"
        );

        // Vacuity control: the triggers really were re-created, so the write
        // below succeeds because the row was fixed and not because the rule
        // went missing with the DROP.
        let err = insert_raw(&pool, "another", None, Some("because"))
            .await
            .expect_err("0042's INSERT trigger must be in force after the file is re-run");
        assert!(
            err.to_string().contains("must carry an expiry"),
            "got: {err}"
        );

        // The half that matters: a household member can still be removed.
        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .expect(
                "0038's profile-delete trigger updates this row, so a frozen row blocks the \
                 deletion of a household member entirely",
            );
    }

    /// The workspace's `crates/` directory, for the reachability guard below.
    const CRATES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

    /// Every `.rs` file under `crates/`, as `(path, source)`.
    fn workspace_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(e) => panic!("cannot read {}: {e}", dir.display()),
            };
            for entry in entries {
                let path = entry.expect("read dir entry").path();
                if path.is_dir() {
                    // `target` can appear under a crate on some layouts and is
                    // generated, not authored.
                    if path.file_name().and_then(|n| n.to_str()) != Some("target") {
                        walk(&path, out);
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    let src = std::fs::read_to_string(&path)
                        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
                    out.push((path.display().to_string(), src));
                }
            }
        }
        let mut out = Vec::new();
        walk(std::path::Path::new(CRATES_DIR), &mut out);
        out
    }

    /// PAI-7 P3a's central claim, re-proved on every run instead of asserted in
    /// prose: **nothing outside this file constructs a proposal repository.**
    ///
    /// The reason this is a test rather than a sentence in the stamp is that
    /// the usual tripwire cannot fire. Every symbol in the proposal domain is
    /// `pub` in a library crate, so `dead_code` says nothing about a type no
    /// production path can reach -- which is exactly how PAI-1 P5 was recorded
    /// as landed while being inert on every install, and how PAI-6 P1's clamp
    /// shipped with its one call site missing. An unreachable mechanism has to
    /// be claimed positively, the way `UNGATED_SENDERS` claims the egress
    /// partition, or the next reader assumes reachability from the fact that it
    /// compiles.
    ///
    /// **This test is meant to fail one day.** When PAI-7 P4 adds
    /// `SqliteProposalRepository::new(db.system.clone())` to `serve()` in
    /// `crates/pond-server/src/main.rs`, delete it, and update the P3a stamp in
    /// `docs/architecture/pai/07-proactive-intelligence.md` section 3.2 and the
    /// ledger in the same change. Deleting it silently is the failure it exists
    /// to prevent, in the other direction.
    #[test]
    fn nothing_outside_this_file_constructs_a_proposal_repository_yet() {
        let sources = workspace_sources();
        let this_file = "sqlite_proposal.rs";

        // The vacuity controls come first, because a walk that found nothing
        // proves nothing and would report the happiest possible answer.
        assert!(
            sources.len() > 100,
            "the source walk found only {} files under {CRATES_DIR}: it is looking in the \
             wrong place, and the assertion below would pass against an empty set",
            sources.len()
        );
        let sibling_construction: Vec<&str> = sources
            .iter()
            .filter(|(path, src)| {
                !path.ends_with(this_file) && src.contains("SqliteDraftRepository::new(")
            })
            .map(|(path, _)| path.as_str())
            .collect();
        assert!(
            !sibling_construction.is_empty(),
            "the walk cannot see a construction site it is KNOWN to be able to see: \
             SqliteDraftRepository::new( is called from pond-server. Either the walk does not \
             reach other crates, or that wiring moved -- and until this control passes, the \
             assertion below is not evidence of anything"
        );

        let constructors: Vec<&str> = sources
            .iter()
            .filter(|(path, src)| {
                !path.ends_with(this_file) && src.contains("SqliteProposalRepository::new(")
            })
            .map(|(path, _)| path.as_str())
            .collect();
        assert!(
            constructors.is_empty(),
            "a proposal repository is now constructed outside its own module, in {constructors:?}. \
             That is good news and this test is the wrong shape for it: PAI-7 P3a is stamped \
             as domain-and-persistence-only in docs/architecture/pai/07-proactive-intelligence.md \
             section 3.2, on the strength of this assertion. Delete this test and correct the \
             stamp and the ledger in the same change."
        );
    }

    /// The SQL half of the `PROPOSAL_ORIGIN` contract, which nothing tied down.
    ///
    /// 0041's two rationale triggers and 0042's two expiry triggers each
    /// hardcode the literal `'proactive'` in a `WHEN` clause. Renaming the Rust
    /// constant therefore disables all four for every row this repository
    /// writes -- invariant 2's and invariant 7's storage layers stop applying,
    /// silently, with pond-core and pond-infra entirely green. The migration's
    /// own header says the constant "is load-bearing in SQL as well as in
    /// Rust", and that sentence was the only thing holding it.
    ///
    /// Asserted as two COUNTS rather than as a `contains`. A presence check
    /// passes while a fifth trigger keyed on some other origin value is added
    /// beside these, and it cannot tell "the constant moved" from "the
    /// migration moved" -- the generic count is the vacuity control for the
    /// bound one, and it fails first if the search has stopped describing the
    /// file.
    #[test]
    fn the_origin_constant_is_the_literal_the_migrations_hardcode() {
        for (name, sql, triggers) in [
            ("0041_proposals.sql", MIGRATION_0041, 2usize),
            ("0042_proposal_expiry.sql", MIGRATION_0042, 2usize),
        ] {
            let any = sql.matches("NEW.origin = '").count();
            assert_eq!(
                any, triggers,
                "{name} no longer has {triggers} triggers keyed on an origin literal, it has \
                 {any}. Either a trigger was added or removed -- in which case update this \
                 count -- or the WHEN clause was rewritten and the assertion below has \
                 stopped describing the file."
            );
            let bound = sql
                .matches(format!("NEW.origin = '{PROPOSAL_ORIGIN}'").as_str())
                .count();
            assert_eq!(
                bound, triggers,
                "{name} hardcodes an origin literal that PROPOSAL_ORIGIN ({PROPOSAL_ORIGIN:?}) \
                 no longer matches: {bound} of its {triggers} origin-keyed triggers agree with \
                 the constant. Every row SqliteProposalRepository::save writes would slip past \
                 the rest, so the storage layer under PAI-7 invariants 2 and 7 would stop \
                 applying without one test going red."
            );
        }
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
