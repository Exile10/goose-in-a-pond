#!/usr/bin/env node
import * as readline from 'readline';
import { SpotifyProvider } from './providers/spotify.js';

const provider = new SpotifyProvider();

const SERVER_INFO = {
  name: 'giap-music',
  version: '0.1.0',
};

interface ToolDefinition {
  name: string;
  description: string;
  inputSchema: {
    type: string;
    properties: Record<string, unknown>;
    required?: string[];
  };
}

const TOOLS: ToolDefinition[] = [
  {
    name: 'now_playing',
    description:
      'Get info about the currently playing track including title, artist, album, and playback progress.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'play',
    description:
      'Start or resume playback. Optionally play a specific track, album, or playlist by Spotify URI.',
    inputSchema: {
      type: 'object',
      properties: {
        uri: {
          type: 'string',
          description:
            'Spotify URI (e.g. spotify:track:..., spotify:album:..., spotify:playlist:...). Omit to resume current playback.',
        },
      },
    },
  },
  {
    name: 'pause',
    description: 'Pause the current playback.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'next',
    description: 'Skip to the next track in the queue.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'previous',
    description: 'Go back to the previous track.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'search_tracks',
    description:
      'Search for tracks on Spotify by name, artist, or any keyword. Returns matching tracks with URIs.',
    inputSchema: {
      type: 'object',
      properties: {
        query: {
          type: 'string',
          description: 'Search query (e.g. "Bohemian Rhapsody", "Drake", "chill vibes").',
        },
        limit: {
          type: 'number',
          description: 'Maximum number of results (1-50, default 10).',
        },
      },
      required: ['query'],
    },
  },
  {
    name: 'search_albums',
    description:
      'Search for albums on Spotify by name, artist, or keyword. Returns matching albums with URIs.',
    inputSchema: {
      type: 'object',
      properties: {
        query: {
          type: 'string',
          description: 'Search query (e.g. "Abbey Road", "Kendrick Lamar").',
        },
        limit: {
          type: 'number',
          description: 'Maximum number of results (1-50, default 10).',
        },
      },
      required: ['query'],
    },
  },
  {
    name: 'get_playlists',
    description: "Get the user's saved playlists from Spotify.",
    inputSchema: {
      type: 'object',
      properties: {
        limit: {
          type: 'number',
          description: 'Maximum number of playlists to return (1-50, default 20).',
        },
      },
    },
  },
  {
    name: 'get_playlist_tracks',
    description: 'Get all tracks in a specific playlist.',
    inputSchema: {
      type: 'object',
      properties: {
        playlist_id: {
          type: 'string',
          description: 'The Spotify playlist ID.',
        },
      },
      required: ['playlist_id'],
    },
  },
  {
    name: 'create_playlist',
    description: "Create a new playlist in the user's Spotify library.",
    inputSchema: {
      type: 'object',
      properties: {
        name: {
          type: 'string',
          description: 'Name for the new playlist.',
        },
        description: {
          type: 'string',
          description: 'Optional description for the playlist.',
        },
      },
      required: ['name'],
    },
  },
  {
    name: 'add_to_playlist',
    description: 'Add one or more tracks to an existing playlist.',
    inputSchema: {
      type: 'object',
      properties: {
        playlist_id: {
          type: 'string',
          description: 'The Spotify playlist ID to add tracks to.',
        },
        track_uris: {
          type: 'array',
          items: { type: 'string' },
          description:
            'Array of Spotify track URIs (e.g. ["spotify:track:4iV5W9uYEdYUVa79Axb7Rh"]).',
        },
      },
      required: ['playlist_id', 'track_uris'],
    },
  },
  {
    name: 'get_queue',
    description:
      'Get the current playback queue including the currently playing track and upcoming tracks.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'set_volume',
    description: 'Set the playback volume (0-100).',
    inputSchema: {
      type: 'object',
      properties: {
        percent: {
          type: 'number',
          description: 'Volume level from 0 (mute) to 100 (max).',
        },
      },
      required: ['percent'],
    },
  },
  {
    name: 'set_shuffle',
    description: 'Enable or disable shuffle mode.',
    inputSchema: {
      type: 'object',
      properties: {
        enabled: {
          type: 'boolean',
          description: 'True to enable shuffle, false to disable.',
        },
      },
      required: ['enabled'],
    },
  },
];

function formatTrack(track: {
  name: string;
  artist: string;
  album: string;
  duration_ms: number;
  uri: string;
  is_playing?: boolean;
  progress_ms?: number;
}): string {
  const mins = Math.floor(track.duration_ms / 60000);
  const secs = Math.floor((track.duration_ms % 60000) / 1000)
    .toString()
    .padStart(2, '0');
  let line = `${track.name} - ${track.artist} (${track.album}) [${mins}:${secs}]`;

  if (track.progress_ms != null) {
    const pMins = Math.floor(track.progress_ms / 60000);
    const pSecs = Math.floor((track.progress_ms % 60000) / 1000)
      .toString()
      .padStart(2, '0');
    line += ` ${pMins}:${pSecs}/${mins}:${secs}`;
    if (track.is_playing) {
      line += ' (playing)';
    } else {
      line += ' (paused)';
    }
  }

  line += `\n  URI: ${track.uri}`;
  return line;
}

interface JsonRpcRequest {
  jsonrpc: string;
  id?: number | string;
  method: string;
  params?: Record<string, unknown>;
}

interface CallToolResult {
  content: Array<{ type: string; text: string }>;
  isError?: boolean;
}

function textResult(text: string): CallToolResult {
  return { content: [{ type: 'text', text }] };
}

function errorResult(message: string): CallToolResult {
  return { content: [{ type: 'text', text: `Error: ${message}` }], isError: true };
}

