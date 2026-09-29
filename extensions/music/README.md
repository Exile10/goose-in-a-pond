# GIAP Music Extension

MCP extension for music playback, for **Spotify** or **Apple Music**. One service is active at a
time, so the tool list, and the prompt it costs, is the same size whichever is in use.

## Installation

Install from the GIAP Extensions marketplace with one click.

- **Spotify:** click **Sign in with Spotify** to authorize playback control. GIAP handles the
  entire OAuth flow.
- **Apple Music (macOS):** nothing to sign in to. The extension drives the Music app; the first
  time, macOS asks whether Goose In A Pond may control Music. See [Apple Music](#apple-music).

Every credential is optional at install time, so an Apple Music user is never asked to sign in to
Spotify.

## Choosing the service

`MUSIC_SERVICE` decides; leave it blank for auto.

| `MUSIC_SERVICE` | Result |
|---|---|
| `spotify` | Spotify |
| `apple` (also `apple-music`) | Apple Music, on macOS only; anywhere else it falls back to Spotify |
| blank or `auto` | Spotify if the user has signed in to it, otherwise Apple Music on macOS |

The choice and the reason are logged as `service_chosen`. In the app, set `MUSIC_SERVICE` in the
extension's settings.

## Playback behaviour

`play` starts music **and keeps it going**. A song is played *inside its album*
— `PUT /me/player/play` with `context_uri` set to the album and `offset` set to
the track — so the requested song starts and the album follows on. When the
release is a single or a two-track EP, more by the same artist is queued behind
it, because an album context is no help when the album is one song long.

This matters because Spotify's play endpoint takes either `uris` or
`context_uri`, never both, and a `uris` list is an ad-hoc queue that Spotify
plays and then **stops**. Passing a single track URI as `uris: [track]` is
therefore a playlist of exactly one song, which is why playback used to fall
silent with nothing left in the queue.

Two constraints worth knowing before changing any of this:

- `offset` is only valid when the context is an **album or playlist**. Spotify
  rejects it for an artist context, so "play this song within the artist"
  cannot be expressed.
- The obvious source for follow-ups, `GET /artists/{id}/top-tracks`, answers
  **403** for this app — see the withdrawn-endpoint list in
  `src/providers/spotify.ts`. Follow-ups come from a field-filtered `/search`
  instead, narrowed to the seed track's artist id.

`queue` is unchanged and still appends without interrupting.

## Tools

`TOOLS` is built per service by `buildTools` in `src/tools.ts`, which is the only authority; a
capability a service lacks is left out rather than advertised and refused.

| Tool | What it does | Notes |
|---|---|---|
| `play` | Play a song, playlist or link; resume with no arguments | `when: next` (queue) is Spotify only |
| `playlists` | List the user's playlists | |
| `library` | Liked or favourite songs, most played, recently played | `time_range` is Spotify only; Apple reports lifetime play counts |
| `devices` | List playback devices, or move playback to one | Spotify Connect devices, or AirPlay speakers on Apple Music |
| `status` | What is playing now | The upcoming queue is Spotify only |
| `control` | Pause, resume, next, previous, volume, shuffle, seek, repeat | |

## Apple Music

Apple Music has no single API that does everything, so the extension combines three:

| Piece | Used for | Needs |
|---|---|---|
| The **Music app**, scripted with AppleScript (`src/providers/apple/music-app.ts`) | Playing, pausing, volume, shuffle, repeat, seek, now playing, the library, playlists, AirPlay | macOS; permission to control Music |
| The **iTunes Search API** | Finding a song that is not in the library | Nothing: public and keyless (about 20 requests a minute) |
| The **Apple Music API** (`src/providers/apple/rest.ts`) | Searching the catalog in the user's store, and adding a song to their library | An Apple Developer key and a Music User Token, both optional |

The Music app cannot search Apple's catalog or play a song that is not in the library, and
Apple's API cannot play anything. So `play` works like this:

1. A song **in the library** starts at once.
2. A song **only in the catalog** is added to the library through the Apple Music API when it is
   set up, waited for, and played. The result says it was added.
3. Without the API it is opened in the Music app and the result says it has **not** started
   playing. Apple offers no way to start a catalog song from a script without the library step.

There is no queue: the Music app has nothing to append to, so `when: next` is refused rather than
replacing what is playing.

### Setting up the Apple Music API (optional)

Only needed for step 2. Add these in the extension's settings. They live in GIAP's secret store;
the Team ID, Key ID and key are host-only and are not passed to the extension, which receives just
the Music User Token.

1. Join the [Apple Developer Program](https://developer.apple.com/programs/) and create a
   **MusicKit key** under Certificates, Identifiers & Profiles. Download the `.p8` once.
2. Enter the **Team ID**, the **Key ID** and the `.p8` text. The key may be pasted with or
   without its line breaks.
3. Click **Sign in with Apple Music**. A page served by the local GIAP server opens; approve the
   prompt from Apple.

GIAP signs the short-lived developer token itself (`POST /api/v1/musickit/developer-token`), so the
private key stays in the host. The Music User Token is issued by Apple's MusicKit JS only, has no
refresh, and lasts about six months; sign in again when Apple starts refusing it.

### Network policy

Every outbound Apple call asks the host first (`POST /api/v1/extension/egress`), so
`network_mode` applies and the call appears in Logs as `via giap-music`. A host that cannot be
reached does not block the extension, so a standalone `npm start` still works. Spotify calls are
not routed through this yet.

### Requirements and limits

- macOS with the Music app, and an Apple Music subscription for streaming.
- `MUSIC_SERVICE=apple` on any other platform falls back to Spotify.
- Scripts use the raw code `pLov` for favourites: the property was `loved` before it was
  `favorited`, and only the code stayed the same.
- Commands are scoped in macOS 26 and later (`com.apple.Music.playback`, `.library.read`,
  `.library.read-write`), so a permission prompt may name more than one.

## Manual Setup (Development)

### Spotify

For local development and testing without the GIAP OAuth flow:

1. Create a Spotify Developer app at https://developer.spotify.com/dashboard
2. Generate an access token with the required scopes:
   - `user-read-playback-state`
   - `user-modify-playback-state`
   - `user-read-currently-playing`
   - `playlist-read-private`
   - `playlist-modify-private`
   - `playlist-modify-public`
3. Set the token as an environment variable:

```bash
export SPOTIFY_ACCESS_TOKEN="your-token-here"
```

4. Run the server:

```bash
cd extensions/music
npm install
npm start
```

### Apple Music

Nothing to configure: `MUSIC_SERVICE=apple npm start`. The Music app must answer, so if a call
hangs, look for a macOS permission dialog or a sign-in window in Music. Calls give up after 20
seconds with that advice rather than waiting.

## Testing

```bash
npm test            # unit tests: no network, no Music app, no Spotify
npm run typecheck
```

The script tests include a syntax compile of every AppleScript (macOS only). It cannot check that
a term exists in Music's dictionary, since an unknown name compiles as a variable; read the
dictionary with `sdef /System/Applications/Music.app` when adding one.

Test the MCP protocol handshake:

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | npx tsx src/server.ts
```

List available tools:

```bash
echo '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' | npx tsx src/server.ts
```

## Requirements

- Node.js 18+ (for native fetch)
- Spotify: a Premium account (required for playback control) and an active device (phone,
  desktop app, or web player)
- Apple Music: macOS, and see [Apple Music](#apple-music)
