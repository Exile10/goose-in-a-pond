# Embedded remote access: verification ledger

Date: 2026-09-20. Branch: `feature/embedded-tailnet` in both repositories.
Dependencies: Pond pinned HTTPS (PR 375) and authorization (PR 376); GOTG pinned
HTTPS (PR 38). Both origin and upstream main refs were refreshed before this work.

## Measured results

- Go race tests pass for the userspace CONNECT transport and enrollment service.
  Added authority persistence/signature checks, corrupt and missing identity
  rejection, storage-failure fail-closed behavior, and pending-Pond enrollment
  rejection.
- Real Headscale 0.29.3 registers two isolated households. Own-Pond port access
  succeeds; cross-household, phone-to-phone, and wrong-port access fails with
  actual listeners present on forbidden destinations.
- Forced encrypted-relay test passes with direct UDP disabled. A DISCO ping proves
  Goose DERP region 999 and no direct endpoint. The isolated Caddy test root is
  installed only inside the Go test process; production TLS validation is unchanged.
- Enrollment container builds from the pinned Go/distroless images. Caddy's pinned
  image validates the deployment configuration. The full Compose deployment boots with loopback-only published ports. Its TLS
  gateway rejects public administration paths and unsigned approvals. A stopped
  backup/restore into new volume directories preserves a real Headscale user,
  the provisioned household public-key mapping, and healthy startup. Active-node
  restore is additionally covered by the real Headscale service integration test.
- GOTG typechecking and 151 tests across 18 suites pass. Explicit opt-in,
  cancellation, no automatic enrollment-write replay, and atomic same-pairing
  address revisions have regression coverage. Lint: zero errors, 19 pre-existing
  warnings.
- Regenerated Android native sources compile and native unit tests pass. Both
  Android AAR and iOS device/simulator XCFramework were rebuilt from the pinned Go
  module. The full iOS ARM64 simulator Debug and unsigned device Release app builds pass.
  These builds must run serially: React Native replaces Debug/Release frameworks
  inside shared Pods directories, independently of DerivedData.
- Pond `cargo check -p pond-server --all-targets` passes. Four Rust bridge/lifecycle/revocation tests and five real-SQLite remote
  authorization integration tests pass. The production server target compiles. The dashboard bundle
  builds; new typed dashboard API methods have been added for local approval.

- Apple Foundation/Security checks linked with the current embedded library pass:
  SPKI pinning, renewal with the same key, validity, hostname, redirects, plaintext,
  incremental SSE, cancellation and cleared trust. These are core transport checks,
  not evidence of mobile embedded-proxy operation.

- The dashboard typecheck, production bundle and all 833 tests pass.
- The Galaxy A57 passes the isolated native app TLS/REST/XHR/SSE/persistence suite
  against a scratch Pond over direct home LAN. The production app remains intact.
- The existing iPhone simulator passes the same isolated app suite, including
  real scratch-Pond pairing, authenticated REST, refresh, SSE and process restart.
  Its transport fixtures use loopback; no physical iPhone claim is made.
- The bundled Linux ARM64 helper runs on the Jetson. Its production service remains
  active with the same PID (29038); this is not a complete Jetson server build.

- SQLite issuance and refresh now use transactions; authenticated device revocation
  also invalidates a refresh that completed while network revocation was queued.
  All 17 handshake tests and five API authorization integration tests pass. No
  migration is introduced. Go race tests cover pending CONNECT and active-stream
  cancellation on profile replacement.
- Existing PR 375 and main both fail Rust CI before compilation because the runner
  cannot authenticate the private llama-cpp-2 Git dependency. Both runs also show
  the same Matter esbuild 0.28.2 lockfile mismatch. Their security logs contain
  the same 21 RustSec advisory IDs, with no additional ID in the dependency PR.
  Frontend/desktop jobs in both runs fail npm peer resolution for HeroUI before
  Playwright executes. This was verified from
  runs 35473827371 and 35434808169; no access or security checks were weakened.

