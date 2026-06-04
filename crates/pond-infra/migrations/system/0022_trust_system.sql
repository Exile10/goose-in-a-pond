-- Biometric trust system: hardware-bound public keys, replay protection,
-- intent queue, and a signed audit log for privileged actions.
--
-- See docs/biometric-trust-system.md for the full protocol description and
-- the rationale for splitting into four tables.

-- 1. Public keys for paired GOTG devices.
--    Each install_id (already used by the handshake protocol) maps to one
--    Ed25519 public key. Phones that pre-date this migration simply have
--    no row here and cannot authorise privileged actions until they re-pair.
CREATE TABLE device_pubkeys (
    install_id    TEXT PRIMARY KEY,
    public_key    BLOB NOT NULL,                 -- 32 bytes Ed25519
    algorithm     TEXT NOT NULL DEFAULT 'ed25519',
    registered_at TEXT NOT NULL,
    last_used_at  TEXT,
    revoked_at    TEXT
);

-- 2. Replay protection. Every nonce we have ever accepted in the last
--    replay-window is here. A periodic prune task deletes rows older than
--    the window so the table stays bounded.
CREATE TABLE replay_nonces (
    install_id TEXT NOT NULL,
    nonce      BLOB NOT NULL,                    -- 16 bytes random
    seen_at    TEXT NOT NULL,
    PRIMARY KEY (install_id, nonce)
);
CREATE INDEX idx_replay_seen ON replay_nonces(seen_at);

-- 3. Outstanding intents waiting on biometric assertion.
--    A privileged-tier handler creates a pending row, blocks until the
--    matching assertion arrives over the WebSocket, then resolves it.
CREATE TABLE pending_intents (
    id           TEXT PRIMARY KEY,               -- uuid v4
    action       TEXT NOT NULL,                  -- canonical action name
    summary      TEXT NOT NULL,                  -- human-readable summary
    payload_hash BLOB NOT NULL,                  -- blake3 of canonicalised payload
    requested_by TEXT NOT NULL,                  -- install_id or "local-desktop"
    created_at   TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    resolved_at  TEXT,
    resolved_by  TEXT,                           -- install_id of the signer
    outcome      TEXT                            -- "approved" | "denied" | "expired"
);
CREATE INDEX idx_pending_intents_expires ON pending_intents(expires_at);

-- 4. Privileged action audit log. Every assertion the pond accepts and
--    executes against gets a row here, including the raw signature so the
--    record is independently verifiable later.
CREATE TABLE privileged_audit (
    id          TEXT PRIMARY KEY,
    intent_id   TEXT NOT NULL,
    action      TEXT NOT NULL,
    install_id  TEXT NOT NULL,
    signature   BLOB NOT NULL,                   -- 64 bytes Ed25519 signature
    executed_at TEXT NOT NULL,
    result      TEXT NOT NULL                    -- "ok" | "error: <msg>"
);
CREATE INDEX idx_privileged_audit_action ON privileged_audit(action);
CREATE INDEX idx_privileged_audit_executed ON privileged_audit(executed_at);
