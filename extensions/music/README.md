# GIAP Music Extension

MCP extension for music playback and playlist management via the Spotify Web API.

## Installation

Install from the GIAP Extensions marketplace with one click. Click **Sign in with Spotify** to authorize playback control -- GIAP handles the entire OAuth flow.

## Available Tools

| Tool | Description |
|------|-------------|
| `now_playing` | Get info about the currently playing track |
| `play` | Start or resume playback, optionally by Spotify URI |
| `pause` | Pause the current playback |
| `next` | Skip to the next track |
| `previous` | Go back to the previous track |
| `search_tracks` | Search for tracks by name, artist, or keyword |
| `search_albums` | Search for albums by name, artist, or keyword |
| `get_playlists` | List saved playlists |
| `get_playlist_tracks` | Get all tracks in a playlist |
| `create_playlist` | Create a new playlist |
| `add_to_playlist` | Add tracks to an existing playlist |
| `get_queue` | View the current playback queue |
| `set_volume` | Set playback volume (0-100) |
| `set_shuffle` | Enable or disable shuffle mode |

## Manual Setup (Development)

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

## Testing

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
- Spotify Premium account (required for playback control)
- An active Spotify device (phone, desktop app, or web player)
