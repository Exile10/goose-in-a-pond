# Biometric trust system

This document describes how Goose In A Pond (GIAP) authorises *privileged*
actions using a hardware-attested biometric assertion from a paired
Goose On The Go (GOTG) phone.

The design adopts the FIDO2/WebAuthn *pattern* (challenge-response with a
hardware-bound signing key gated by an OS biometric prompt) without any
cloud middleman. All keys, intents, signatures, and audit records live on
hardware the user owns.

## Three trust tiers

Every API endpoint is classified by [`crates/pond-api/src/trust_levels.rs`](../crates/pond-api/src/trust_levels.rs):

| Tier | Examples | Required proof |
|---|---|---|
| **Ambient** | health check, weather lookup, ambient identification | None |
| **Personal** | calendar lookup, memory read, settings read | Valid session token (existing handshake) |
| **Privileged** | wake-word change, provider swap, biometric deletion, household-member edits | Hardware-attested biometric assertion from a paired GOTG |

Actions can additionally be flagged `RemotePolicy::LocalOnly`, which refuses
the call when it didn't arrive over a LAN-classified connection. Use this
for irreversible side effects whose risk includes coercion (e.g. door
unlock).

## Roles

| FIDO2 role | This project |
|---|---|
| Authenticator | GOTG phone, Ed25519 keypair sealed in Secure Enclave / StrongBox |
| Relying Party | GIAP (`pond-server`) |
| Client | GOTG app, no browser |

## Wire flow

```text
1. PAIR (one-time, on LAN)
   GOTG ───► GIAP   POST /api/v1/handshake/init   (existing)
   GIAP ───► GOTG   challenge_id + 32-byte challenge
   GOTG generates Ed25519 keypair via tweetnacl, stores priv with
       requireAuthentication=true (Secure Enclave / StrongBox)
   GOTG ───► GIAP   POST /api/v1/handshake/verify
                    { challenge_id, mac, public_key (hex32) }
   GIAP verifies HMAC, stores public_key against client_id in
       device_pubkeys.

2. PRIVILEGED-ACTION REQUEST (any time, anywhere)
   Caller (desktop / voice) ───► GIAP   PUT /api/v1/settings ...
   GIAP classifies: TrustLevel::Privileged.
   GIAP creates Intent, persists into pending_intents.
   GIAP ───► GOTG (via WebSocket /api/v1/auth/ws)
        { type: intent.request, intent_id, action, summary,
          payload_hash, expires_at }

3. BIOMETRIC ASSERTION (on the phone)
   GOTG shows modal: "Approve: <summary>"
   User taps Approve → expo-local-authentication fires
       Face ID / Touch ID / fingerprint at the OS level.
   On success, GOTG reads the private key (this triggers the OS
       Secure Enclave gate again — coalesced with the prompt above
       on iOS, single biometric on Android).
   GOTG signs SHA-256(scope) with Ed25519.
   GOTG ───► GIAP (over the same WS)
        { type: intent.assert, intent_id, install_id, ts, nonce, signature }

4. VERIFY + EXECUTE
   GIAP verifies signature against stored public_key.
   GIAP checks ts skew ≤ 30 s, nonce unseen in 5-min replay window.
   GIAP atomically inserts the nonce into replay_nonces.
   GIAP records signed evidence in privileged_audit.
   GIAP executes the underlying action and returns the result to the
       original caller.
```

## Signature scope

The signature is over SHA-256 of the following length-delimited buffer:

```text
"giap-intent-v1\0"
u32 install_id_len ‖ install_id_bytes
16 bytes intent_id (UUID big-endian)
u32 action_len ‖ action_bytes
32 bytes payload_hash    (blake3 of canonicalised JSON payload)
i64 ts_secs (big-endian)
16 bytes nonce
```

The string `"giap-intent-v1\0"` is a domain separator. Bumping it would
invalidate every existing biometric assertion — versions are intentionally
hard to roll out so the bar is high.

The implementation lives in
[`pond-core/src/domain/trust.rs::signature_scope`](../crates/pond-core/src/domain/trust.rs)
on the server and
[`goose-on-the-go/services/fido-key.ts::buildScope`](../../goose-on-the-go/services/fido-key.ts)
on the phone. Tests in `pond-adapters-trust` exercise both happy and
adversarial paths.

