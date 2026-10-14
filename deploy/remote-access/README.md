# Goose remote-access pilot deployment

## Scope and trust boundary

This package prepares Goose-operated Headscale, a signed enrollment service, Caddy,
and Headscale's embedded DERP/STUN relay. It is not a public deployment. The public
domains, hosting, backup destination, monitoring ownership, and incident procedures
must be selected before enabling public ingress or claiming cellular acceptance.

Application HTTPS terminates on the Pond. Headscale receives node metadata and the
enrollment service stores household public keys, device/node mappings, permissions,
and replay records. DERP forwards WireGuard ciphertext. This does not complete the
separate application authorization milestone.

Every image is pinned by digest, and every container runs as UID 65532 with a
read-only root, no capabilities and `no-new-privileges`. Caddy listens on 8080 and
8443 inside its container and Docker maps 80 and 443 onto them. It keeps one
capability, `NET_BIND_SERVICE`, only because its image marks the binary with it and
the kernel refuses to start such a binary without it. Three networks separate the traffic (2026-09-30): `edge` carries the
gateway and the Headscale control plane, `enroll` the gateway and the enrollment
service, and the internal `admin` network the enrollment service and Headscale
alone. What the split keeps off `edge` is the admin key, not the admin API
(corrected 2026-10-12): Headscale serves its control plane and its administration
API on one listener, `:8080`, attached to both `edge` and `admin`, so anything on
`edge`, the gateway included, can reach `/api/v1` there. Only enrollment holds the
key, and it talks to Headscale over `admin`, as `headscale-admin`, so the key never
crosses the gateway's network. Publicly, Caddy answers `404` for `/api/*`,
`/metrics`, `/debug/*`, `/swagger*` and `/web*` on the Headscale host before
anything is proxied; Headscale's metrics and gRPC listeners are bound to its own
loopback. Both sites send HSTS. No API key belongs in an image, environment file,
repository, or phone. The
coordinator is Headscale, run here or by the household; nothing depends on
Tailscale's hosted control plane, with one exception under Residual risks.

## Upgrading to the hardened layout (2026-09-30)

Existing state was written by root. Before recreating the stack, stop it and hand
the volumes to the unprivileged user once:

```sh
docker compose down
sudo chown -R 65532:65532 runtime/headscale runtime/caddy-data runtime/caddy-config
sudo chown 65532:65532 runtime/headscale.yaml && sudo chmod 400 runtime/headscale.yaml
install -m 600 backup.env.example runtime/secrets/backup.env   # then set BACKUP_OFFSITE
docker compose up -d
```

Then check that the gateway renews certificates, `/health` answers on the
enrollment host, a paired phone still connects, and `sudo ./rotate-admin-key.sh` and
`sudo ./backup.sh` complete. Deploy this service before any Pond that sends a household
invite.

## First boot

Use a dedicated host. Copy `headscale.example.yaml` to
`runtime/headscale.yaml`, replacing `server_url` with the selected HTTPS origin and
`dns.base_domain` with an operator-owned namespace. Create private persistent
`runtime/headscale`, `runtime/enrollment`, and `runtime/secrets` directories. Give
the enrollment directory to UID/GID 65532 with mode 0700; restrict the others to
root. Set `HEADSCALE_HOST` and `ENROLLMENT_HOST` in a deployment-local `.env` file.
These names are public configuration, not credentials.

1. Run `docker compose up -d headscale` from this directory. Leave the gateway off.
2. Create a short-lived administration key using
   `docker compose exec -T headscale headscale apikeys create --expiration 24h`.
   Redirect its output directly to `runtime/secrets/headscale_admin` under umask
   077, then give that file to UID/GID 65532 with mode 0400. Do not copy the value
   into commands, tickets, logs, or shell history. A Compose file secret is a bind
   mount rather than a copy, so the container reads the host file's own ownership:
   left root-only it is unreadable to the non-root enrollment user, and enrollment
   never becomes healthy. Note also that `docker compose exec -T` consumes standard
   input, which silently truncates the remainder of a script piped to
   `ssh <host> bash -s`; redirect it from `/dev/null` inside such scripts.
3. Build and start enrollment with `docker compose up -d --build enrollment`.
   It installs the complete default-deny policy before becoming healthy.
4. Start the gateway with `docker compose up -d gateway`. It requires healthy
   enrollment at startup. Check HTTPS health and confirm that `/api/v1/user` on
   the public Headscale hostname returns 404.
