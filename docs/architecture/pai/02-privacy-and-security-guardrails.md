# PAI-2 — Privacy and security guardrails

Requirement: *privacy and security guardrails to minimise data and secret exposure.* Part of the
[Personal Agentic Intelligence programme](../personal-agentic-intelligence.md).
Prerequisites: [PAI-1](./01-identity-and-profile-boundaries.md) — a guardrail needs a subject.

Verified against code 2026-08-03.

---

## 1. What is true today

### 1.1 What is already strong, and must not regress

**Pairing and tokens.** Two-phase HMAC handshake where the six-digit code never crosses the wire
(`security/ports/handshake.rs:1-161`, `pond-infra/src/sqlite_handshake.rs`). Only SHA-256 hashes of
codes and tokens are persisted (`:5-12,164-165`); tokens are 32 `OsRng` bytes (`:96-98`);
comparisons are constant-time (`ConstantTimeEq` for the code hash, `hmac::Mac::verify_slice` in
`verify_mac` for the MAC); TTLs are pairing 10 min, challenge 60 s, session 24 h, refresh 30 d. The
blanket loopback bypass was removed in #94 and now requires `POND_DEV_ALLOW_LOOPBACK=1`.

I originally wrote "five failed attempts lock out". **That is wrong — there is no lockout, and its
absence is deliberate.** `sqlite_handshake.rs` says so directly: a wrong guess "burns the challenge
… and never touches the operator's pairing code, so there is no remote-triggerable lockout", the
`pairing_codes` table has no attempt counter, and `bad_mac_attempts_do_not_lock_out_pairing_code`
asserts it. Brute force is bounded instead by one-challenge-per-attempt plus a per-IP limiter of 10
verify attempts per 60 s over a 30-per-60 s handshake bucket. That is the better design for a
device on a home LAN — a lockout would hand any guest a denial-of-service against pairing — and the
doc should not have implied a mechanism the authors consciously rejected.

**Egress visibility.** `shared/services/egress.rs` is the best privacy primitive in the codebase: a
process-global request context attributes every outbound call to a session and tool (`:25-53`), and
host classification (`:126-161`) uses a curated 17-entry `KNOWN_PUBLIC_SUFFIXES` allowlist where
loopback is `Internal`, allowlisted hosts are `Public`, and **everything else defaults to
`Sensitive`**. Suffix matching is exact-or-dotted, with a test proving
`notwikipedia.org.evil.com` does not match (`:233-237`).

**Sensitivity classification.** `PrivacySensitivity { Public < Internal < Sensitive < Secret }`
(`security/domain/event.rs:56-67`) is ordered and queryable. The audit MCP server excludes `Secret`
in the store query itself rather than post-filtering (`pond-mcp-server/src/audit.rs:36-38,104-113`),
so row limits count only surfaceable events.

**Retention.** `pond-infra/src/pruning.rs` runs every six hours: event log 30 d, sensor readings
7 d, acknowledged camera events 14 d, 500 messages per session, orphaned face embeddings swept. Face
embeddings are deliberately never auto-expired — "biometric data managed by the user" (`:14-19`).

**Data minimisation already practised.** Memory embeddings are `#[serde(skip)]` and never appear in
JSON. Face recognition retains vectors, never images. FCM pushes are **data-only wake pings** with
no title or body, so content never transits Google (`fcm_push_relay.rs:1-26`). Secret *values* are
never returned by the secrets REST API.

### 1.2 The holes

**The policy layer is inert.** `SecurityPolicy::allow` returns `Ok(true)` unconditionally in both
implementations (`security/services/policy.rs:27-28`, `pond-infra/src/sqlite_security_policy.rs:62-64`).
Every `.audit()` call site in the repository is inside a `#[cfg(test)]` module — `policy.rs:76,79`
and `sqlite_security_policy.rs:125,151,182,200`. The file's own doc comment says so
(`sqlite_security_policy.rs:11-16`).