## Native transport continuation

- Refreshed origin and upstream main in both repositories, preserving the feature
  branches and unrelated edits.
- Fixed failed native activation on Android and iOS to stop partially initialized
  nodes. Target configuration now commits only after the proxy accepts its allowlist.
  iOS invalidates in-flight connections on failed as well as successful transitions.
- Found a system-network fallback in the pinned tsnet general-purpose dialer.
  Mobile CONNECT now uses the userspace WireGuard TCP stack exclusively. Go race
  tests and vet pass. Real Headscale tests pass again with this exact dialer,
  proving own-household access, cross-household/phone/port denial, forced DERP,
  backup restoration, replay rejection and revocation.
- Apple native transport checks pass through the production Go CONNECT proxy for
  two clients and SSE, public-key pins, hostname, expiry, redirects, plaintext,
  cancellation, failed activation, profile restoration and unavailable-proxy
  rejection. The test-only binding substitutes node lifecycle; this is macOS
  CFNetwork coverage, not complete mobile Headscale enrollment or iOS 16.4 evidence.
- Four Android native-client tests pass through the same production proxy with
  no skips. Proxy credentials are absent from the destination's HTTP headers.
- The existing iPhone simulator passes 19 first-run checks and three restart checks,
  including actual Go-binding activation failure cleanup and disabled remote
  fetch/XHR rejection. The Galaxy A57 passes all 19 first-run checks; its restart
  phase was interrupted by removal of the test task, confirmed by Android's exit
  reason. That restart rerun remains pending.
- Native libraries were rebuilt. Android's original AAR covered only two of the
  four architectures advertised by the app. The build script now produces all
  four, and the Expo plugin adds a gate rejecting missing ABI libraries.
- The normal iOS Debug simulator and unsigned Release device builds pass after
  the transport fixes. The ABI gate rejects the actual old two-architecture AAR
  and accepts the rebuilt four-architecture AAR in an isolated Gradle project.
- Normal Android Debug and Release builds pass with the ABI gate installed. Both
  APKs were inspected and contain all four embedded libraries. All 13 native
  TLS/proxy tests pass, with zero skips. The production phone app was not replaced.
- All 151 app tests and typechecking pass; lint has zero errors and the same 19
  existing warnings. No public infrastructure or production app was changed.

## Recovery and identity continuation (2026-09-20)

- Refreshed both repositories' origin/main and upstream/main; neither has commits
  missing from the current dependent feature branches.
- Reproduced silent identity replacement for missing/empty-map state and a helper
  panic for JSON `null`. The production node now validates and exclusively owns a
  private persistent store before networking starts. Malformed or partial profiles,
  missing/zero machine keys, changed coordinator, symlinks, public permissions,
  oversized files and trailing data fail closed. Writes are atomic and synchronized;
  write failure is terminal until repair/restart. Valid older keys are preserved.
- The actual production node registered with a local HTTPS Headscale and restarted
  with the same public identity and address (`/tmp/pond-identity-live.log`). This
  caught and fixed public-node-key reporting through the wrong local API.
- Signed approvals now bind the public machine identity available before node
  registration. A deliberately lost real registration response was recovered by
  inventory reconciliation without another registration write. Real two-household
  isolation, forced DERP region 999, restored-state client reconnection, replay
  rejection and revocation pass (`/tmp/pond-recovery-headscale-live.log`).
- Unit tests additionally reject wrong owner/key/address, unexpected tags/routes,
  duplicate machines, address reuse and revocation races. Preexisting unmanaged
  machines cannot be claimed, and definite registration rejection cannot become a
  later approval. HTTP tests distinguish rejected writes from ambiguous failures.
  Revoked machine tombstones
  remove registrations that arrive late. Legacy pending entries without this
  binding cannot recover automatically. Explicit replacement is covered by the
  subsequent coordinator continuation below; product recovery remains unfinished.
