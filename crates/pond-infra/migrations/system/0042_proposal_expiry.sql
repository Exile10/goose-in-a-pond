-- PAI-7 P3: give invariant 7 the storage layer invariant 2 already got.
--
-- 0041 made a rationale-less proposal unstorable, and argued for it like this:
-- "Proposal::from_parts refuses a blank rationale ... This is the layer under
-- that: a row that reaches the table by any other route still cannot be a
-- rationale-less proposal."
--
-- The same argument applies to the expiry and 0041 did not make it. A row with
-- origin = 'proactive' and a NULL expires_at was storable, and 0041's approve
-- trigger keys on `OLD.expires_at IS NOT NULL`, so that row was approvable
-- forever -- an immortal proposal, which is the exact thing invariant 7 exists
-- to forbid ("an assistant that surfaces yesterday's suggestion has failed
-- twice"). Verified with the sqlite3 CLI against a database with 0001-0041
-- applied: both the INSERT and the UPDATE to 'approved' succeeded.
--
-- It is unreachable through SqliteProposalRepository::save, which always binds
-- an expiry. That is the same defence 0041 declined to accept for the
-- rationale, and for the same reason: "the Rust constructor refuses it" is a
-- claim about one writer, and a trigger is a claim about the table. The Rust
-- guard is also a branch, and a branch has a call site that a later refactor
-- can delete; this cannot be deleted without a migration.
--
-- Scope, deliberately narrow: NULL only, not "unparseable". An expires_at
-- SQLite cannot read is ALREADY covered, because 0041's approve trigger treats
-- `datetime(OLD.expires_at) IS NULL` as expired and every read here filters the
-- same way. Refusing it at INSERT as well would turn a narrowing failure into a
-- loud one for a row that was never approvable anyway.
--
-- ── What happens to rows that already exist ────────────────────────────────
--
-- The set this can touch is empty in every pond that exists: every draft today
-- has `origin IS NULL`, because SqliteProposalRepository::save is the only
-- writer of that column and nothing constructs it (see the P3a stamp in
-- docs/architecture/pai/07-proactive-intelligence.md section 3.2). The UPDATE
-- below is still not optional, and I only found out why by running the upgrade
-- rather than reasoning about it.
--
-- The trigger pair is stated over a row's STATE, not over the transition into
-- it. So a row already in the forbidden state -- proactive with no expiry --
-- would be frozen by it: every subsequent UPDATE aborts, including
-- `status = 'rejected'`, including the sweep in expire_due, and including the
-- two UPDATEs inside 0038's `BEFORE DELETE ON profiles` trigger. That last one
-- is the serious half. It would abort the DELETE, so removing a household
-- member would fail with "a proposal must carry an expiry" and PAI-1's whole
-- deletion path would be blocked by a row nobody can see. Verified with the
-- sqlite3 CLI, twice: against a database with 0001-0041 applied I inserted
-- exactly such a row and then applied this file WITHOUT the UPDATE below.
-- `UPDATE drafts SET status='rejected'` failed with SQLITE error 19, and so did
-- `DELETE FROM profiles` once the row was addressed to a member. With the
-- UPDATE below in place, both succeed.
--
-- So: fix the data, then constrain it. `expires_at = created_at` is
-- deliberately born-expired -- the row is unreadable to every path here
-- (Proposal::from_parts refuses it, the read filter excludes it, 0041 refuses
-- the approval) while being a value the triggers below accept, so the row can
-- still be swept, rejected and released. The rationale COALESCE guards this
-- statement against 0041's own rationale-on-update trigger: such a row cannot
-- have a blank rationale today, and a migration that CAN abort is a server that
-- will not start, so the case that cannot happen is handled anyway.
UPDATE drafts
   SET expires_at = created_at,
       status = 'expired',
       rationale = COALESCE(
           NULLIF(trim(rationale), ''),
           'this proposal predates migration 0042 and carried no expiry'
       )
 WHERE origin = 'proactive'
   AND expires_at IS NULL;

CREATE TRIGGER IF NOT EXISTS trg_drafts_proposal_needs_an_expiry_on_insert
BEFORE INSERT ON drafts
FOR EACH ROW
WHEN NEW.origin = 'proactive' AND NEW.expires_at IS NULL
BEGIN
    SELECT RAISE(ABORT, 'a proposal must carry an expiry');
END;

-- The UPDATE companion, for the reason 0041 gives for its own pair, and here it
-- is the half that does the work: `UPDATE drafts SET origin = 'proactive'`
-- against one of the legacy rows -- all of which have no expiry -- would walk
-- straight past an INSERT-only trigger and mint exactly the immortal proposal
-- this migration is about.
--
-- On the whole row rather than `UPDATE OF expires_at`, for that same reason: a
-- BEFORE UPDATE OF trigger only fires when the named column is in the SET list.
-- Nothing legitimate is caught. `expire_due` and 0038's profile-delete trigger
-- both write `status` on rows whose expires_at is already non-NULL, and neither
-- writes `origin` at all.
CREATE TRIGGER IF NOT EXISTS trg_drafts_proposal_needs_an_expiry_on_update
BEFORE UPDATE ON drafts
FOR EACH ROW
WHEN NEW.origin = 'proactive' AND NEW.expires_at IS NULL
BEGIN
    SELECT RAISE(ABORT, 'a proposal must carry an expiry');
END;
