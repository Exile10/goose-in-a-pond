# The music player

A small player window the desktop shell keeps alive, and the path the `music` extension uses to
drive it. It exists because the only route to a whole streaming catalog on a Mac is the service's
own web SDK, and those SDKs need DRM. Nothing in this design names a service outside its adapter.

A service plugs in one of two ways, decided by where its control plane is:

| Style | Service | The window is | The extension |
|---|---|---|---|
| **Controller** | Apple Music (MusicKit is JavaScript-only) | Everything: search, queue, library, playback | `WebPlayerProvider`, over the bridge |
| **Speaker** | Spotify (a REST Web API drives playback) | One Connect device, named "Goose In A Pond" | The existing `SpotifyProvider`, which plays *to* that device when no other is active |

Status, 2026-09-29: the parts below are built and tested with fakes and over real HTTP. **Audio
has not played through it, for either service**: see "What was measured, and what is blocked".

## Parts

| Part | Where | Job |
|---|---|---|
| Player window | `pond-desktop/electron/main/player.ts` | Hidden `BrowserWindow` in its own session partition (`persist:giap-player`). Waits for the Widevine module, serves `app://giap` into its partition, and filters every request through the network policy. Hides on close, so music outlives the main window. |
| Network policy filter | `electron/main/playerPolicy.ts` | Asks `POST /api/v1/player/egress-policy` once per host per minute; refuses a host it cannot ask about. |
| Player page | `pond-desktop/src/player/` | `PlayerAdapter` (the service-agnostic interface, `types.ts`), one adapter per service (`adapters/`), the bridge (`bridge.ts`), a small UI (`PlayerApp.tsx`). Built as its own page, `player.html`. |
| Host bridge | `crates/pond-api/src/player.rs` | Holds who is attached and what is in flight; relays a command to the page and waits for its reply. |
| Developer token | `crates/pond-api/src/musickit.rs` | Signs Apple developer tokens (ES256) from the stored key, so the key stays out of every page and extension. |
| User token | `GET /api/v1/player/user-token` (`player.rs`) | Hands the paired page the person's Spotify access token, and renews it through the egress gate when the SDK says the last one went stale. |
| Spotify speaker | `pond-desktop/src/player/adapters/spotifyWebPlayback.ts` | Registers the window as a Spotify Connect device, reports what is playing, and does transport. Search, queue and library stay in the extension. |
| Extension | `extensions/music/src/providers/web-player.ts` | `WebPlayerProvider`: the extension's `MusicProvider` over the bridge, with the Music app as its fallback. |
| Spotify in the extension | `providers/spotify.ts`, `providers/player/speaker.ts` | Every call to Spotify asks the host first (`network_mode`); with no active device, playback moves to the in-app device. The tool list and its wording are unchanged. |

## The protocol

Extension to host, `POST /api/v1/player/command` (internal token, loopback only):
`{service, op, args?, timeout_ms?}`. The host answers `{ok:true, result}` or
`{ok:false, code, error}` with status 200; status codes are for a malformed or unauthorised request.

| Code | Meaning | The provider |
|---|---|---|
| `no_player`, `player_gone`, `player_replaced` | No page is attached, or it closed or reopened mid-call | Falls back to the Music app |
| `refused` | `network_mode` refused a call that reaches the service | Reports it; a fallback would not be allowed either |
| `timeout` | The page took the command and did not answer | Reports it; the song may have started |
| `needs_authorization`, `not_ready`, `drm_refused` | The page answered no | Falls back, and says why |
| `unsupported`, `bad_request`, `player_error` | The page answered no | Reports it |

