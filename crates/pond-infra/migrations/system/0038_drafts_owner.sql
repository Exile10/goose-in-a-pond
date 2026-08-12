-- PAI-2 P1 (second call site): give a staged action a knowable owner.
--
-- `drafts` shipped in 0018 with `session_id TEXT NOT NULL`, no owner column and
-- no foreign key. Two consequences, both live until this migration:
--
--   1. approve_draft looked a draft up by id, checked only that it was pending,
--      and approved it. Any session could approve any draft id.
--   2. `session_id` was never authoritative. SaveDraftParams.session_id is a
--      field the MODEL fills in, defaulting to the literal "default", so every
--      draft landed in one shared bucket and list_drafts -- which DOES scope by
--      session -- read everybody's out of it.
--
-- The column was not merely absent; the substitute was untrustworthy. The code
-- change alongside this takes the session from the MCP request `_meta`
-- (`agent-session-id`, injected by the engine on every tool call) instead of
-- from the model, and stamps profile_id from the resolved turn scope.

ALTER TABLE drafts ADD COLUMN profile_id TEXT;
-- The household member whose turn staged this action. NULL means "staged
-- before an owner could be resolved".
--
-- NULL here is NOT the reading it carries on memory_fragments. There, NULL is
-- shared household context and Owner(id) deliberately matches it. A staged
-- shell command is not shared context: an unowned draft is one nobody can be
-- shown to have asked for, so it must narrow rather than widen. See
-- is_draft_decision_permitted.

ALTER TABLE drafts ADD COLUMN identification_source TEXT;
-- 'paired_device' | 'explicit' | 'face' | 'unknown'; NULL for legacy rows.
-- Same vocabulary as sessions.identification_source (0037). PAI-1 invariant 3:
-- a draft approved on a 0.62 face match must be distinguishable in the audit
-- log from one approved on a paired token.

CREATE INDEX IF NOT EXISTS idx_drafts_profile_status ON drafts(profile_id, status);

-- What happens to rows that already exist, stated rather than left implicit.
--
-- Every draft that already exists is pending in the "default" bucket with no
-- owner, staged by a speaker nobody recorded. Leaving them pending would mean
-- the first person to say "approve" after this upgrade inherits the right to
-- run them -- the exact outcome this migration exists to prevent.
-- Owned-by-nobody-and-approvable-by-anybody was the defensible alternative and
-- it is rejected here, because these are side-effecting actions and the set is
-- small: a staged action is meant to be confirmed in the same breath, not
-- inherited across an upgrade.
--
-- DraftStatus::Expired already existed and had no producer. It does now. The
-- cost is real and small: a user mid-confirmation across a restart is told the
-- draft expired and asks again.
UPDATE drafts SET status = 'expired' WHERE status = 'pending';

-- Deleting a household member must not leave their staged actions approvable.
--
-- Unlike the sibling trigger in 0037, this one is not standing in for a missing
-- ON DELETE action: drafts.profile_id carries no foreign key (0018 declared
-- none, and adding one to an existing table means rebuilding it), so the delete
-- was never going to fail. The trigger is here for the semantics, not the
-- constraint -- a departed member's pending shell command is precisely the
-- thing that must not outlive them. Order matters: expire first, then release,
-- or the second UPDATE hides the rows from the first.
CREATE TRIGGER IF NOT EXISTS trg_profiles_delete_expires_drafts
BEFORE DELETE ON profiles
BEGIN
    UPDATE drafts
       SET status = 'expired'
     WHERE profile_id = OLD.id AND status = 'pending';
    UPDATE drafts
       SET profile_id = NULL,
           identification_source = NULL
     WHERE profile_id = OLD.id;
END;

-- The draft server learns the ENGINE's session id, not GIAP's, so resolving a
-- speaker means walking engine_session_map backwards. Its only index today is
-- the primary key on session_id.
CREATE INDEX IF NOT EXISTS idx_engine_session_map_engine
    ON engine_session_map(engine_session_id);
