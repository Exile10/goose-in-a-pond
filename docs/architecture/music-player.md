# The music player

A small player window the desktop shell keeps alive, and the path the `music` extension uses to
drive it. It exists because the only route to a whole streaming catalog on a Mac is the service's
own web SDK, and those SDKs need DRM. Nothing in this design names a service outside its adapter.

Status, 2026-09-29: the parts below are built and tested with fakes and over real HTTP. **Audio
has not played through it**: see "What was measured, and what is blocked".

## Parts

| Part | Where | Job |
|---|---|---|
| Player window | `pond-desktop/electron/main/player.ts` | Hidden `BrowserWindow` in its own session partition (`persist:giap-player`). Waits for the Widevine module, serves `app://giap` into its partition, and filters every request through the network policy. Hides on close, so music outlives the main window. |
| Network policy filter | `electron/main/playerPolicy.ts` | Asks `POST /api/v1/player/egress-policy` once per host per minute; refuses a host it cannot ask about. |
| Player page | `pond-desktop/src/player/` | `PlayerAdapter` (the service-agnostic interface, `types.ts`), one adapter per service (`adapters/`), the bridge (`bridge.ts`), a small UI (`PlayerApp.tsx`). Built as its own page, `player.html`. |
| Host bridge | `crates/pond-api/src/player.rs` | Holds who is attached and what is in flight; relays a command to the page and waits for its reply. |
| Developer token | `crates/pond-api/src/musickit.rs` | Signs Apple developer tokens (ES256) from the stored key, so the key stays out of every page and extension. |
| Extension | `extensions/music/src/providers/web-player.ts` | `WebPlayerProvider`: the extension's `MusicProvider` over the bridge, with the Music app as its fallback. |

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
`volume`, `shuffle`, `repeat`, `playlists`, `library`. `authorize` is refused on purpose: signing in
needs a click in the window.

Transport ops (`pause`, `next`, `volume`...) are never judged by `network_mode`. A policy that cannot
stop the music that is already playing is a bug. Ops that reach the service (`search`, `play`,
`enqueue`, `playlists`, `library`) are.

## Security model

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

## Known holes

- **The Widevine module download and its updates are made by Chromium's component updater**, not by
  the window, so neither the request filter nor `network_mode` covers them. Under Offline the module
  may still be fetched from Google. A switch that keeps the updater off until the network setting
  allows it is not built.
- **No permission handler** is installed on the player's session; Electron's defaults apply. Denying
  the unneeded ones (camera, microphone) is right, but a wrong deny on the protected-media
  permission would look exactly like a license failure, so it waits until audio plays.
- The extension's own outbound calls ask the host first (`/extension/egress`); Spotify's do not yet.

## Adding a service (Tidal, Spotify's web player...)

1. `pond-desktop/src/player/adapters/<service>.ts`: implement `PlayerAdapter`. Only this file may
   know the SDK. Throw `PlayerError` with a stable code; `play` must resolve only when audio is
   really playing.
2. Register it in `adapters/index.ts`.
3. Host: add the service's API host to `service_host` in `player.rs` so `network_mode` can judge
   its network ops.
4. Extension: construct `new WebPlayerProvider({host, service, label, local, linkToId})` in
   `providers/index.ts`. Widen `ServiceId` in `types.ts`.

## Verification

`cargo test -p pond-api --test player_routes` drives real SSE frames through the real router.
`npm test` in `pond-desktop` covers the adapter against a fake MusicKit (built from the sequence the
live run showed), the bridge, the SSE reader, the UI and the policy filter. `npm test` in
`extensions/music` covers the provider over a fake host. `GIAP_MUSIC_LIVE=1` adds checks against the
real Music app for the fallback.

Not covered by anything runnable yet: audio through the player (blocked above), the player window
under real Electron, and `enqueue`, `playlists` and `library` against Apple's real replies.