async function callTool(
  name: string,
  args: Record<string, unknown>
): Promise<CallToolResult> {
  try {
    switch (name) {
      case 'now_playing': {
        const track = await provider.getNowPlaying();
        if (!track) return textResult('Nothing is currently playing.');
        return textResult(formatTrack(track));
      }

      case 'play': {
        const uri = args.uri as string | undefined;
        const result = await provider.play(uri);
        return textResult(result);
      }

      case 'pause': {
        const result = await provider.pause();
        return textResult(result);
      }

      case 'next': {
        const result = await provider.next();
        return textResult(result);
      }

      case 'previous': {
        const result = await provider.previous();
        return textResult(result);
      }

      case 'search_tracks': {
        const query = args.query as string;
        const limit = args.limit as number | undefined;
        if (!query) return errorResult('query is required');
        const tracks = await provider.searchTracks(query, limit);
        if (tracks.length === 0) return textResult(`No tracks found for "${query}".`);
        const lines = tracks.map((t, i) => `${i + 1}. ${formatTrack(t)}`);
        return textResult(lines.join('\n'));
      }

      case 'search_albums': {
        const query = args.query as string;
        const limit = args.limit as number | undefined;
        if (!query) return errorResult('query is required');
        const albums = await provider.searchAlbums(query, limit);
        if (albums.length === 0) return textResult(`No albums found for "${query}".`);
        const lines = albums.map(
          (a, i) =>
            `${i + 1}. ${a.name} - ${a.artist} (${a.release_date}, ${a.total_tracks} tracks)\n  URI: ${a.uri}`
        );
        return textResult(lines.join('\n'));
      }

      case 'get_playlists': {
        const limit = args.limit as number | undefined;
        const playlists = await provider.getPlaylists(limit);
        if (playlists.length === 0) return textResult('No playlists found.');
        const lines = playlists.map(
          (p, i) =>
            `${i + 1}. ${p.name} (${p.track_count} tracks)${p.description ? ' - ' + p.description : ''}\n  ID: ${p.id} | URI: ${p.uri}`
        );
        return textResult(lines.join('\n'));
      }

      case 'get_playlist_tracks': {
        const playlistId = args.playlist_id as string;
        if (!playlistId) return errorResult('playlist_id is required');
        const tracks = await provider.getPlaylistTracks(playlistId);
        if (tracks.length === 0) return textResult('Playlist is empty.');
        const lines = tracks.map((t, i) => `${i + 1}. ${formatTrack(t)}`);
        return textResult(lines.join('\n'));
      }

      case 'create_playlist': {
        const name = args.name as string;
        const description = args.description as string | undefined;
        if (!name) return errorResult('name is required');
        const playlist = await provider.createPlaylist(name, description);
        return textResult(
          `Created playlist "${playlist.name}" (${playlist.id})\nURI: ${playlist.uri}`
        );
      }

      case 'add_to_playlist': {
        const playlistId = args.playlist_id as string;
        const trackUris = args.track_uris as string[];
        if (!playlistId) return errorResult('playlist_id is required');
        if (!trackUris || trackUris.length === 0)
          return errorResult('track_uris is required and must not be empty');
        const result = await provider.addToPlaylist(playlistId, trackUris);
        return textResult(result);
      }

      case 'get_queue': {
        const queue = await provider.getQueue();
        if (queue.length === 0) return textResult('Queue is empty.');
        const lines = queue.map((t, i) => {
          const prefix = i === 0 && t.is_playing ? 'Now playing' : `${i}`;
          return `${prefix}. ${formatTrack(t)}`;
        });
        return textResult(lines.join('\n'));
      }

      case 'set_volume': {
        const percent = args.percent as number;
        if (percent == null) return errorResult('percent is required');
        const result = await provider.setVolume(percent);
        return textResult(result);
      }

      case 'set_shuffle': {
        const enabled = args.enabled as boolean;
        if (enabled == null) return errorResult('enabled is required');
        const result = await provider.setShuffle(enabled);
        return textResult(result);
      }

      default:
        return errorResult(`Unknown tool: ${name}`);
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    return errorResult(message);
  }
}

async function handleRequest(
  request: JsonRpcRequest
): Promise<Record<string, unknown> | null> {
  const { id, method, params } = request;

  switch (method) {
    case 'initialize':
      return {
        jsonrpc: '2.0',
        id,
        result: {
          protocolVersion: '2024-11-05',
          capabilities: { tools: {} },
          serverInfo: SERVER_INFO,
        },
      };

    case 'notifications/initialized':
      // Notification -- no response required
      return null;

    case 'tools/list':
      return {
        jsonrpc: '2.0',
        id,
        result: { tools: TOOLS },
      };

    case 'tools/call': {
      const toolName = (params as Record<string, unknown>)?.name as string;
      const toolArgs =
        ((params as Record<string, unknown>)?.arguments as Record<string, unknown>) || {};

      if (!toolName) {
        return {
          jsonrpc: '2.0',
          id,
          error: { code: -32602, message: 'Missing tool name in params' },
        };
      }

      const result = await callTool(toolName, toolArgs);
      return {
        jsonrpc: '2.0',
        id,
        result,
      };
    }

    default:
      return {
        jsonrpc: '2.0',
        id,
        error: { code: -32601, message: `Method not found: ${method}` },
      };
  }
}

// --- stdio transport ---
const rl = readline.createInterface({ input: process.stdin });

rl.on('line', async (line: string) => {
  try {
    const request = JSON.parse(line) as JsonRpcRequest;
    const response = await handleRequest(request);
    if (response) {
      process.stdout.write(JSON.stringify(response) + '\n');
    }
  } catch {
    // Ignore malformed JSON lines
  }
});
