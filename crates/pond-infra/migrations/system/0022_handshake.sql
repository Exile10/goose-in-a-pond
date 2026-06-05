-- Two-phase GIAP↔GOTG handshake / pairing (#93).
--
-- Three tables back the `SqliteHandshakeAdapter`:
--   * pairing_codes        — single-use 6-digit codes (only the sha256 hash is
--                            stored; the plaintext lives process-local until
--                            consumed). failed_attempts gives a soft lockout.
--   * handshake_challenges — short-lived per-init challenges the client must MAC.
--   * session_tokens       — issued session + refresh tokens (sha256-hashed),
--                            with expiry / refresh-expiry / revoke / last-seen.

CREATE TABLE IF NOT EXISTS pairing_codes (
    code_hash       TEXT    NOT NULL PRIMARY KEY,
    created_at      TEXT    NOT NULL,
    expires_at      TEXT    NOT NULL,
    consumed_at     TEXT,
    failed_attempts INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_pairing_codes_active
    ON pairing_codes (consumed_at, expires_at);

CREATE TABLE IF NOT EXISTS handshake_challenges (
    id          TEXT NOT NULL PRIMARY KEY,
    client_id   TEXT NOT NULL,
    challenge   BLOB NOT NULL,
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    consumed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_handshake_challenges_client
    ON handshake_challenges (client_id);

CREATE TABLE IF NOT EXISTS session_tokens (
    token_hash         TEXT NOT NULL PRIMARY KEY,
    refresh_hash       TEXT NOT NULL,
    device_id          TEXT NOT NULL,
    client_id          TEXT NOT NULL,
    client_type        TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    expires_at         TEXT NOT NULL,
    refresh_expires_at TEXT NOT NULL,
    last_seen_at       TEXT NOT NULL,
    revoked_at         TEXT
);

CREATE INDEX IF NOT EXISTS idx_session_tokens_refresh ON session_tokens (refresh_hash);
CREATE INDEX IF NOT EXISTS idx_session_tokens_device  ON session_tokens (device_id);
