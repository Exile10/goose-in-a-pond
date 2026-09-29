# pondcredentials: managed credentials for Apple Music

A household should not need an Apple developer account to play a song. `pondcredentials` is the
small service that lets that be true: it holds one MusicKit key and hands ponds a signed developer
token. Status, 2026-09-29: **built and tested; not deployed.** No droplet, no domain, and the pond
does not use it by default (see "Turning it on").

## Why a service, and not a token in the release

The pond already ships Spotify's client id in the binary (`bundled_client_id`), and that is fine
because an OAuth PKCE client id is not a secret. Apple's key is a secret, so the equivalent is to
ship a *token*. That is simpler and has no server. It was rejected for two reasons:

- **Revocation is an outage.** If the key is revoked, a shipped token stays dead until every household
  updates. With a service, a new key is deployed once and ponds refetch.
- **It cannot be rate limited.** A token in a release can be copied and used without limit; a service
  can at least slow one address down.

The cost of the service is real: something to run and pay for, a Jarida host every pond can reach,
and a privacy footprint, below.

## The parts

| Part | Where | Job |
|---|---|---|
| Shared signing crate | `services/pondcredentials/token` (`pond-apple-token`) | Normalises a pasted `.p8`, signs the ES256 token, refuses a lifetime past Apple's ceiling. Used by **both** sides, so they cannot drift. |
| The service | `services/pondcredentials/server` | `POST /v1/musickit/developer-token`. Fails to start on a missing setting or a key that cannot sign. |
| The pond's client | `crates/pond-api/src/musickit.rs` | A stored local key wins; else, when a service address is set, fetch, cache, and serve. |
| The deploy kit | `deploy/pondcredentials/` | Dockerfile, compose, Caddy, and a runbook with the `doctl` steps. |

The service lives in its own Cargo workspace so a Docker image can copy only that directory, without
the goose submodule and the pond's native engines.

## How a pond uses it

`GET /musickit/developer-token` (the player page's route) signs locally if the household stored a
key. Otherwise, if `POND_CREDENTIALS_URL` names a service, it asks that:

- **Once a month.** The token lives 30 days and is refetched when a fifth of its life is left (or under
  a day), so a household holds weeks of validity at all times.
- **Through the gate.** The fetch is `egress::begin_as(.., "giap-credentials")`: refused under
  `network_mode = offline` before anything is sent, and recorded in the egress log under that name.
- **Kindly to a service that is down.** A failure stands for a minute so the player's ten-second retry
  does not hammer it, and a token still inside its life is served if a refresh fails.
- **Suspiciously.** The reply must be a three-part token with an expiry Apple could have issued
  (not past 200 days), and the address must be `https` (loopback is allowed, for testing).

## What it does to the privacy picture

Every pond that uses it calls a Jarida-run host. What that reveals: **an address asked for a token, at a
time.** The service reads no body, sets no cookie, identifies no one, and keeps no address (the rate
limit is memory only; the counters are two numbers; Caddy has no access log). What it cannot promise is
what the hosting provider's network keeps.

It is one more outbound path, documented as such in `02-privacy-and-security-guardrails.md`. It is
**off until an address is set**, so a household that never enables it never calls anyone.

## Threat model

| Threat | Answer | Left over |
|---|---|---|
| The key leaks from the droplet | It is a file, read-only, never in an image, variable or log; the container is non-root, read-only, no shell, no capabilities | Revoking it breaks ponds holding its tokens until they refetch, up to about three weeks. Rotate with an overlap (runbook). |
| Someone collects tokens | A token is not secret: every MusicKit page ships one | Nothing to add; the rate limit slows it |
| Abuse or a flood | Per-address limit (IPv6 by /64, memory bounded), a 1 KB body cap, one route | A wide botnet is not stopped |
| A network attacker swaps the reply | https only, apart from loopback; the reply is validated | Trusting the certificate authority system |
| Third parties running the open-source pond | Nothing stops them calling it | Rate limits, and revoking the key |
| **Apple's terms** | Not established | **Whether Apple permits one team's developer token to serve independent installations of an open-source app is not checked here and should be, before this is turned on.** |

## Turning it on

Deploy it (`deploy/pondcredentials/README.md`), try it on one pond with `POND_CREDENTIALS_URL`, and only
then put the address in `DEFAULT_MANAGED_URL`. That last step changes what every pond does with no key
of its own, so it is a decision rather than a default.

## Verification

Service and shared crate: 29 tests (config validation, the token's cryptography, the limiter, and the
HTTP behaviour, including that no response can contain key material). The pond's side: 28 route tests
against a real mock service on loopback (fetch once and cache, a local key wins, failure is remembered,
garbage is refused, offline sends nothing and says which setting, an insecure address is ignored). The
**real service binary** was run with a throwaway key and a **real pond** fetched a token from it: the
signature verified against the service's public key, a second ask did not reach the service, and the
pond logged the call as `giap-credentials`.

Not done: building the Docker image (no running daemon on the machine this was written on), starting the
compose stack, the Caddyfile, and every `doctl` command. Nothing has run against Apple's servers with a
managed token.