- Mobile activation requests stay on the selected LAN origin even when the shared
  REST client roams; they do not automatically retry or rotate tokens. All 154 app
  tests and typechecking pass; lint remains zero errors/19 existing warnings.
- The Galaxy A57 isolated app passed all 19 first-launch and three restart checks,
  including native failed-activation cleanup and blocked disabled remote traffic
  (`/tmp/gotg-identity-android-app.log`). This used direct LAN and synthetic USB
  certificate fixtures, not public cellular roaming or full mobile enrollment.
- Go race tests/vet and four Pond embedded-network tests pass. Android and iOS
  bindings have been rebuilt with the identity and machine-binding changes.
- The existing iPhone simulator passed all 19 initial and three restart checks
  with the rebuilt native library (`/tmp/gotg-recovery-ios-app.log`). The normal
  bundled helper build and production Pond compilation also pass. Physical iPhone
  and iOS 16.4 runtime acceptance remain pending.
- Normal Android Debug/Release and iOS Debug simulator/unsigned Release device
  builds pass. All 13 Android native tests passed with zero skips. Both APKs contain
  all four required embedded-library architectures. Logs are
  `/tmp/gotg-recovery-android-builds.log`, `/tmp/gotg-recovery-ios-debug-build.log`
  and `/tmp/gotg-recovery-ios-device-build.log`.
- Fresh/populated scratch-Pond lifecycle and authentication checks pass against
  the full production binary (`/tmp/pond-recovery-scratch-live.log`). Temporary
  Headscale/relay containers were stopped and their temporary administration
  credential removed. Production apps, Jetson service/data and signing keys were
  untouched during this continuation. No public infrastructure was deployed.

## Coordinator replacement continuation (2026-09-20)

- Refreshed both repositories' origin/main and upstream/main; neither has commits
  missing from the current feature branches. Unrelated local changes are preserved.
- Added signed inspection and explicit phone replacement to the enrollment service
  and private authority helper. Replacement compares the reviewed durable revision,
  binds a new machine and pending registration, consumes the signed approval once,
  and changes the revision before touching Headscale. Every newer revocation changes
  the revision as well. Ordinary enrollment cannot replace an existing identity.
- Retired machine records are persisted in the enrollment backup, never reused for
  enrollment, and reconciled for late registration cleanup without deleting another
  household's nodes. History is bounded at 256 records per household; exhaustion
  refuses replacement rather than dropping revocation history.
- The regression check failed before implementation with HTTP 400 for signed
  inspection (`/tmp/pond-replacement-before.log`). New tests cover concurrent/stale
  replacements, expired/wrong-household approvals, active/revoking records, machine
  reuse, newer revocation, lost responses, legacy inspection, malformed retirement
  state, restoration and late registration cleanup.
- The full Go race suite and vet pass (`/tmp/pond-replacement-all-race.log`,
  `/tmp/pond-replacement-vet.log`). The production bundled helper builds
  (`/tmp/pond-replacement-helper-build.log`).
- Real local Headscale verification passes with two households and forced Goose
  DERP region 999 (`/tmp/pond-replacement-headscale.log`). After backup restoration
  and revocation, an explicitly approved new machine reconnects to its own Pond;
  the old phone remains blocked, and a stale replacement approval is refused.
  The first fixture attempt exposed an immediate-poll timing error; the test now
  gives the newly authorized client a bounded startup interval before connecting.
- These are service/helper changes. No new Android/iOS app or Rust route changes
  were made in this continuation, and prior app/build results were not rerun.
  The phone-facing local review/approval/cancellation path still needs wiring and
  validation; this is not an end-to-end device recovery completion claim.

## Remaining acceptance work

- The Android API 37 emulator passes the expanded 39-assertion suite, including
  coordinator/relay outage, infrastructure restore, local replacement approval,
  and full app restart with authenticated REST/SSE. Both platforms pass after
  the restore fixture was changed to use fresh volume paths.
