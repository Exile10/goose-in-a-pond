# Embedded remote access with pinned HTTPS

## Connection model

The Android and iOS companion pairs on the Pond's directly attached LAN. Once paired,
it uses HTTPS on the LAN at home and HTTPS inside Tailscale/WireGuard away from
home. HTTPS is HTTP over TLS; WireGuard is an additional encrypted transport.
TLS terminates on the Pond. No cloud service holds its private key.

GOTG embeds a userspace Tailscale/WireGuard node on Android and iOS. The Pond
bundles the same pinned Go networking module as a supervised helper. No separate
Tailscale app, system VPN profile, or user Tailscale account is required. After
local pairing, choose **Enable remote access** or **Keep local only**. Local-only
profiles do not start a node or contact a coordination service. Remote operation
uses only the explicitly configured Goose-operated Headscale and DERP endpoints;
there is no fallback to hosted Tailscale or a separate VPN app.

This branch prepares a locally tested pilot deployment. Public domains and hosting
are still prerequisites for cellular use. Follow
[the deployment guide](../deploy/remote-access/README.md) to provision a pilot
household and obtain local Pond approval. The enrollment service alone holds
Headscale administration credentials. The Pond signs short-lived, single-use
approvals binding its household, paired phone and pending network registration.
Replacement phones require local pairing and explicit approval; there is no cloud
account-recovery bypass.

Source-masking proxies and subnet routers are unsupported for pairing. Public
listeners classify the actual TCP peer, never forwarding headers. Embedded traffic
enters through a private Unix socket whose trusted helper supplies the real remote
peer identity. It remains remote for handshake and local-management guards. Do not
publish the companion listener directly on the public internet. Default-deny
coordinator policy permits an approved phone to reach only its own Pond HTTPS port.

## Listeners and local tools

- HTTP binds exclusively to `127.0.0.1`, normally port 4000. Desktop assets,
  development pages, CLI, and OAuth callback integration remain local.
- HTTPS binds to all IPv4 interfaces, normally port 4443, and serves the API.
  `serve --https-port PORT` overrides its starting port. Each listener tries ten
  consecutive ports. Neither listener falls back to plaintext network access.
- Read `<data_dir>/.runtime_api_port` and `.runtime_https_port` for actual ports.
  `/api/v1/system/info` publishes `https_port` and `tls_spki_sha256`.
- mDNS `_pond._tcp.local.` publishes the HTTPS port and `scheme=https`.
  Discovery supplies candidates; it never supplies trusted keys.

To view the dashboard from another computer, use an authenticated SSH tunnel to
the loopback HTTP port. Preserve the existing OAuth redirect port. Such tunnels
are administrative access, not a supported remote mobile pairing mechanism.

## Pairing and identity

Open the local dashboard or run the `pairing` CLI command. The dashboard, CLI,
and startup output use the same payload contract:

```text
pond://pair?v=2&scheme=https&host=<hostname>.local&port=<https-port>&code=<6-digits>&pin=<url-encoded-sha256/base64-SPKI>&ip=<LAN-address>&ts=<tailnet-address>
```

`ip` and `ts` are optional. The pin hashes DER SubjectPublicKeyInfo, not the
certificate. Manual pairing requires the HTTPS address, full `sha256/...` pin,
and pairing code displayed locally. Old HTTP-only profiles need a fresh local
scan; an unauthenticated HTTP response cannot establish trust.

The phone stages the pin before contacting the Pond. It probes only local
candidates for initial pairing. The server independently guards legacy handshake,
challenge initialization, and verification, returning 403 `pairing_requires_lan`
for tailnet or unclassifiable peers. Knowing a code or starting a challenge at
home does not permit completing it remotely. Code issuance remains loopback-only.
Existing sessions and refresh tokens continue to work remotely.

Credentials and the pin are stored together in native SecureStore. Non-secret
addresses are stored separately and associated with that secure profile. Failed
pairing does not overwrite the previous secure identity. Cancellation clears
staged native trust and restores the previous profile.

## Certificate lifecycle

