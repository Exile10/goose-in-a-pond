# Auth & network-exposure posture (Phase 0)

How `pond-server` authenticates clients and what it exposes on the network.
Covers issues #4, #8, #93, #94.

## Bind & exposure

`pond-server` binds `0.0.0.0:<API_SERVER>` (default 4000), so it is reachable
from the **local network**, not just loopback. Everything below assumes that
LAN reachability — the server must be safe to expose to other devices on the
same network (e.g. a GOTG phone).

## Authentication

- All `/api/v1/*` routes require `Authorization: Bearer <session_token>` and are
  rejected with **401** otherwise, **except** the public allowlist: `/health`,
  `/handshake`, `/handshake/{init,verify,refresh,revoke,pairing-code}`,
  onboarding routes, and a few local dev/test pages
  (`crates/pond-api/src/middleware/mod.rs::is_public_route`).
- Tokens are validated against the DB-backed `SqliteHandshakeAdapter`
  (`validate_token`): only unrevoked, unexpired session tokens pass.
- Session tokens expire after 24h; refresh tokens after 30d. Clients rotate via
  `POST /api/v1/handshake/refresh` (rotation revokes the old session).

## Pairing (how a client gets a token)

Two-phase, HMAC-based — the 6-digit pairing code is **never sent over the wire**:

1. Operator reads the pairing code printed on server startup (or `GET
   /api/v1/handshake/pairing-code`, loopback-only).
2. Client `POST /handshake/init {client_id,…}` → `{challenge_id, challenge}`.
3. Client computes `mac = HMAC-SHA256(pairing_code, challenge ‖ client_id)` and
   `POST /handshake/verify {challenge_id, mac}` → `{session_token, refresh_token,
   expires_at}`.

Codes are single-use, expire in 10 min, and lock out after 5 failed attempts.
Only sha256 hashes of codes/tokens are persisted.

### Token contract

`HandshakeResponse` uses **`session_token`**, **`refresh_token`**, and
**`expires_at`** (absolute RFC3339). We standardised on absolute expiry rather
than relative `expires_in` to avoid clock-skew / round-trip drift; server,
GOTG, and desktop clients all use this shape.

## Loopback

The blanket loopback auth bypass was **removed** (#94). By default, even
same-host clients (including the desktop app) must present a valid token —
the desktop auto-pairs via the loopback `pairing-code` endpoint.

For local development you can opt back into the bypass with:

```
POND_DEV_ALLOW_LOOPBACK=1
```

This is **off by default** and intended only for dev machines.

## CORS

Scoped to first-party origins (`tauri://localhost`, `http://localhost:1420`,
`http://127.0.0.1:1420`); **not** `Any`. Add extra browser origins (e.g. a LAN
dashboard) with a comma-separated:

```
POND_CORS_ALLOWED_ORIGINS=https://dashboard.lan,https://…
```

Native mobile clients (GOTG) don't send a browser `Origin` header, so CORS does
not apply to them.

## Rate limiting

Per-client IP rate limiting (600 req / 60 s) applies to remote clients,
including handshake attempts. Loopback is exempt from throttling (first-party
host); brute-forcing a pairing code is independently stopped by the 5-attempt
lockout.