- Android development/release builds and 13 native tests pass. The AAR and both
  APKs pass 16 KB ELF checks; APK ZIP alignment and normal package identity pass.
  Each APK has 62 checked 64-bit libraries. Release packaging excludes test
  fixture configuration and includes dependency notices.
- The Jetson CUDA release link now passes with isolated Whisper GGML symbols.
  Combined GPU inference/transcription and clean shutdown pass with cached public
  embedding assets. Fresh/populated HTTPS checks pass. An uncached embedding
  download can outlive startup timeout and block runtime shutdown; this existing
  background-download issue remains separate. The validated production binary
  and helper are now installed with a private rollback backup.
- Out-of-band coordinator inventory drift has unit coverage; additional real
  coordinator mutation cases remain unmeasured.
- iOS 16.4 runtime acceptance and physical iPhone tests remain pending. No new iOS
  simulator has been created. The unavailable Android phone is replaced only for
  local test coverage by an API 37 ARM64 emulator with 16 KB pages.
- Public hosting/domain deployment remains a separate prerequisite. Real cellular
  roaming with the separate Tailscale app disconnected is not established by
  loopback/USB or simulator relay tests. Production phone installation waits for
  the Galaxy A57 to return.
- Strict server Clippy remains blocked by ten pond-voice warnings in files
  identical to origin/main; no lint rules were relaxed. Existing CI dependency
  access, Matter lockfile, security advisory and frontend resolution blockers
  remain documented above.

## Reproduction

Run Go checks from `native/pondnet` with Go 1.27.1:

```
go test -race ./...
go vet ./...
```

The live Headscale fixture must be a disposable loopback-only instance. Set
`POND_TEST_HEADSCALE=http://127.0.0.1:18090` and
`POND_TEST_HEADSCALE_CREDENTIAL_FILE` to its private administration-key file, then
run `go test -v ./enrollment -run TestHeadscaleLive -count=1`.
For forced DERP, also set `POND_TEST_FORCE_RELAY=1` and `POND_TEST_ROOT_CA` to the
isolated gateway's root certificate. Its HTTPS listener must match the fixture's
configured Headscale server URL. Never point this fixture at production Headscale.


## Earlier verification checkpoint

The final scratch-Pond fresh/populated live suite passes with transactional refresh
and device revocation enabled. Temporary Compose and relay containers were stopped;
no public infrastructure was deployed. The Jetson helper smoke test preserved the
active production server. All feature changes remain on `feature/embedded-tailnet`;
new milestone PRs are deferred until the remaining local embedded-proxy and recovery
acceptance work is complete. Unrelated IDE changes were preserved.

## Local approval flow and deployment preparation (2026-09-20)

The phone-facing recovery flow is now wired. After local re-pairing, a user can
explicitly request recovery from the remote-access screen. The Pond dashboard
shows the same short-lived request identifier. Local approval marks that request;
only the still-authenticated LAN phone can execute the exact machine/revision-bound
replacement. Polling, cancellation, expiry, revocation and profile-generation
changes cannot turn an old approval into a new registration. Ambiguous replacement
writes are not replayed automatically.

Measured checks for this continuation:

- Eight embedded-network Rust tests pass, including a real SQLite-issued token
  revoked after approval, remote execution denial and forged-forwarding-header
  rejection (`/tmp/pond-recovery-flow-rust.log`).
- GOTG's 157 tests in 18 suites and typecheck pass; lint has zero errors and 19
  existing warnings. Three added cases exercise approval polling, cancellation and
  ambiguous replacement without replay (`/tmp/gotg-recovery-flow-tests.log`).
- Dashboard: 833 tests in 58 files and production build pass
  (`/tmp/pond-recovery-dashboard-tests.log`).
- The Mac production Pond build and fresh/populated scratch live suite pass before
  the Linux-only Whisper dependency patch was introduced
  (`/tmp/pond-recovery-flow-production.log`, `/tmp/pond-recovery-flow-live.log`).