Host to page, `GET /api/v1/player/events?service=` (server-sent events, the page's session token):
an `event: ready`, then `event: command` with `{id, service, op, args}`. Page to host:
`POST /player/reply` `{id, ok, result|error, code}` and `POST /player/state` `{service, state}`.

Ops: `state`, `search`, `play`, `enqueue`, `pause`, `resume`, `next`, `previous`, `seek`,
`volume`, `shuffle`, `repeat`, `playlists`, `library`, `device`. `authorize` is refused on purpose:
signing in needs a click in the window. `device` is for a speaker-style service: it answers
`{device_id, name, ready}`, and a controller-style one answers `unsupported`. The window runs one
adapter per service it knows, each attached to the host under its own name; one that is not set up
sits dormant and is asked again every ten seconds.

Transport ops (`pause`, `next`, `volume`...) are never judged by `network_mode`. A policy that cannot
stop the music that is already playing is a bug. Ops that reach the service (`search`, `play`,
`enqueue`, `playlists`, `library`) are.

## Security model

- **Spotify's user token goes to the page.** The SDK signs in with the person's own access token,
  so `GET /player/user-token` gives it to the paired page, and Spotify's SDK script (loaded from
  `sdk.scdn.co`) runs in that window with it. That is new: Apple's page only ever holds a token that
  is public by design. The route is session-only (an extension's internal token is refused, tested),
  answers 400 with where to sign in when nothing is stored, and renews only through the egress gate.
  It is the same token the extension already holds in its environment; what changed is that a
  third-party script now sees it too.
- **The sign-in needs new scopes.** The SDK will not start without `streaming`, `user-read-email`
  and `user-read-private`. A token issued before they were asked for makes the player say "sign in
  to Spotify again" instead of failing silently; everyone signs in once more.

- **Managed credentials.** A household with no key of its own can use `pondcredentials`, a service
  that holds Jarida's key (`docs/architecture/pondcredentials.md`). A stored local key always wins.
- **The signing key never leaves the host.** `APPLE_MUSIC_TEAM_ID`, `_KEY_ID` and `_PRIVATE_KEY` are
  `host_only` secrets (`SecretRequirement.host_only`): stored and reported as saved, withheld from
  every extension's environment on install, restart, token refresh and startup. Proven by tests on
  each path. The player page asks `GET /api/v1/musickit/developer-token` with its own session.
- **Extensions cannot mint tokens.** That route is an ordinary authenticated one; the internal token
  an extension holds is refused there (tested).
- **The player pairs as its own device**, from its own storage partition, so it never revokes the
  app's session (pairing revokes earlier sessions for the same client id).
- **Every request the player window makes is judged**, because Chromium makes them and the Rust gate
  never sees them. The policy sees the origin only, never a path or query.
- Developer tokens live seven days (Apple's ceiling is about six months): one that lapses mid-song
  ends the music.

## What was measured, and what is blocked

Measured on macOS 27, arm64, in scratch folders:

- Stock Electron 44.4.2 (Chrome 152) has EME but **no key systems**: Widevine, FairPlay and
  PlayReady are all unsupported. So the shell needs castlabs' Electron (`+wvcus`) for any DRM.
- castlabs `v44.1.0+wvcus` loads Widevine 4.10.3050.0 (about 6 s on first launch), and the module
  answers. Its Chromium (152.0.7977.65) is older than stock's, and castlabs' newest stable for 44 is
  `44.1.0` against this repo's `44.4.2`.
- **A live run with a real MusicKit key got as far as the license**: token signing, MusicKit load,
  sign-in, catalog search and queueing all worked, then Apple refused the license (MusicKit
  `MEDIA_LICENSE`, code -42605). The same failure on the same module version is reported by another
  Apple Music client on this stack (castlabs/electron-releases#237, Cider-2#2015), where 4.10.3050.1
  and 4.10.3112.0 work. **That is the likely cause, not a proven one**: no way was found to load
  3112.0 into castlabs' build (`--widevine-cdm-path` is ignored, and a hand-copied module directory
  is ignored), so the other suspect, castlabs' production VMP signing, is not ruled out.
- MusicKit reports "playing" for a moment **before** a license failure arrives, so the adapter
  confirms only when the position has advanced.
- **Spotify has not been run at all.** The adapter is built from the SDK's documented events and
  tested against a fake made from them. Nobody has yet signed in to Spotify Premium in this window,
  so it is not known whether Spotify accepts this Widevine module (Apple did not), whether the SDK
  starts in castlabs' Electron, or whether a device appears in Spotify's list. A run that plays
  Spotify audio would also settle whether the Apple refusal is Apple's own or a fault in this
  DRM stack, so it is the single most useful experiment left.

## The Electron build

The shell now depends on castlabs' Electron (`git+https://github.com/castlabs/electron-releases.git#v44.1.0+wvcus`),
because stock Electron has no Widevine. Three things came with that, and only the first is tested:

- **Install.** castlabs' package has no `postinstall`, so `package.json` adds one
  (`scripts/install-electron.mjs`) that fetches the binary. castlabs' own installer ignores
  `ELECTRON_SKIP_BINARY_DOWNLOAD`, which stock Electron honours and CI's frontend job sets, so the
  script puts that flag back (tried both ways). A public GitHub dependency installs over https even
  with SSH unavailable (tried, in a scratch directory with SSH broken); the lockfile still records a
  `git+ssh` URL, which npm writes regardless of the specifier.
- **Packaging.** electron-builder looks for a `+wvcus` version on Electron's own releases and will
  not find it, so `electron-builder.yml` sets `electronDist: node_modules/electron/dist`. **`npm run
  pack:dir` and `bundle:app` have not been run with it.**
- **Signing.** castlabs' Widevine needs a VMP signature from their EVS service for a production
  build. That has not been set up, and it is the second suspect for Apple's refusal.

It also trails stock (44.1.0, Chrome 152.0.7977.65, against 44.4.2), so it misses whatever
Electron shipped after 44.1.0. The CI smoke job launches Electron on Linux; whether castlabs'
build runs there is untried. To go back, restore `"electron": "^44.4.2"` and the lockfile, and drop
the `postinstall`, `scripts/install-electron.mjs` and the two `electron-builder.yml` keys.

## Spotify's terms

Read from the Spotify Developer Terms, not settled:

| Clause | Says | Effect |
|---|---|---|
| III.1.1 | The licence is for "private personal use" and "on Approved Devices". | A household assistant is arguably personal use; shipping one as a product is not clearly covered. |
| IV.2.1 | Do not use Spotify Content "to train a machine learning or AI model or otherwise ingest Spotify Content into a machine learning or AI model". Spotify Content includes metadata and playlists. | The assistant shows track and playlist names to the model, and memory extraction can keep them. This applies to the Spotify tools that already existed, not only this player. |
| VI.1.2 | The client id must be embedded "in a secure manner not accessible by third parties". | The bundled client id sits in an open-source repository. |
| "Streaming" | Includes controlling a background Spotify application. | The Connect-based tool was already a Streaming application. |

The Web Playback SDK page also says it must not be used in commercial projects without Spotify's
prior written approval (read from a summary of that page, not its text). Building this is
reversible; shipping it is Jarida's decision, and none of these has been put to Spotify.

## Known holes

- **The Widevine module download and its updates are made by Chromium's component updater**, not by
  the window, so neither the request filter nor `network_mode` covers them. Under Offline the module
  may still be fetched from Google. A switch that keeps the updater off until the network setting
  allows it is not built.
- **No permission handler** is installed on the player's session; Electron's defaults apply. Denying
  the unneeded ones (camera, microphone) is right, but a wrong deny on the protected-media
  permission would look exactly like a license failure, so it waits until audio plays.
- The extension's own outbound calls ask the host first (`/extension/egress`), Spotify's included.
  What the window itself sends (the SDK's script, its sockets, the audio) is judged host by host by
  the request filter. Spotify's hosts are classed as sensitive like Apple's, so under
  `network_mode = allowlist` or `offline` the in-app player is refused, and its message says to
  check the network setting.
- **Spotify's device is only used when nothing else is active.** A phone or another computer that is
  playing is never taken over, so the household's other devices keep working; the cost is that
  "play X" with a device already active plays there, not in the window.

## Adding a service (Tidal...)

First decide the style: does the service's own SDK, in JavaScript, do everything (a controller, as
MusicKit), or does a REST API drive playback that an existing provider already speaks (a speaker, as
Spotify)?

1. `pond-desktop/src/player/adapters/<service>.ts`: implement `PlayerAdapter`. Only this file may
   know the SDK. Throw `PlayerError` with a stable code; a controller's `play` must resolve only when
   audio is really playing. A speaker implements `device()` and transport, and answers `unsupported`
   for what its REST provider does.
2. Register it in `adapters/index.ts`. If the SDK signs in with the person's token, add the service
   to `user_token_handler` in `player.rs`; if with a developer key, follow `musickit.rs`.
3. Host: add the service's API host to `service_host` in `player.rs` so `network_mode` can judge
   its network ops.
4. Extension: a controller constructs `new WebPlayerProvider({host, service, label, local,
   linkToId})` in `providers/index.ts` and widens `ServiceId` in `types.ts`. A speaker passes a
   `HostSpeaker` and an `EgressGate` to its existing provider, as `createSpotify` does.

## Verification

`cargo test -p pond-api --test player_routes` drives real SSE frames through the real router.
`npm test` in `pond-desktop` covers the Apple adapter against a fake MusicKit (built from the sequence
the live run showed), the Spotify adapter against a fake SDK (built from its documented events),
the bridge, the SSE reader, the UI, the multi-service page and the policy filter. `npm test` in
`extensions/music` covers the providers over a fake host and a fake network: every Spotify call
asking the host first (a refusal sends nothing, the retry after a refresh asks again), and the
fallback to the in-app device happening only for Spotify's "no active device" and nothing else.
`GIAP_MUSIC_LIVE=1` adds checks against the real Music app for the fallback.

Not covered by anything runnable yet: audio through the player for either service (blocked above),
the player window under real Electron (it was watched attach and answer a command by hand, once,
for Apple; there is no repeatable test), the Spotify adapter against Spotify, and `enqueue`,
`playlists` and `library` against Apple's real replies.
