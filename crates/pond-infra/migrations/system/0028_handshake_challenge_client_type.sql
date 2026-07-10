-- Persist the client_type from InitRequest through the challenge so that
-- verify_handshake types the devices row correctly (was hardcoded 'gotg').
--
-- ADD COLUMN NOT NULL DEFAULT 'gotg' keeps any in-flight pre-migration challenge
-- rows valid and preserves current behaviour for the legacy mobile client, whose
-- InitRequest also sends 'gotg'.
ALTER TABLE handshake_challenges
    ADD COLUMN client_type TEXT NOT NULL DEFAULT 'gotg';
