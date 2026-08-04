-- PAI-1 P2: give a session a knowable owner, and record how we know.
--
-- `sessions.profile_id` is NOT added here. It has existed since
-- 0003_profiles.sql, which created the profiles table and bolted the column on
-- in the same file. Nothing has ever written it: the domain `Session` had no
-- such field and the SQLite adapter never selected it. This migration adds the
-- provenance the column always needed, and the adapter change alongside it is
-- what finally populates both.
--
-- Provenance is not decoration. A session bound because a paired phone
-- presented that member's token is a far stronger claim than one bound by a
-- 0.62-confidence face match, and every later authorisation decision has to be
-- able to tell those apart. Storing only `profile_id` would flatten them.

ALTER TABLE sessions ADD COLUMN identification_source TEXT;
-- 'paired_device' | 'explicit' | 'face' | 'unknown'; NULL for legacy rows.

ALTER TABLE sessions ADD COLUMN identification_confidence REAL;
-- Only meaningful for 'face'. NULL everywhere else, including for the sources
-- that are certain -- a paired-device binding is not "confidence 1.0", it is a
-- different kind of claim, and encoding it as a number invites averaging.

-- Deleting a household member must not fail because they once held a session.
--
-- 0003 declared the column as a plain `REFERENCES profiles(id)` with no ON
-- DELETE action, which SQLite reads as NO ACTION, and `Database::init` sets
-- `PRAGMA foreign_keys = ON`. That combination has been harmless only because
-- the column was always NULL. The moment this migration's sibling code starts
-- writing it, `DELETE FROM profiles WHERE id = ?` begins failing for any member
-- who has ever spoken to the pond.
--
-- SQLite cannot alter a foreign key's action without rebuilding the table, and
-- `sessions` is the hottest table in the database. A BEFORE DELETE trigger
-- gives exactly ON DELETE SET NULL semantics at a fraction of the risk: it runs
-- before the row is removed, so nothing references the profile by the time the
-- delete is applied.
--
-- The session itself survives, stripped of its attribution. That is the
-- correct outcome and it is the same one ON DELETE SET NULL would produce --
-- deleting a member erases who they were, not the fact that a conversation
-- happened. Erasing their content is a separate, deliberate cascade (PAI-1 P7).
CREATE TRIGGER IF NOT EXISTS trg_profiles_delete_releases_sessions
BEFORE DELETE ON profiles
BEGIN
    UPDATE sessions
       SET profile_id                = NULL,
           identification_source     = NULL,
           identification_confidence  = NULL
     WHERE profile_id = OLD.id;
END;

CREATE INDEX IF NOT EXISTS idx_sessions_profile_id ON sessions(profile_id);