5. Rotate the administration key before expiry with
   `sudo ./rotate-admin-key.sh [90d]`; it needs root to read and replace the secret,
   which belongs to UID 65532. The key is read once at startup, so the order is
   load-bearing: mint, install, recreate enrollment, confirm it reconciles with the
   coordinator, and only then expire the previous key. Reversing the last two locks
   the service out of the coordinator. Any failure before that restores the previous
   key and expires the new one.

The Compose port bindings default to loopback. Setting `HTTPS_BIND=0.0.0.0` and
`STUN_BIND=0.0.0.0` is an explicit public-deployment step and is outside this
preparation milestone. Configure host firewall and DNS before doing so. The
control Docker network is not an authorization boundary against host operators;
only trusted operators may manage this host.

## How a household joins

A household registers itself with an invite from the operator (2026-09-30). The
Pond sends its public key, companion port and invite, signed with that key, to
`/v1/household`; the service names the household by digesting the key rather than
trusting what was sent, spends the invite, creates its coordinator user and stores
it, in one write. Repeating the call answers with the same household and needs no
second invite, so a lost response costs nothing; the invite a household already
spent also admits it again.

Issue an invite on the host running the service:

```sh
docker compose exec enrollment /pond-enrollment --state /state --issue-invite --expires 72h
```

It prints `giap-inv1-XXXX-XXXX-...` once; only its digest is stored. It lasts seven
days by default and at most 30, admits one household, and can be typed in any case
with or without its dashes. The household pastes it into Remote access on its Pond.
Refusals are a closed set: `invite_required`, `invite_invalid`, `invite_expired`,
`invite_used`. The command talks to the running service over `admin.sock` in the
state directory, mode `0600`; `--admin-socket` moves it. Someone running their own
service issues invites from it the same way.

Admission used to prove possession of a key and nothing else, so anyone could
register households and consume this service's users and addresses; that is what
the invite now bounds. It is not what separates households: the policy is, and it
grants each phone its own Pond's HTTPS port and nothing more. The operator can
neither read household data nor use a Pond; it can deny or disrupt remote access,
which is why the self-hosted path stays. Registration is also rate limited per
source address, the source table is capped, and there are at most 1000 households.

An invite sent to the wrong person, or pasted somewhere it should not have been, can
be withdrawn until it is spent:

```sh
docker compose exec enrollment /pond-enrollment --state /state --revoke-invite giap-inv1-XXXX-...
```

The admin commands need the running service's state directory (`--state /state` in
the compose stack) or `--admin-socket`; without either they stop and say so rather
than looking for `admin.sock` in the current directory.

Deploy this service before any Pond that sends an invite. The registration is
decoded strictly, so an older service answers `400` to a registration carrying one;
a Pond sends none when the field is left empty.

A Pond's helper ignores fields it does not know in the service's answer to an approval
(2026-10-12). Helpers before that refuse such an answer outright, so do not add a field
to that answer until the Ponds this service admits run a helper from 2026-10-12 or
later. The answer's status set is closed either way: a new status still needs every
Pond updated first.

### Device certificates (2026-10-05)

A Pond imaged by the operator needs no invite. Imaging gives it a device key, and the
operator signs that key's public half, on the operator's own machine, with a
provisioning key; the certificate goes back onto the Pond. Its first registration
carries the certificate and a signature by the device key over the household it is
registering, so a proof seen in transit admits no other household. The service checks
the certificate against the provisioning keys it trusts, admits one household per
certificate, and records the admission in the same write that creates the household.

Make the provisioning key once, on the operator's machine, and keep it offline. The
tool builds from this repository (`go build -o pond-provision ./cmd/pond-provision` in
`native/pondnet`):

```sh
pond-provision keygen --key ~/jarida-provisioning.key
```

It prints the public half. Give the service that, and only that, by adding to the
enrollment command in `compose.yaml`; repeat the flag to trust a second key while
rotating:

```
--provisioning-key <base64 public key>
```

Without `--provisioning-key` the service ignores certificates and only invites admit.
Imaging a Pond creates its device key with `pondnet --device-action create`, signs
the printed public key with `pond-provision sign --key ... --device-public-key ...`,
and installs the result with `pondnet --device-action install`. `scripts/giap.sh
provision` runs those steps over SSH; it is added with the Pond-side change. The
device key never leaves the Pond; the provisioning key never leaves the operator's
machine.

Certificate refusals are a closed set: `device_certificate_invalid` (any check failed,
deliberately not saying which), `device_revoked`, `device_used`. A Pond whose
certificate is refused can still be admitted with an invite: the certificate is tried
first and the invite is the fallback.