## Storage

SQLite migration `0016_trust_system.sql` adds:

- `device_pubkeys` — `(install_id, public_key, registered_at, last_used_at, revoked_at)`
- `replay_nonces` — `(install_id, nonce, seen_at)`
- `pending_intents` — outstanding intents waiting for assertions
- `privileged_audit` — append-only log of every executed Privileged action

## What is NOT here

- **Active liveness on the pond webcam.** This was rejected as a
  user-experience non-starter; the photo-attack surface is bounded to
  Tier-C/B, where it's acceptable.
- **Passive identification** on the pond is unchanged: ArcFace + the
  PAD ensemble continues to identify the speaker for ambient personalisation.
  It is no longer the gate for privileged actions.

# Networking — WireGuard + Headscale

To support remote control without a cloud middleman, GIAP and GOTG join a
self-hosted mesh.

## Why WireGuard + Headscale

- WireGuard: end-to-end encrypted by design, every datagram authenticated
  by per-peer keys. The relay (if any) carries only ciphertext.
- Headscale: open-source self-hosted Tailscale control plane. You run the
  coordinator on a tiny VPS (or even at home behind a public DNS name).
  No SaaS dependency.

Tailscale's mobile clients are wire-compatible with Headscale, so on the
phone we ride on the official Tailscale app — meaning we don't need to
implement the Network Extension entitlement ourselves.

## Setup (recommended)

### 1. Run Headscale on a small VPS

```bash
# Any tiny VPS (1 vCPU / 1 GB RAM is plenty).
docker run --name headscale \
  -v ./headscale:/etc/headscale \
  -p 8080:8080 -p 50443:50443 \
  -d headscale/headscale:latest serve
```

A real config goes in `./headscale/config.yaml`. See the
[Headscale docs](https://headscale.net/) for full details.

### 2. Pre-auth keys

```bash
docker exec headscale headscale users create giap-user
docker exec headscale headscale --user giap-user preauthkeys create --reusable --expiration 24h
```

Hand the resulting key to both pond and phone during pairing.

### 3. Pond joins the mesh

On the Jetson / Mac running pond-server:

```bash
brew install --cask tailscale     # or apt: tailscale, etc.
sudo tailscale up --login-server https://your-headscale.example \
                  --authkey YOUR_PRE_AUTH_KEY \
                  --hostname pond-living-room
```

`pond-server` listens on `0.0.0.0:4000` as today. Inside the mesh, GOTG
reaches it at `http://pond-living-room:4000` (the Tailscale magic-DNS
name).

### 4. Phone joins the mesh

Install the **Tailscale** app from the App Store / Play Store, log in
against `https://your-headscale.example`, paste the same pre-auth key.
GOTG's API client now connects to `http://pond-living-room:4000`
regardless of the phone's actual network.

### 5. (Optional) `LocalOnly` actions

The trust gate refuses `RemotePolicy::LocalOnly` actions when the request
originates from a non-private IP. With Tailscale, all traffic appears to
come from the `100.64.0.0/10` mesh range, which is RFC1918-private.
`origin_from_socket` in `trust_gate.rs` treats those as `Local` already.

If you want stricter "must be on home Wi-Fi" semantics, add a Tailscale
ACL tag (e.g. `tag:home`) and parse the `Tailscale-Tag` HTTP header in a
custom `Origin` extractor.

## What this gives the user

- The phone can reach the pond from anywhere with internet — coffee shop,
  cellular, hotel Wi-Fi.
- No third-party server sees plaintext.
- Even Headscale only sees connection metadata (timing, peer IDs); the
  payload is end-to-end encrypted by WireGuard.
- If a user prefers not to run a VPS, they can skip Headscale and use
  port forwarding on their home router with a DDNS name. The pond's TLS
  + signed-envelope auth still protects every privileged action.

## Migration story

Already-paired GOTG instances from PR #73 do not have a public key on
file. Until they re-pair:

- All Ambient and Personal endpoints continue to work normally.
- Privileged endpoints will return `408 Request Timeout` because the WS
  intent never resolves (the device has no signing key to assert with).
- The desktop app should display a banner: "GOTG needs to be re-paired
  to authorise privileged actions."

Re-pairing is idempotent thanks to PR #73's prior-session-revocation fix.