- Android development/release builds pass; iOS simulator Debug and unsigned device
  Release builds pass (`/tmp/gotg-recovery-flow-ios-debug.log`,
  `/tmp/gotg-recovery-flow-ios-device.log`). These builds precede the added native
  notice resources, which still require packaging verification.

The complete mobile-through-Headscale fixture is under active diagnosis. It uses
an isolated test package, a scratch Pond, pinned Headscale/Caddy images, and a
process-local fixture CA compiled through a temporary Go source overlay. Normal
native libraries are backed up and restored. No CA enters a system trust store.
A successful direct transport suite does not establish success through the
embedded node; the initial full Android activation attempts failed and are not
counted as passing acceptance checks.

The user has authorized updating the production Jetson service and Android app
once their replacement artifacts pass validation. Neither replacement has been
installed yet. The Jetson release build runs against a separate source snapshot;
its wrapper stops the old service while compiling and restores that same service
on success or failure. A dependency translated `target-cpu=cortex-a78` into an
invalid GCC `-march` value in the first attempt. The retry clears Rust CPU flags
and passes `-march=armv8.2-a+fp16+dotprod` explicitly to C/C++.

The vendored `whisper-rs-sys` patch isolates Whisper's static GGML symbols from
llama.cpp, including CUDA compilation. It remains unverified until the Jetson
link and combined inference/transcription runtime checks pass. Suppressing
duplicate-symbol errors is not an acceptable substitute.

## Complete mobile relay validation (2026-09-20)

The Galaxy A57 and iOS 27 simulator passed the full application path through an
isolated real Headscale and Goose DERP relay. Expo fetch and React Native XHR/SSE
reach the scratch Pond's private companion bridge. Tests verify the peer has no
direct address and uses the Goose relay, authenticated REST, remote token refresh,
LAN-only pairing including remote completion of a local challenge and forged
forwarding identity, wrong pins on both clients, plaintext refusal, same-key
certificate renewal, expired/wrong-host certificates, redirect rejection before
its destination, native stop/reconnect with unchanged identity, failed activation
cleanup, disabled-node refusal, and secure app-profile persistence.

Logs: `/tmp/gotg-headscale-android-tls-pass.log` and
`/tmp/gotg-headscale-ios-tls-pass.log`. These are synthetic scratch runs, not public
cellular acceptance. Certificate variants disable session tickets and evict pooled
connections so rejection checks exercise new handshakes. Fixture-only CA and TLS
variants use temporary Go overlays; normal libraries are restored in cleanup.
Apple gomobile replaces GOFLAGS, so the fixture builds the pinned tool and reapplies
its overlay at each Go invocation, then checks the CA exists in the test framework.

Android now supplies network interfaces through the native callback because its
sandbox forbids Go netlink enumeration. Connectivity callbacks notify the embedded
node without collecting SSIDs. Mobile diagnostics use the validated private profile
directory; upstream remote log upload remains disabled. A forced-relay test knob
that leaked a dummy socket on rebind was replaced by `TS_DEBUG_NEVER_DIRECT_UDP`;
relay-only routing is independently asserted after real traffic.