Revoke a lost or stolen Pond's certificate by its serial, which `pondnet
--device-action install` printed when it was imaged:

```sh
docker compose exec enrollment /pond-enrollment --state /state --revoke-device <serial>
```

Revocation stops the certificate admitting a household from then on. A household it
already admitted keeps its registration, since it holds its own key; the Pond's tailnet
access is removed through the household's own device removal, or by the operator.

A certificate has no expiry. It stops admitting only when its serial is revoked or
once it has admitted its one household. If the provisioning key is lost or exposed,
make a new one, drop the old one's `--provisioning-key` from the enrollment command,
and redeploy; every unspent certificate the old key signed then stops admitting, and
those Ponds need an invite or a certificate signed by the new key.

A certificate does not separate households, any more than an invite does: the policy
does. What it bounds is the same thing, who can consume this service's households and
addresses, without a code a person has to carry. Deploy the service with this support
before any provisioned Pond registers: an older service answers `400` to a
registration carrying a certificate.

Source addresses come from the gateway's `X-Forwarded-For`, believed only from
`--trusted-proxy` (the pinned `172.31.250.0/28` compose network); without it every
client would share the gateway's budget. Only the last hop counts, IPv6 clients are
grouped by /64, and a full table refuses new sources instead of forgetting every
budget. Since 2026-10-12 a full table first forgets the sources whose limit holds
nobody back, those with no allowed request for ten minutes or whose budget has
refilled, and a refused request no longer counts as activity. A source that is still
rate limited is never forgotten: evicting the least recently used would let anyone
with enough addresses reset a victim's budget. Approvals are limited per source and,
after the signature is checked, per household.

The service enrolls at most 4096 devices across every household and refuses more
with `503 capacity`. Each enrolled device is a coordinator node the policy must
read, so this is what keeps the inventory readable; it is streamed node by node and
refused past 8192 nodes, leaving room for nodes added by hand. Before this, enough
registered nodes pushed the inventory past a 1 MiB read limit, after which every
policy update failed and revocations stopped taking effect. A full inventory runs to
about 33 MB, so the coordinator client's 10-second timeout bounds only the wait for
response headers, and each operation's own deadline bounds reading the body: 15
seconds for a request, 60 for startup and for each reconciliation, which reads the
inventory up to three times (2026-10-12). Matching devices to nodes is indexed by
node ID and machine key rather than comparing every pair while the store is locked.

The manual path below remains for an operator-created household and for anyone
running their own coordination service.

## Provisioning a household by hand

On the Pond's local Pairing page, choose **Prepare household identity**. Transfer
only its household identifier and public key to the pilot operator. The private
Ed25519 identity stays under the Pond's `embedded-network/authority` directory,
separate from HTTPS and WireGuard keys.

Create a dedicated Headscale user for the household with `headscale users create`.
Record its numeric ID. Stop enrollment while provisioning because its state is
exclusively locked. Run the enrollment image with the same state mount and:

```
--state /state --provision HOUSEHOLD_ID --public-key PUBLIC_KEY --user-id USER_ID --https-port 4443
```

Use the Pond's actual HTTPS port if it differs from 4443. Restart enrollment and
confirm health. On the local Pond, enter the two Goose HTTPS origins and approve
activation. The Pond signs its pending Headscale registration. After local phone
pairing, GOTG's **Enable remote access** requests a device-bound registration via
the pinned local Pond. **Keep local only** makes no coordination contact.

Approvals expire, are single-use, and bind the household, device, role, exact
Headscale pending authorization ID and public machine key. The machine key is
available before authorization; the returned node is validated before policy grants
access. Clients cannot supply the administrative user or ACL. An ambiguous
registration remains pending and is never replayed. Reconciliation recovers only
an exact, unique match for the approved machine identity in the correct household.
It cannot claim a preexisting unmanaged node or undo a definite registration
rejection. Those failed transitions require explicit local recovery. A `failed`
record keeps its machine key as a tombstone, like a revoked one (2026-10-12): when
the coordinator registered a node the service then refused, for its tags, routes or
owner, the node is deleted straight away and again by every reconciliation until it
is gone. It used to stay registered, uncounted against the enrollment capacity.
Deletion still requires the household's own coordinator user, so a node registered
to another user is kept and logged.
Revocation wins over pending recovery and retains the machine identity as a
tombstone, including when registration appears after the first revocation attempt.
Do not erase the enrollment database to retry.

The enrollment service exclusively owns the policy and registered node mappings.
Do not manually reassign users, nodes, addresses, tags, routes, or policies behind
it. Cross-household traffic, phone-to-phone traffic, other Pond ports, subnet
routing, and exit nodes have no allow rule.

The policy also grants the `cache-network-maps` node attribute to every active,
verified phone's address, never a Pond's (2026-10-05), so a phone can start from
its last network map while the coordinator is out of reach. The service reinstalls
its policy at start, so the grant takes effect when the service is redeployed. See
"Cached network map on phones" in `docs/remote-access.md`.

## Backups and restoration

Back up the Pond's complete private data directory using its existing backup
procedure. Restoring the household authority, HTTPS identity, and WireGuard state
together preserves trust. Loss of the authority requires its backup or a new
household; there is no cloud account recovery.

For infrastructure backup, stop gateway and enrollment first, then Headscale. Take
a coordinated, encrypted backup of both stores and their accompanying files,
Headscale Noise/DERP identities, configuration, and Caddy state. The two stores are
not alike: Headscale keeps SQLite, while enrollment keeps `state.json`, written by
atomic rename under a `store.lock` flock. Copy the complete Headscale directory,
including any SQLite WAL/SHM files. An independent copy of only enrollment or only
Headscale can restore inconsistent authorization mappings. Keep the backup key
outside this host; never store plaintext archives in Git.

Backups must leave this host, and `sudo ./backup.sh` refuses to run until
`BACKUP_OFFSITE` says how (the reference unit reads it from
`runtime/secrets/backup.env`). It needs root to read volumes owned by UID 65532. A
run by hand reads the value as systemd does, without surrounding whitespace, a
trailing carriage return, or one pair of matching quotes (2026-10-12); it used to
keep the quotes and refuse `BACKUP_OFFSITE="pull"`. Set it to `user@host:/directory`
and each archive is copied there with rsync over SSH and checked there before
anything local is pruned: a second, checksumming rsync dry run must find nothing to
change. That needs only rsync on the far side; the size check it replaced (2026-10-12)
ran GNU `stat` there. Set it to `pull` when another machine collects `runtime/backups`
and verifies what it collected, as the pilot's Mac does; this host then holds no
credential to anywhere.

`./backup.sh` performs exactly this sequence and is driven by the reference units in
`systemd/`, whose paths assume a deployment at `/opt/goose-remote-access`: it stops gateway, enrollment and Headscale in order, checks
both stores while nothing is writing, encrypts a single archive to an age recipient
whose private key is deliberately absent from this host, restarts in reverse order
through an EXIT trap, and prunes to `KEEP` archives, in `pull` mode only those
collected. It refuses to run at all rather
than write an unencrypted archive. Expect roughly fifteen seconds of downtime per
run; a connected Pond reconnects afterwards with its machine identity retained.

Restore into fresh isolated directories with the original permissions, using the
pinned versions, and recreate the containers with those directories mounted. Do
not replace files beneath a reused Docker Desktop bind mount: the local mobile
restore fixture reproduced stale-file failures there despite valid SQLite data.
Verify Headscale's SQLite integrity, and that the enrollment `state.json` parses,
before startup. Start Headscale and enrollment with the gateway still off. Verify policy
installation, two-household isolation, replay rejection, and a revoked device.
Only then permit ingress. Provisioning credentials should be replaced after a
restore. Unit tests cover authority persistence and corrupt files; the complete
infrastructure backup/restore drill passed locally for a populated household/user
mapping. The Android emulator and existing iOS simulator also pass active mobile
reconnection after full infrastructure restoration: retained machine identity,
authenticated REST, and incremental SSE work with the restored state. These local
fixtures do not establish public cellular availability.

Key rotation (`sudo ./rotate-admin-key.sh`) expires exactly the key enrollment was
using, named by its own prefix, and only after Headscale lists the new one and
enrollment is healthy on it. It used to scrape the key table and expire every key
it could parse. Since 2026-10-12 it writes only the key line of `apikeys create`
into the secret, and fails when there is none. Docker's health check passes on any
`200`, and a service whose key the coordinator refuses still answers `200` with
`"degraded":true`, so the script then reads enrollment's own `/health` body for up
to a minute, from inside the gateway with its busybox `wget` (the enrollment image
has no shell), and treats `"degraded":true` as failure. Until that passes, every
failure, an interruption included, restores the previous key, recreates enrollment
on it and expires the new key. The gateway must be running.

### What a pull-mode collector must do (2026-10-12)

In `pull` mode this host prunes only what it can see was collected. The contract:

- After the collector has copied `control-plane-<stamp>.tar.age` and verified its
  copy, it writes an empty `control-plane-<stamp>.tar.age.collected` beside the
  archive in `runtime/backups` on this host, as the account it reads the archives
  with. It never writes a marker for a copy it did not verify.
- `backup.sh` keeps the newest `KEEP` archives regardless, and of the older ones
  deletes only those with a marker, together with the marker. An archive with no
  marker is kept however old it is.
- If no marker is younger than `COLLECTED_WITHIN_DAYS` (default 3) while some archive
  is older than that, the run still writes its archive and then exits non-zero, so
  `goose-backup.service` shows failed. A new host passes until its first archive is
  that old.

The collector lives on the operator's machine, outside this repository. Add this
after its pull, with `HOST` and `REMOTE` set to the SSH destination and this host's
`runtime/backups` path, and `LOCAL` to the directory it pulled into. The check
compares modification times and permissions as well as content, so the pull must
keep them: pull with `rsync -a`. A copy taken with `scp` or a plain `rsync` differs in
its times, is never marked, and `backup.sh` fails once `COLLECTED_WITHIN_DAYS` pass.

```sh
for copy in "$LOCAL"/control-plane-*.tar.age; do
  name="$(basename "$copy")"
  # Verified: a checksumming dry run of this host's archive against the copy changes
  # nothing, and the rsync itself succeeded (an archive pruned meanwhile fails it).
  if changes="$(rsync -an --checksum --itemize-changes "$HOST:$REMOTE/$name" "$LOCAL/")" \
     && [ -z "$changes" ]; then
    ssh "$HOST" touch -- "$REMOTE/$name.collected"
  else
    echo "not marking $name: the copy is not verified" >&2
  fi