The Pond stores one atomic key/certificate bundle at `tls/identity.json` under
its data directory. The directory is mode 0700 and identity is mode 0600 on Unix.
An exclusive lifetime lock prevents competing writers. Missing material in an
existing directory, invalid JSON, a mismatched key, unsafe permissions, or a
symlink produces a visible startup failure. Restore the existing bundle from a
secure backup; do not delete it to silence an error.

The ECDSA P-256 key persists. Certificates are self-signed, valid for one year,
and renewed within 30 days of expiry or when current address SANs change. The
server checks every 30 seconds and reloads rustls without changing the pin. Key
replacement requires local re-pairing on each phone. This transport identity is
separate from any production signing key and requires no database migration.

## Roaming and failures

A shared manager selects endpoints for REST and foreground SSE. On Wi-Fi it
prefers pinned local addresses, uses mDNS to find a changed address, then tries
the authenticated embedded address when remote access is enabled. Cellular uses
the embedded node; local-only profiles remain disconnected away from home.

The node's resolvers are dialled **concurrently**, first to answer wins. They used
to be tried in order with a three-second budget each, and a carrier showed why
that is not enough: Safaricom reports two resolvers for its LTE network and the
first refuses DNS over TCP, so every lookup spent its budget on a server that
would never answer and the node resolved nothing on cellular while working on
Wi-Fi. Nor is the transport assumed: each server is tried over TCP and UDP at
once, and whichever proves itself first is used. A home gateway was found that
refuses DNS over TCP on every resolver it advertises while answering UDP, and
there racing servers cannot help (#394). UDP is connectionless, so a dial proves
nothing; reachability over UDP is shown by a real root-zone query whose random
id comes back. Network changes and foreground
resume re-evaluate the choice; background probing pauses. Recovery is coalesced,
uses a capped backoff, and stops after six failed attempts until another trigger
or a manual retry. NetInfo does not perform external reachability probes or
collect SSIDs.
On Wi-Fi, six bounded local rechecks also allow a connected phone to return from
tailnet after a temporary LAN outage without waiting for a network-change event.

Reads retry at most once after recovery. Writes and refresh-token rotations are
never replayed after ambiguous transport failures. A failed write may have
succeeded on the Pond; inspect the result before repeating it. Notification IDs
are deduplicated across stream reconnections.

The Android factory is installed before React Native and Expo initialization.
Both Expo fetch and React Native XHR use it. The configured pin, certificate
validity, and hostname must match before a Pond request is sent. Cross-origin
redirects and plaintext Pond URLs are rejected. Unrelated HTTPS traffic retains
platform trust validation. Release builds have no cleartext exception; debug
builds permit loopback HTTP for Metro through `adb reverse` only. Run Expo
prebuild to regenerate native integration from `plugins/with-pond-tls.js`.

On iOS, both Expo fetch and React Native XHR install the shared Pond URL protocol
before initialization. Configured Pond requests and every request to the embedded address ranges use this
protocol; unconfigured remote endpoints fail closed. Each accepted request uses an
ephemeral session with SPKI, validity, and hostname validation. Changing trust
cancels active requests, including SSE. Unrelated traffic uses normal platform
trust. Expo prebuild recreates the source files, bridge, and Xcode integration.
On iOS 17+, scoped ATS exceptions for `100.64.0.0/10` and
`fd7a:115c:a1e0::/48` allow native verification of the Pond's self-signed identity.
They do not replace native HTTPS, pin, hostname or date validation. Browser pinning
is outside this implementation. iOS 16.4 remains the build minimum, with its older
proxy path still awaiting runtime acceptance.

### Enabling, replacing and lapsing (2026-09-21)

Enabling remote access inspects the coordinator first and returns the existing
enrollment when the device is already **active and still holds the identity it
enrolled with**. Pressing the button on a pond where remote access already works
is a no-op rather than a conflict. A mismatched identity -- a phone that re-paired
and regenerated its tailnet keys -- is a real conflict: the enrollment is refused
with `409` and the app points at recovery.

Recovery replaces an enrollment. The coordinator replaces one that has been stood
down rather than a live one, so the Pond revokes the existing enrollment itself
and then replaces it, and **only after a person has approved the replacement at
the Pond**. It is not done at request time: that route needs only a LAN peer and
a bearer token, so revoking there would let anyone with both drop the household's
remote access without approving anything. The revision is re-read from the
stand-down's own answer, because standing an enrollment down gives it a new one --
assuming otherwise cost a household its enrollment without a replacement.

Remote access lapses after thirty days without the device authenticating from the
household LAN; see `docs/auth-network-posture.md`. The deadline is reported in the
remote configuration and the app warns from a week out.

A failure reports which kind it is. The helper exits 3 when the coordinator
refused the request and 1 when it could not be reached, so `register_phone`
answers `409` for a decision and `503` for an outage, and the app can tell a
household whose phone is already enrolled from one whose coordinator is
unreachable. Every layer carries the cause it was given: the Pond captures the
helper's stderr, the helper prints `Submit`'s error, and `Submit` carries the
coordinator's status and its error identifier.

### Disabling and signing out

Disabling remote access stops local networking and preserves the pairing. Logout
waits for acknowledged Pond revocation before clearing credentials, and also
removes the device from the registry so it does not linger as one that is merely
offline. The Pond queues
network revocation durably and retries while coordination is unavailable; application
session and refresh credentials are revoked together. Corrupt identity files cause
visible failure. Restore the private identity backup rather than deleting it to
create an unrelated household.

### Cached network map on phones (2026-10-05)

A phone's embedded node keeps its last network map on disk and, at a cold start,
runs from it at once instead of waiting for the coordinator. Without it, a phone
that cannot reach the coordinator straight away cannot reach its Pond either, even
when the Pond and the relay are both up. tailscale (v1.102.4, pinned in
`native/pondnet/go.mod`) writes the cache only for a node the coordinator grants
the `cache-network-maps` node attribute. Before this change no node had it, which
is why phones logged `load netmap from cache: netmap cache is not available` at
every start.

**What it stores.** The node's own entry, its peers, the relay map, DNS
configuration, the packet filter and user profiles, one file per item, mode
`0600` in a `0700` directory at `<state>/profile-data/<profile>/netmap-cache/`.
It holds no private keys; those stay in the identity store. The tsnet test in
`native/pondnet/netmapcache_test.go` asserts the file modes and the absence of
private key text.

**Where.** `<state>` is the node's private directory, which is outside every
backup: Android `noBackupFilesDir/pond-network/<profile>`, iOS Application Support
marked `isExcludedFromBackup`. A restored phone starts without a cache and waits
for a live map.

**Who gets it.** Phones only. The enrollment service adds one `nodeAttrs` entry to
the policy it installs. Its targets are the addresses of every active, verified
phone across households, sorted so an unchanged store gives an unchanged entry:

```json
"nodeAttrs": [{"target": ["100.64.0.2", "100.64.0.3"], "attr": ["cache-network-maps"]}]
```

With no such phone the list is empty. A Pond's address is never a target.

**Why not the Pond.** The Pond enforces access: its packet filter decides which
phones may reach it. A Pond starting from a cached map would enforce the rules as
they were when the map was written, so a phone revoked while the Pond was down
could get through until the coordinator answered. The Pond helper (`cmd/pondnet`)
therefore sets `TS_USE_CACHED_NETMAP=false` before it starts its node, so it
neither reads nor writes a cache even if a policy granted it one.

**The stale-peer window.** A phone running from its cache may hold peers and
rules the coordinator has since changed, until its first live map replaces them,
which happens as soon as the coordinator answers. That is acceptable because the
phone decides nothing about access: the Pond always runs from a live map, so a
revoked phone's packets are dropped there, and a removed phone's application
credentials are revoked with it. A cache cannot get a phone anything the Pond does
not currently allow.

**A removed phone.** tailscale does not erase the cache when the coordinator stops
accepting a node. A removed phone still loads it at every start, reports
`Running` on it, and is refused only when the coordinator answers its
registration with a login URL. The phone learns it was forgotten only from its
Pond, which it can reach only on the home network, so without help it would carry
the Pond's addresses and the access rules indefinitely. `mobile.Start` therefore
runs `Node.ForgetNetworkMapWhenRefused` alongside the node: on a login URL,
whether announced on the IPN bus or already in the status when the watch begins,
it clears and removes the cache, keeps the identity, and writes one event-log
line, `the coordinator no longer accepts this device, so its cached network map
was erased`. A node enrolling for the first time has no cache and is left alone.
Measured on the Galaxy A57 on 2026-10-05: after the phone was removed in the
dashboard, a cold start loaded the cache, got `machineAuthorized=false;
authURL=true` from the coordinator, and stayed `disconnected` from the Pond; the
cache stayed on disk until this watcher was added.

**Erasing it.** `mobile.Disable(directory)` stops the node and erases the cache.
It asks the running backend to clear it (`clear-netmap-cache`, which also drops
the backend's in-memory copy), stops the node, then removes the cache directories
from disk. The removal comes last so a map that arrived in between does not
survive, and because the backend's own call does not report a failed delete. A
failure is returned and written to the event log. The identity is kept, so
enabling again needs no new enrollment. `mobile.Stop` still keeps the cache, so a
profile switch or a service stop does not throw it away. The companion app's
Android and iOS plugins call `Disable` when remote access is switched off or the
pairing is forgotten (goose-on-the-go `feature/phone-netmap-cache`, 2026-10-05).

**Kill switch.** `TS_USE_CACHED_NETMAP=false` in a node's environment turns the
cache off. tailscale reads the knob on every check, and with it off it neither
loads nor writes a cache.

**Rollout.** The attribute is part of the coordinator's policy, so it takes effect
only once the enrollment service is redeployed; the service reinstalls its policy
on start and on every enrollment change. Headscale 0.29 accepts `nodeAttrs` with
address targets and passes this attribute through. Whether a phone on the deployed
coordinator actually receives it is checked by the live Headscale test
(`enrollment/headscale_live_test.go`), not by the default test run, and that has
not yet been run against Headscale 0.29.3.

## Verification

Use the security tests and `scripts/live-test.sh` against scratch data, including
a restart with populated databases. Device acceptance additionally requires
physical Android/iOS phones and a Jetson: home LAN, cellular with embedded networking, another Wi-Fi,
and home LAN again. Exercise app/server restarts, LAN address changes, unavailable
coordination/relay service, certificate renewal, and incorrect-pin rejection by both REST and SSE.
Disconnect the separate Tailscale app for these tests. A build or unit-test pass
does not establish physical roaming acceptance. See the current
[embedded verification ledger](embedded-remote-access-verification.md) for measured
simulator, scratch-Pond, backup/restore and remaining hardware results.

This branch includes W3 authorization checks for notification ownership, bearer
revocation and protected diagnostics. HTTPS is independent of those checks. See
[the security posture](auth-network-posture.md) for the implemented contract and
remaining bearer-token risks.

## Backing up the Pond's irreplaceable state

The household authority is an Ed25519 private key at
`<data_dir>/embedded-network/authority/identity.json`. Losing it means a new household:
there is no cloud account recovery, and every paired device must pair again. The HTTPS
identity (`tls/identity.json`) and the WireGuard node state
(`embedded-network/node/tailscaled.state`) must be restored *with* it, because trust is
the combination and not any one of the three.

`scripts/pond-snapshot.py` streams a tar of exactly that state to standard output:
the three items above, `secrets/`, `secrets.json`, the schedules, and consistent copies
of `pond_system.db` and `pond_vectors.db` taken through SQLite's online backup API so
the Pond keeps serving. It deliberately omits `models/`, `hf_cache/`, `bin/`, `lib/` and
the logs, which are gigabytes and all refetchable; the remainder is under a megabyte.

Run it from an operator machine so the Pond needs no additional software, no elevated
privileges and no writable scratch space, and so the archive lands somewhere the Pond's
own disk failure cannot reach:

```
ssh <pond> 'python3 -' < scripts/pond-snapshot.py | age -R <recipients> -o pond-state.tar.age
```

Encrypting on the operator machine to an age recipient keeps the private key off the
Pond, matching the coordinator's arrangement in `deploy/remote-access/`.

One trap when verifying such an archive: the databases are produced by SQLite's backup
API, so they carry a WAL journal-mode header but no `-wal` sidecar. They open normally,
but an explicit read-only open (`file:...?mode=ro`) fails with `unable to open database
file`, because SQLite cannot create the write-ahead index. That is a property of the
verification command, not a corrupt backup; check integrity with an ordinary connection.
