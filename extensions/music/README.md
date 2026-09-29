# GIAP Music Extension

MCP extension for music playback, for **Spotify** or **Apple Music**. One service is active at a
time, so the tool list, and the prompt it costs, is the same size whichever is in use.

## Installation

Install from the GIAP Extensions marketplace with one click.

- **Spotify:** click **Sign in with Spotify** to authorize playback control. GIAP handles the
  entire OAuth flow.
- **Apple Music (macOS):** works at once through the Music app; the first time, macOS asks whether
  Goose In A Pond may control Music. Add a MusicKit key to play the whole catalog in the app's own
  player. See [Apple Music](#apple-music).

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
| `play` | Play a song, playlist or link; resume with no arguments | `when: next` needs a queue: Spotify and the in-app player have one, the Music app does not |
| `playlists` | List the user's playlists | |
| `library` | Liked or favourite songs, most played, recently played | `time_range` is Spotify only; Apple reports lifetime play counts |
| `devices` | List playback devices, or move playback to one | Spotify Connect devices, or AirPlay speakers through the Music app; absent with the in-app player |
| `status` | What is playing now | The upcoming queue is Spotify only |
| `control` | Pause, resume, next, previous, volume, shuffle, seek, repeat | |

## Apple Music

Apple Music plays through **the app's own player** once you have added an Apple Music key, and
through the **Music app** otherwise, and whenever the player cannot be used. The extension tells
you which it used in the result.

| | In-app player | Music app |
|---|---|---|
| Plays | Any song in the Apple Music catalog, at once | Songs in your library; a catalog-only song is opened in Music and does **not** start |
| "Play next" | Yes | No queue to add to |
| Devices | The Mac's sound output (change it in Sound settings) | Music's AirPlay devices, through the `devices` tool |
| Needs | An Apple Developer MusicKit key, one sign-in click, and a Widevine-capable build of the app | macOS, and permission to control Music |

### Setting up the in-app player

1. Join the [Apple Developer Program](https://developer.apple.com/programs/), create a **Media ID**
   and a **MusicKit key** (Certificates, Identifiers & Profiles), and download the `.p8`: Apple
   allows that once.
2. Enter the **Team ID**, **Key ID** and the `.p8` text in the extension's settings. The key may be
   pasted with or without its line breaks. These three are host-only: the app keeps them and signs a
   short-lived developer token for the player, so **the key is never given to this extension**.
3. The player window opens once and asks you to **Connect Apple Music**. Sign in with the Apple ID
   that has the subscription. It then hides itself and keeps playing in the background; closing it
   only hides it.

How it fits together, the protocol and the security model are in
[`docs/architecture/music-player.md`](../../docs/architecture/music-player.md).

### Status

Audio through the in-app player has **not** been confirmed. A live run got as far as Apple's
license and was refused (`MEDIA_LICENSE`, code -42605), most likely because of a broken Widevine
module Google is currently serving; see the architecture doc. Until that clears, a refused license
is reported and the Music app plays instead.

### Music app notes

- The scripts read Music's own dictionary (`sdef /System/Applications/Music.app`); favourites use the
  raw code `pLov`, since the property was `loved` before it was `favorited` and only the code stayed.
- A song played from the library carries on through the whole `Music` playlist, in library order,
  not through its album.
- Opening a catalog song's page in Music does not start it (checked on macOS 27).
- macOS 26 and later scope Music's commands (`com.apple.Music.playback`, `.library.read`,
  `.library.read-write`), so a permission prompt may name more than one.
- `MUSIC_SERVICE=apple` on any other platform falls back to Spotify.

### Network policy

The in-app player's every request is judged by `network_mode` in the app; the extension's own
public-search calls ask the app first too (`POST /api/v1/extension/egress`). A host that cannot be
reached does not block the extension, so a standalone `npm start` still works. Spotify's calls are
not routed through this yet.

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
   - `user-library-read`, `user-top-read`, `user-read-recently-played`
   - `streaming`, `user-read-email`, `user-read-private` (only for the in-app player; a
     Premium account is needed for it)
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

`GIAP_MUSIC_LIVE=1 npm test` also runs read-only checks against the real Music app;
`GIAP_MUSIC_LIVE=play` additionally plays a library track quietly for a few seconds. The fakes
passed while a position read was failing silently, so these are the ones that vouch for the scripts.

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