done
```

Where the collector also holds the age identity, decrypting each copy to
`tar -t` before marking it is the stronger check.

## Health, logs, and remaining verification

Enrollment exposes `/health`, limits request bodies to 8 KiB, concurrent operations
to eight, and public requests to 10/s with a burst of 20. A coordinator that cannot
be reconciled leaves the service running but degraded, reported as
`"degraded":true` on `/health` (still 200, so the gateway keeps serving) and in the
log, until a reconciliation succeeds; it used to exit and restart into the same
failure. Since 2026-10-12 that holds after startup too: a failed reconciliation
(every 30 seconds) or policy update marks the service degraded, so a later outage,
an admin key the coordinator refuses, or an inventory past `MaxNodes` shows on
`/health` instead of leaving it at `ok`. The transition is logged once with its
cause, and each deferred reconciliation logs its error. A signed approval is spent as soon as its signature checks out, even if it
is then refused. Logs name transitions and
failures, never signatures, auth IDs, QR payloads, or credentials. Container logs
rotate at 10 MiB with three files. Alerts, off-host metrics, disk thresholds, and
credential-expiry notifications must be configured for public operation.

When checking the embedded DERP relay, verify STUN with a well-formed request. The
relay is Tailscale's STUN server, which deliberately drops binding requests carrying
neither a SOFTWARE attribute nor a FINGERPRINT so it cannot be used as a general
reflector. A minimal twenty-byte binding request therefore times out against a
perfectly healthy relay, including from the host itself and against the container
address, which reads exactly like a firewall or publishing fault. Confirm the
listener with `ss -lunp` inside the container's network namespace before suspecting
the network, and confirm `derp.server` in the Headscale log at `debug` level: the
packaged `warn` level suppresses the line announcing the STUN listener.

See `../../docs/embedded-remote-access-verification.md` for measured local results
and remaining gaps. Do not infer physical-phone roaming or complete deployment
readiness from unit tests or a successful container build.


### Revocation and coordinator drift

The Pond durably queues authenticated phone revocation before acknowledging logout.
Retries continue after restart and while embedded networking is disabled. Local-only
households with no enrollment configuration make no coordinator request. Paired
device IDs map to domain-separated SHA-256 enrollment IDs, preserving compatibility
with legacy device identifiers without exposing them to the coordinator.

The enrollment service retains a revocation tombstone to reject delayed enrollment.
It clears retired node addresses after successful removal so a subsequently reused
address cannot inherit permissions or prevent a legitimate new registration. Before
applying policy, it verifies active node keys, IDs, owner, address and absence of tags
or approved routes against Headscale. Revocation never deletes a mismatched node
from a restored or replaced coordinator database.

### Explicit replacement protocol

The private Pond authority helper now accepts `--authority-action inspect` and
`--authority-action replace` in addition to initial enrollment and revocation.
Inspection signs a single-use request for one device and returns its current
`revision`. Replacement signs the same household/device, `role: "phone"`, a new
pending `authId` and `machineKey`, optional `nodeKey`, and `expectedRevision` from
that inspection. The helper supplies its own household, nonce and two-minute expiry;
no administration credential is involved.

`--authority-action register` reads `{"invite": "..."}` on stdin (2026-09-30). An
empty or absent value, or no input at all, sends no invite, so an already-registered
household keeps working. The invite is never a command-line argument, because other
accounts on the host can read those.

This is an operator authority, not a public recovery endpoint. Replacing an `active`
enrollment must follow fresh local pairing and explicit local review of the exact
device and pending registration, and a caller must never replace one automatically.
Since 2026-10-05 the Pond replaces a `revoked` enrollment for the bearer's own device
without review, and only from the home network, because such a record holds no
working remote access and a first enrollment needs no review either. A `failed`
enrollment still needs review: each replacement retires a record against the
household's bounded budget, and a refused enrollment can be failed again at will.
Active/revoking records, reused machines, stale revisions,
expired approvals and replays fail closed. A newer revocation invalidates an older
replacement approval.

A replacement retires the old record so a late registration under its machine key
can still be deleted. Retired records are bounded at 256 per household and, since
2026-10-13, expire: every reconciliation (each 30 seconds) compares them with the
coordinator inventory it read before deleting anything, and a record is dropped once
reads at least 24 hours apart have all found no node holding its identity under the
household's user. The first such read stamps `absentSince` on the record; any read
that finds a node clears it, and that node is deleted as before. A node deleted in a
pass therefore still counts as present in that pass, and a registration still in
flight shows up well inside the 24 hours. Older state files load unchanged, their
records simply unstamped. The reverse does not hold: an older `pond-enrollment`
refuses a state file that holds `absentSince`, as it refuses any unknown field, and
restarts in a loop, and the first stamp lands within about 30 seconds of the upgrade.
Back up `/state` before deploying this version; rolling back needs that backup, or
`absentSince` removed from the state file. Before this a record was kept for good, so 256
replacements, each recovery and since 2026-10-05 each lapsed phone coming back,
left a household unable to replace any phone without an operator editing the store.

Once a record expires its machine key is no longer refused as `identity_retired`.
That is deliberate, and no list of expired keys is kept: the key belongs to an old
phone, no node holds it, and enrolling it again still needs a fresh approval signed
by the household's own Pond for that exact key, the same as any new phone. A key a
node does hold is still refused as `identity_already_registered`.

A household that still reaches the bound, through 256 replacements within about a
day, is refused with `409 retired_capacity` rather than the `replacement_conflict` a
stale or invalid approval gets, and the service logs it. It clears as records
expire. An older Pond treats the new reason like any other refusal, as before. A
current Pond answers the phone `503 {"error":"retired_capacity"}`, a status the app
already shows as a failure rather than pointing it at recovery, which would meet the
same bound, and an approved recovery refused this way stays on the dashboard's
review for ten minutes with the reason explained.

Current pilot limitations: the service protocol and helper are implemented and tested
against real Headscale, but the phone-to-local-dashboard recovery flow is unfinished.
It still needs credential revalidation at approval and cancellation/revocation race
checks. Legacy pending records without machine bindings cannot recover automatically.
Do not deploy publicly while those lifecycle acceptance items are open.

## Residual risks

**A Pond or phone may ask Tailscale's servers to resolve the coordinator's name.**
When the system resolver cannot resolve the Headscale host, tsnet's control client
falls back to the bootstrap-DNS endpoint of DERP servers from a map compiled into
the Tailscale module (`net/dnsfallback`, wired as `LookupIPFallback` in
`controlclient.NewDirect` and `controlhttp.(*Dialer).resolver` of v1.102.4).
Tailscale's default regions are always part of that map, whatever the coordinator's
own DERP map says, and there is no setting or build tag that turns the fallback off.
What such a request reveals is the client's address and the coordinator's host name;
it carries no household data and grants Tailscale nothing. It happens only when
normal DNS has failed. We document it rather than patch the module, and have drafted
a request for an upstream option in `docs/upstream/tailscale-dnsfallback.md`.
