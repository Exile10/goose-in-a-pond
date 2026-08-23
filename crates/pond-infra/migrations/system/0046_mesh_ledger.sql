-- Real persistence for the private mesh's CreditLedger, UsageTally, and
-- PeerDirectory ports (#132 Milestone 4) — previously in-memory-only mocks.
--
-- peer_id is the lowercase-hex PeerId Display (64 chars) in all three tables.
-- Money/counters are INTEGER, never REAL: millisats are Bitcoin-denominated
-- (21M BTC cap = ~2.1e18 millisats), nowhere near i64::MAX (~9.22e18).

CREATE TABLE mesh_credit_balances (
    peer_id            TEXT PRIMARY KEY,
    balance_millisats  INTEGER NOT NULL DEFAULT 0,
    updated_at         TEXT NOT NULL
);

-- tokens_borrowed: what we owe the peer (settlement pays this).
-- tokens_lent: what the peer owes us (their job to settle, not ours).
CREATE TABLE mesh_usage_tally (
    peer_id         TEXT PRIMARY KEY,
    tokens_borrowed INTEGER NOT NULL DEFAULT 0,
    tokens_lent     INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT NOT NULL
);

CREATE TABLE mesh_trusted_peers (
    peer_id      TEXT PRIMARY KEY,
    trust_scope  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
