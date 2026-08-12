-- PAI-7 P3: proactive proposals live in the drafts table.
--
-- A proactive impulse does not perform an action; it produces a proposal that
-- one household member approves or rejects. Section 3.2 of
-- docs/architecture/pai/07-proactive-intelligence.md puts proposals in the
-- draft system that already exists rather than beside it, so a proactive
-- suggestion inherits an approval flow that is already built, already
-- understood and already trusted. That decision is why this migration adds
-- columns instead of a table.
--
-- Three columns, and each one earns its place by being something SQL has to be
-- able to read. Everything else about a proposal (its trigger, its proposed
-- action, its confidence) rides in `payload` as JSON, because nothing queries
-- on it.
--
-- What happens to rows that already exist, stated rather than left implicit:
-- ALTER TABLE ADD COLUMN gives every existing row NULL in all three, and NULL
-- is the right reading for each. `origin IS NULL` means user-staged, which
-- every draft in every pond is today. `expires_at IS NULL` means "does not
-- expire", which is exactly the behaviour drafts have had since 0018. There is
-- no UPDATE in this migration and none is needed -- unlike 0038, which had to
-- expire every pending row because it was changing what an existing row MEANT.
-- This one only adds vocabulary.

ALTER TABLE drafts ADD COLUMN origin TEXT;
-- 'proactive' for a proposal; NULL for a draft a turn staged. Not 'user' for
-- the legacy rows: writing a value into old rows would claim we know how they
-- were made, and NULL already says the only true thing, which is that nothing
-- proactive existed when they were written.

ALTER TABLE drafts ADD COLUMN expires_at TEXT;
-- RFC 3339, seconds precision, UTC, 'Z'-suffixed -- written by
-- SqliteProposalRepository via to_rfc3339_opts(Secs, true). The format matters
-- because the triggers below compare it, so it is pinned by a test
-- (`the_stored_expiry_format_is_the_one_sql_compares`) rather than by this
-- comment. NULL means the row never expires.
--
-- PAI-7 invariant 7: "a proposal expires. An assistant that surfaces
-- yesterday's suggestion has failed twice." An expiry that only a sweeper
-- honours stops holding the day the sweeper fails to start, so this column is
-- read on the SELECT path and enforced on the UPDATE path below. No background
-- task is required for either.

ALTER TABLE drafts ADD COLUMN rationale TEXT;
-- Why GIAP thinks this matters. PAI-7 invariant 2, and section 3.2's argument
-- for it: an assistant that says "you asked me to watch for this, and the
-- delivery window closes at six" is helpful; one that says "I did a thing" is
-- alarming.
--
-- It is a COLUMN rather than a field inside `payload` for one reason: the
-- database can only refuse an empty rationale it can see. See the trigger
-- below. NULL for user-staged drafts, which have a summary and no reason to
-- justify themselves.

CREATE INDEX IF NOT EXISTS idx_drafts_origin_profile_status
    ON drafts(origin, profile_id, status);
-- The proposal read path is "live, pending, addressed to this member", which
-- is exactly this prefix. It does not subsume idx_drafts_profile_status from
-- 0038, whose leading column is profile_id.

-- ── Invariant 2, at the storage layer ──────────────────────────────────────
--
-- Proposal::from_parts refuses a blank rationale and `Proposal` derives no
-- Deserialize, so there is no serde door past it. This is the layer under
-- that: a row that reaches the table by any other route still cannot be a
-- rationale-less proposal.
--
-- It fires on UPDATE as well as INSERT, and on the whole row rather than
-- `UPDATE OF rationale`, deliberately. `BEFORE UPDATE OF rationale` only fires
-- when that column is in the SET list, so `UPDATE drafts SET origin =
-- 'proactive'` against a row with no rationale would walk straight past it.
CREATE TRIGGER IF NOT EXISTS trg_drafts_proposal_needs_a_rationale_on_insert
BEFORE INSERT ON drafts
FOR EACH ROW
WHEN NEW.origin = 'proactive'
 AND (NEW.rationale IS NULL OR trim(NEW.rationale) = '')
BEGIN
    SELECT RAISE(ABORT, 'a proposal must carry a rationale');
END;

CREATE TRIGGER IF NOT EXISTS trg_drafts_proposal_needs_a_rationale_on_update
BEFORE UPDATE ON drafts
FOR EACH ROW
WHEN NEW.origin = 'proactive'
 AND (NEW.rationale IS NULL OR trim(NEW.rationale) = '')
BEGIN
    SELECT RAISE(ABORT, 'a proposal must carry a rationale');
END;

-- ── Invariant 7, at the storage layer ──────────────────────────────────────
--
-- An expired row may not be approved. This is the half the read filter cannot
-- do: approve_draft looks a row up by id through DraftRepository::get and
-- flips its status, and it knew nothing about expiry until the Rust-side guard
-- that lands with this migration. Two layers rather than one because the Rust
-- guard is a branch with a call site, and a call site can be deleted; this one
-- cannot be deleted without a migration.
--
-- Three details that decide whether it holds:
--
--   * It keys on `expires_at IS NOT NULL`, not on `origin = 'proactive'`, so
--     the day save_draft grows a TTL it is already covered.
--   * An UNPARSEABLE expires_at makes datetime() return NULL, and the WHEN
--     clause treats that as expired. On failure, access narrows.
--   * It fires only for the 'approved' transition. Rejecting an expired
--     proposal must stay possible: PAI-7 section 3.5 writes rejections back as
--     memories, and 0038's profile-delete trigger flips rows to 'expired',
--     which this must not abort.
CREATE TRIGGER IF NOT EXISTS trg_drafts_no_approving_an_expired_row
BEFORE UPDATE OF status ON drafts
FOR EACH ROW
WHEN NEW.status = 'approved'
 AND OLD.expires_at IS NOT NULL
 AND (datetime(OLD.expires_at) IS NULL
      OR datetime(OLD.expires_at) <= datetime('now'))
BEGIN
    SELECT RAISE(ABORT, 'this proposal has expired and can no longer be approved');
END;