**API keys are in the settings table and returned over HTTP to anyone.** `api_key_guardian`,
`api_key_gnews`, `api_key_finnhub`, `api_key_coingecko` and `searxng_url` are `Option<String>`
fields on `Settings` carrying only `#[serde(default)]` — no `skip_serializing`. `GET /settings` does
`serde_json::to_value(settings)`, so it returns every one of them in plaintext. Meanwhile a real
`SecretRepository` port exists (`security/ports/secret.rs`) whose doc says "values are NEVER
returned through the REST API".

I first wrote that this exposed the keys "to any authenticated client". **That was too generous.
Re-checked 2026-08-04: no authentication is required at all.** See below.

**The auth allowlist matches on path only, so several "protected" routes are public.**
*(FIXED 2026-08-05 by P0. Kept in full, because the shape of the mistake is the point and because
the table below is the reproduction record. `is_public_route` now takes a `&Method` and matches a
`PUBLIC_ROUTES: &[(Method, &str)]` table segment-wise; three compile-time guards fail the build if
the table and the router drift. Everything in the table below now requires a token — except
`PUT /settings`, `POST /profiles` and `PATCH /profiles/{id}`, which stay public for onboarding and
are P7's job.)*

`is_public_route` (`pond-api/src/middleware/mod.rs`) received just `path.path()` from
`auth_middleware` — never the method — while its entries were written as though method-scoped:

| Entry | Comment says | Actually public |
|---|---|---|
| `path == "/settings"` | "PUT /settings is public so onboarding steps can save before completion" | **`GET /settings` too — every API key, no token** |
| `"/profiles"` | "POST — create profile during onboarding" | `GET /profiles` (enumerate the household) |
| `path.starts_with("/profiles/")` | "PATCH /profiles/:id — update profile preferences during onboarding" | `GET /profiles/{id}` and **`DELETE /profiles/{id}`** |

The `protected_routes` label in `routes.rs` is cosmetic: `public_routes.merge(protected_routes)`
produces one router, and the single `auth_middleware` layer decides purely on the path string. So
`delete_profile`, registered in the protected group, is reachable unauthenticated by anything that
can open a socket to the pond.

This is the most serious thing this workstream found, it is a live defect rather than a missing
feature, and it should be fixed ahead of the rest of the phase list.

**There is no keyring.** Despite the filename, `pond-infra/src/keyring_secret_repository.rs`
contains only `FileSecretRepository`: plaintext JSON at `<data_dir>/secrets.json`, chmod 0600, with
environment variables taking precedence (`:51-58`). That is what production wires
(`pond-server/src/main.rs:2353`). The `keyring` crate in the root `Cargo.toml` is a Goose submodule
mirror entry, not a GIAP dependency.

**There is no redaction.** Grep for `redact|PII|scrub|anonymi` over `crates/` returns only comments
plus `pond-infra/src/push_token_log.rs`, which shortens push tokens for log lines. Nothing inspects
memory content, chat text, or event attributes for personal data before storage.

**There is no encryption at rest.** Both SQLite databases and `secrets.json` are plaintext; the only
protection is the 0600 file mode.

**Onboarding leaves auth holes open permanently.** `PUT /settings`, `POST /profiles` and
`PATCH /profiles/{id}` are on the public allowlist (`middleware/mod.rs:165-201`) and stay there
after onboarding completes.

---

## 2. The gap

GIAP is excellent at *observing* privacy-relevant events and poor at *preventing* them. It knows
which host a tool called and classifies it `Sensitive`; it cannot stop the call. It has an eight-scope
authorisation model that authorises everything. It writes API keys into the same table it serves to
clients. Requirement 8 asks for guardrails; today there are gauges.

---

## 3. Design

### 3.1 Make the policy real, but land it in audit mode first

`SecurityPolicy` becomes a deny-by-default matrix over the eight existing scopes ×
`PrincipalKind` × profile. A new setting governs the transition:

```
security_policy_mode = "off" | "audit" | "enforce"     # default "audit"
```

- `off` — today's behaviour, for debugging.
- `audit` — every decision is evaluated and **logged**, but a deny does not block. This is how the
  matrix gets validated against real households before it can lock anyone out of their own pond.
- `enforce` — denies bite.

Landing in `audit` is the whole point. A rules matrix written from first principles will be wrong in
ways only real traffic reveals, and an authorisation regression in a home assistant looks like the
lights not turning on.

`audit()` gets its first production call sites here, alongside PAI-1's cross-profile check.

### 3.2 Secret hygiene

1. `api_key_*` migrate from `Settings` to `SecretRepository`. `init_news_deps` and friends
   (`giap_registration.rs:135-155`) already take a settings repo; they take a secret repo instead.
2. `GET /settings` must not be able to regress. Add a build-breaking guard test in the same spirit
   as `every_settings_field_is_dispositioned` (`settings.rs:1630-1795`): **no serialized `Settings`
   key may match `*_key`, `*_token`, `*_secret`, `*_password`.** A guard test is the right tool here
   because the failure mode is a future field added by someone who has not read this document.
3. `FileSecretRepository` gains an encrypted backend (below). The file keeps its name honest — either
   implement the keyring or rename the module. Rename is cheaper and clearer.

### 3.3 A redactor with an explicit boundary

New port `security/ports/redactor.rs`:

```rust
pub trait Redactor: Send + Sync {
    /// Returns the text with detected personal data replaced, plus what was found.
    fn redact(&self, text: &str, level: RedactionLevel) -> Redacted;
}
```

Deterministic, rule-based adapter first (e-mail addresses, phone numbers, card and IBAN shapes,
API-key shapes, postal addresses). No model in the loop — a redactor that costs a 20 tok/s inference
call will be disabled by the first user who notices.

Applied at exactly three chokepoints:

1. Before a `MemoryFragment` is written.
2. Before event `attributes` reach the event log.
3. Before any body leaves the pond (PAI-8 connectors, webhooks).

**Explicit non-goal, stated so nobody 'fixes' it later: the redactor is not applied to the model's
own prompt.** Redacting the assistant's view of your life is what makes it useless. The privacy
property GIAP offers is *the model runs on your hardware*, not *the model is blindfolded*. What
redaction protects is the durable stores and anything that leaves.

### 3.4 Encryption at rest, honestly scoped

Full-database SQLCipher is a real lift: a different SQLite build, key custody, a Jetson cross-build,
and `pond_system.db` is read on every turn and every session listing.

**v1 encrypts what actually hurts**: `secrets.json` and connector tokens, with ChaCha20-Poly1305
under a keyfile at `<data_dir>/secrets/master.key` (0600), generated on first run. This is a small,
testable change that removes the "your Gmail refresh token is a plaintext string on a home server"
problem, which is the one PAI-8 creates.

The SQLCipher path for the full database is designed here and **deferred with its cost written
down**, rather than promised. Re-open it when someone is prepared to own the cross-build.

### 3.5 Egress becomes enforcement

`record_egress` is promoted from observability to a gate:

```
network_mode = "open" | "allowlist" | "offline"        # default "open" for compatibility
```

`allowlist` refuses hosts that classify as `Sensitive` — reusing `KNOWN_PUBLIC_SUFFIXES` and the
existing fail-Sensitive default, which is exactly the right polarity for this. `offline` refuses
everything but loopback, which makes "prove it is not phoning home" a one-setting demonstration
rather than a packet capture.

Every new outbound adapter copies `traced_send` from `pond-adapters-weather/src/lib.rs:15-30`. A
guard test asserts every crate with a `reqwest` dependency references `record_egress`.

### 3.6 The outbound-action gate

`giap-draft` is already unconditionally registered as a safety extension
(`giap_registration.rs:64`) and already models save / list / approve / reject
(`pond-mcp-server/src/draft.rs:97,167,200,248`). Every side-effecting action from a connector —
send, post, publish, delete — routes through it, scoped to the acting profile. This is reuse of a
mechanism that exists and is already trusted, not a new approval system.

### 3.7 Close the onboarding holes

`PUT /settings`, `POST /profiles` and `PATCH /profiles/{id}` leave the public allowlist once
onboarding is complete. `middleware/onboarding_guard.rs` already knows that state.

---

## 4. Phases

- **P0 — LANDED 2026-08-05.** `is_public_route` takes a `&Method`, and the allowlist is a
  `PUBLIC_ROUTES: &[(Method, &str)]` table whose `{brace}` segments match exactly one path segment.
  The four reproduced leaks are closed: `GET /settings` and `GET /profiles` now 401, while the
  `PUT /settings` and `POST /profiles` that onboarding needs stay open — same paths, different
  answers, which is the whole point. `DELETE /profiles/{id}` was public because of a
  `starts_with("/profiles/")` prefix test that also covered `GET` and every other method on any
  sub-path; segment-wise matching ends that.

  **Three build-breaking guards, because the allowlist and the router are two lists that must agree
  and nothing structurally forced them to.** All three parse `routes.rs` through `include_str!`, so
  they run at compile time with no runtime file IO and cannot drift:
  `every_protected_route_requires_a_token` (P0's stated acceptance test),
  `public_router_and_allowlist_agree` (drift in *either* direction — a public route missing from the
  allowlist 401s during onboarding, an allowlist entry with no route is an exemption that outlives
  its reason), and `the_pai2_p0_leaks_are_closed`, which states the four leaks as the HTTP requests
  that leaked.

  **Two exceptions are real** and are listed individually rather than skipped by prefix, so a third
  `/oauth/*` route cannot join them silently: `GET /oauth/callback` (the browser arrives from the
  provider with no token; the PKCE state nonce authenticates it) and `POST /oauth/refresh` (checked
  against `internal_extension_token` inside the handler). Both live in the *protected* router, which
  is why "every route in `protected_routes` requires a token" could not be a blanket assertion — the
  router's split is about **onboarding**, and `is_public_route` is about **auth**. Two different
  axes that read like one.

  **The guards were mutation-tested rather than trusted.** Re-adding `(Method::GET, "/settings")` to
  the table fails all three, with the offending route named. Worth the two minutes: this programme
  has three recorded vacuous-test incidents, and a guard that cannot fail is worse than none because
  it reads as coverage.

  **The fix exposed a test that had been passing for the wrong reason.**
  `settings_is_blocked_before_onboarding` sent no token and asserted 403. It passed only *because*
  `GET /settings` was on the public allowlist: the request went straight through auth and was
  stopped by the onboarding guard. Once auth could refuse it, the assertion saw 401 — the correct
  answer — and the test was the thing that had to change. Every sibling in that file already carried
  a bearer token for exactly this reason; this one never needed one, because it could not reach auth
  to be stopped by it. **A test asserting the right status for the wrong reason is indistinguishable
  from a correct one until the reason moves**, which is the same lesson as the unreachable
  `ProfileScope::Owner` fixtures, arriving from the opposite direction.

  Verified live: `scripts/live-test.sh --ui` passes end to end — all five probed routes 401 with no
  token and the bypass off, 37 API checks, 4/4 live UI. `pond-api` 109 lib tests (105 + 4) and 137
  across all 17 integration targets (135 + 2: `GET /settings` unauthorised over real HTTP with
  onboarding complete so a 403 cannot mask it, and `PUT /settings` still open — if that one ever
  401s, onboarding deadlocks on a pond nobody can finish setting up). `pond-core` 736. fmt and
  clippy clean.

  Original phase text, for the record:

- **P0 (as designed) — do this first, ahead of any design work.** Make the auth allowlist method-aware. Every
  entry in `is_public_route` is written as though it were method-scoped and none of them are. Pass
  the `Method` alongside the path, enumerate `(Method, path)` pairs rather than paths, and add a
  test asserting that every route in `protected_routes` requires a token — the
  `public_routes.merge(protected_routes)` split gives a false sense of safety otherwise. This is a
  small, self-contained fix and it should not wait behind the policy-mode work.

  **Reproduced against a running server, 2026-08-04.** Not inferred from reading the allowlist —
  driven with `curl` against `pond-server serve`, no `Authorization` header, and with
  `POND_DEV_ALLOW_LOOPBACK` unset so the loopback bypass was off:

  | Request | Expected | Actual |
  |---|---|---|
  | `GET /api/v1/settings` | 401 | **200, with `api_key_gnews` and `api_key_finnhub` in plaintext** |
  | `PUT /api/v1/settings` | 200 (deliberately public for onboarding) | 200 |
  | `GET /api/v1/profiles` | 401 | **200, the whole household listed** |
  | `DELETE /api/v1/profiles/{id}` | 401 | **204, member deleted** |
  | `GET /api/v1/sessions` | 401 | 401 |
  | `GET /api/v1/devices` | 401 | 401 |
  | `GET /api/v1/memory` | 401 | 401 |

  The last three matter as much as the first four: the auth layer *does* work, so this is not a
  missing middleware. It is precisely the allowlist entries being matched on path alone.

  The `PUT` row is what turns a read leak into a full chain — an unauthenticated caller on the LAN
  can write a key through the onboarding hole and read it back through the path-matching hole. Both
  halves were exercised in that order and both succeeded.

  One qualifier, because it changes how urgent this looks on a fresh install: `GET /settings` leaks
  no key *value* until a key is actually configured. The exposure is real for any pond whose owner
  has set one up, and invisible before that.

  **The reason I first gave for that qualifier was wrong, and it contradicted section 1.2 of this
  same document.** I wrote that the key fields carry `skip_serializing_if = "Option::is_none"`.
  They do not — verified 2026-08-05, all four `api_key_*` fields carry only `#[serde(default)]`,
  exactly as 1.2 says. An unset key is therefore *serialized as `null`* rather than omitted, which
  happens to leak no value and is not the mechanism I claimed. Worth recording because the two
  halves of one document disagreed and the wrong half was the one attached to the reassuring
  conclusion. **P0 put a bearer token in front of this route; the keys are still on the struct and
  still serialized into the body. That is P2, and it is untouched.**
- **P1** `security_policy_mode` with `audit` default; the scope × principal matrix; first production
  `allow`/`audit` call sites (shared with PAI-1 P4).
- **P2** Secret migration off `Settings`; the `*_key|*_token|*_secret` guard test; module rename.
- **P3** `Redactor` port + rule-based adapter; the three chokepoints; per-rule tests with real-shaped
  false-positive cases (a UK postcode inside a normal sentence must not be mangled).
- **P4** Keyfile encryption for secrets and connector tokens.
- **P5** `network_mode` enforcement + the `reqwest`-implies-`record_egress` guard test.
- **P6** Draft gate for outbound connector actions (lands with PAI-8).
- **P7** Onboarding allowlist closure.
- **P8** Flip default to `security_policy_mode = "enforce"` — only after a release in `audit` with
  telemetry showing what would have been denied.
- **DEFERRED** SQLCipher for the full database.

---

## 5. Invariants

1. A deny is logged with its principal, scope, action and outcome — an unexplained refusal is worse
   than no refusal.
2. Secret values never appear in a REST response, a log line, an event attribute, or a prompt.
3. The redactor never runs on the model's prompt.
4. Egress classification stays fail-`Sensitive`. New allowlist entries are added deliberately, with
   the exact-or-dotted matcher preserved.
5. `enforce` mode never locks a user out of `/handshake/*` or `/health` — recovery must stay
   reachable.

---

## 6. Deliberate deferrals

- **Full-database encryption.** See 3.4.
- **Model-based PII detection.** Costs inference on the critical path; the rule-based redactor
  handles the shapes that actually leak.
- **Per-extension capability sandboxing** (an extension declaring which scopes it needs). Attractive,
  and much easier once the policy matrix exists — revisit after P8.

---

## 7. Verification

- **Unit** — the policy matrix, one test per scope × principal-kind cell, including the recovery
  routes that must never be denied.
- **Guard tests** — no secret-shaped settings key is serialized; every `reqwest`-using crate
  references `record_egress`. Both must fail the build, not warn.
- **Redactor** — a corpus with true positives and deliberately hard negatives; assert idempotence
  (redacting twice equals redacting once).
- **Integration** — `network_mode = "offline"`, run a weather query, assert a clean refusal with an
  actionable message rather than a timeout.
- **Manual** — `GET /settings` on a pond with every API key set; assert not one key value appears in
  the response body.