On iOS 17+, ATS repeats system-root validation for embedded CGNAT addresses even
after request pin verification. The Expo plugin now supplies exceptions limited to
`100.64.0.0/10` and `fd7a:115c:a1e0::/48`, as described in
[Apple's local networking documentation](https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowslocalnetworking).
PondURLProtocol intercepts every request to those ranges, including unmarked and
unconfigured requests; HTTPS and a configured pin are required before creating a
session. Validity and hostname checks remain mandatory. Unrelated traffic retains
platform validation. The native proxy suite asserts interception, no plaintext or
wrong-pin HTTP, and no proxy credential leakage (`/tmp/gotg-final-apple-proxy.log`).

Normal iOS simulator Debug and unsigned device Release builds pass with minimum
version 16.4 and the 132254-byte dependency notice file in each app bundle
(`/tmp/gotg-final-ios-debug.log`, `/tmp/gotg-final-ios-device.log`). The ARM64 helper
runs on Jetson and exposes the same notices via `--third-party-notices`. Go race
tests and vet pass after the native callback changes (`/tmp/pond-final-go-race.log`,
`/tmp/pond-final-go-vet.log`). Both repositories' fetched main branches are ancestors
of the current feature branches. All three production database migration sets
match the staged sources, with no checksum differences or unapplied migrations.
A private rollback backup is stored on Jetson at
`~/.local/state/pond-deployment-backups/20260920-105845`.

## Simulator and packaging completion (2026-09-20)

The iOS 27 simulator passes all 39 assertions after restoring Headscale, enrollment,
and Caddy state into fresh volume paths. This covers correct and incorrect pins,
certificate dates/hostname, renewal with unchanged SPKI, redirects, plaintext,
Expo fetch, RN XHR/SSE, LAN-only pairing through the private bridge, remote refresh,
forced Goose DERP, coordinator/relay outage recovery, revocation, locally approved
replacement, and full application process restart with retained network identity.
Log: `/tmp/gotg-final-ios-fresh-restore.log`. Reusing a Docker Desktop mount path
caused startup failures despite an intact SQLite database; restoring into new
paths and recreating containers follows the documented deployment procedure.

Android's API 37 emulator passes the same expanded 39 assertions in
`/tmp/gotg-final-android-restore.log`. Normal Debug and Release builds and all 13
native unit tests pass (`/tmp/gotg-final-normal-android.log`). AAR and APK checks
verify 16 KB ELF alignment; both APKs contain 62 checked 64-bit libraries. Package
identity, ZIP alignment, third-party notices and absence of release fixture data
were checked. The personal test release uses the existing Android debug signing
identity; this is not a Play distribution signing claim. Installation on the
production phone waits for the device to return.

The final app suite passes 157 tests in 18 suites; typecheck passes and lint has
zero errors with 19 existing warnings. Logs: `/tmp/gotg-shipping-tests.log`,
`/tmp/gotg-shipping-types.log`, `/tmp/gotg-shipping-lint.log`.

## Jetson production verification and installation (2026-09-20)

The ARM64 release build with llama.cpp and Whisper CUDA succeeds. The isolated
CUDA test completes transcription and local generation in the same process,
verifies actual GPU-offload/Whisper CUDA log evidence, restarts against populated
scratch data, and exits cleanly. Public model assets are copied or referenced;
production databases and signing keys are not test fixtures. Results are retained
on Jetson under `~/.cache/pond-https-test/cuda-smoke/`.

A separate cold-cache run exposed a pre-existing FastEmbed startup problem: its
30-second async timeout leaves a blocking download thread alive, and Tokio waits
for that thread on shutdown. The diagnostic trace shows a blocking socket receive;
cached public embedding assets remove that variable and the combined CUDA test
passes. No shutdown check was relaxed and no system ptrace policy was changed.

The validated binary and bundled ARM64 helper were installed atomically after a
fresh consistent backup at `~/.local/state/pond-deployment-backups/20260920-124010`.
The deployment checked the running executable digest and owning PID, loopback-only
HTTP on 8080, pinned HTTPS health on 4443, retained existing HTTPS private key if
present, and helper notices. The service is active. No database migration, service
unit rewrite, model change, or production source checkout reset was performed.
Remote coordination is not configured or publicly deployed by this installation.
Server SHA-256: `ad5e81c6cb116e84928e049e2f05e09dc4cd4611abc583b71ad20d4914ad334c`.
Helper SHA-256: `edc77ad03aebd085851ede48b863067f8d9a45fe767dfe1e7532e6ccf6f1e0bc`.

Both final mobile reruns pass 39 assertions with zero failures:
`/tmp/gotg-final-android-fresh-restore.log` and
`/tmp/gotg-final-ios-fresh-restore.log`. The normal native libraries are restored
after each isolated fixture run. Physical Android installation and cellular tests
wait for the phone and publicly reachable Goose infrastructure; physical iPhone
and minimum-iOS runtime checks remain pending.
