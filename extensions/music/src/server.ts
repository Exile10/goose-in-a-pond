#!/usr/bin/env node
/**
 * GIAP Music Extension — MCP server for Spotify playback.
 *
 * 3 focused tools (not 14):
 *   play     — search by name and play, or resume/play by URI
 *   status   — what's currently playing + queue
 *   control  — pause, resume, next, previous, volume, shuffle
 */
import * as readline from "readline";
import { SpotifyProvider } from "./providers/spotify.js";

const provider = new SpotifyProvider();

const TOOLS = [
  {
    name: "play",
    description:
      "Start playing music now, REPLACING whatever is currently playing and clearing the queue. Give a song name, artist, or album and it will search Spotify and play the best match. Examples: 'play Bohemian Rhapsody', 'play Drake', 'play chill vibes playlist'. To add something without interrupting the current track, use the 'queue' tool instead. Search picks the closest match, which is not always what was asked for — tell the user the track name and artist FROM THE RESULT, never the name they asked for.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description:
            "What to play — song name, artist, album, or mood (e.g. 'Marvin\\'s Room by Drake', 'jazz playlist', 'Kendrick Lamar').",
        },
        uri: {
          type: "string",
          description:
            "Spotify URI to play directly (spotify:track:..., spotify:album:..., spotify:playlist:...). Use this only if you already have a URI. Otherwise use query.",
        },
      },
    },
  },
  {
    name: "queue",
    description:
      "Add a song to the Spotify queue WITHOUT interrupting what is playing. The current track keeps playing and the song is appended after anything already queued. Use this whenever the user says queue, add, or 'after this' — never 'play', which would cut the current song off. Spotify has no way to insert at a specific position, so this always appends to the end.",
    inputSchema: {
      type: "object",
      properties: {
        query: {
          type: "string",
          description:
            "What to queue — song name and optionally the artist (e.g. 'Bomba Train by E-Sir').",
        },
        uri: {
          type: "string",
          description:
            "Spotify track URI to queue directly (spotify:track:...). Use this only if you already have a URI. Otherwise use query.",
        },
      },
    },
  },
  {
    name: "status",
    description:
      "Get what is currently playing on Spotify — track name, artist, album, progress, and upcoming queue.",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "control",
    description:
      "Control Spotify playback: pause, resume, next, previous, set volume, or toggle shuffle.",
    inputSchema: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: [
            "pause",
            "resume",
            "next",
            "previous",
            "volume_up",
            "volume_down",
            "set_volume",
            "shuffle_on",
            "shuffle_off",
          ],
          description: "The playback action to perform.",
        },
        volume: {
          type: "number",
          description: "Exact volume level (0-100). Required when action is 'set_volume'.",
        },
      },
      required: ["action"],
    },
  },
];

// ── Tool handlers ─────────────────────────────────────────────

async function handlePlay(args: Record<string, unknown>): Promise<string> {
  const query = args.query as string | undefined;
  const uri = args.uri as string | undefined;

  // Direct URI play
  if (uri) {
    const result = await provider.play(uri);
    return result;
  }

  // Search and play
  if (query) {
    const tracks = await provider.searchTracks(query, 5);
    if (tracks.length === 0) {
      return `No results found for "${query}". Try a different search.`;
    }

    const top = tracks[0];
    await provider.play(top.uri);

    const others = tracks.slice(1, 4);
    let text = `Now playing: ${top.name} by ${top.artist} (${top.album})`;
    if (others.length > 0) {
      text +=
        "\n\nOther matches:\n" +
        others.map((t, i) => `${i + 2}. ${t.name} by ${t.artist}`).join("\n");
    }
    return text;
  }

  // No query, no URI — resume
  const result = await provider.play();
  return result;
}

async function handleQueue(args: Record<string, unknown>): Promise<string> {
  const uri = args.uri as string | undefined;
  const query = args.query as string | undefined;

  if (uri) {
    await provider.addToQueue(uri);
    return `Queued ${uri}`;
  }

  if (!query) {
    return "Tell me what to queue — a song name, optionally with the artist.";
  }

  const tracks = await provider.searchTracks(query, 5);
  if (tracks.length === 0) {
    return `No results found for "${query}". Try a different search.`;
  }

  const top = tracks[0];
  await provider.addToQueue(top.uri);

  // Name what was queued so a wrong pick is visible and can be skipped, and
  // list the runners-up the way handlePlay does.
  let text = `Queued: ${top.name} by ${top.artist} (${top.album}). Current track keeps playing.`;
  const others = tracks.slice(1, 4);
  if (others.length > 0) {
    text +=
      "\n\nOther matches:\n" +
      others.map((t, i) => `${i + 2}. ${t.name} by ${t.artist}`).join("\n");
  }
  return text;
}

async function handleStatus(): Promise<string> {
  const now = await provider.getNowPlaying();
  if (!now) {
    return "Nothing is currently playing on Spotify.";
  }

  const progress = now.progress_ms
    ? `${Math.floor(now.progress_ms / 60000)}:${String(Math.floor((now.progress_ms % 60000) / 1000)).padStart(2, "0")}`
    : "0:00";
  const duration = `${Math.floor(now.duration_ms / 60000)}:${String(Math.floor((now.duration_ms % 60000) / 1000)).padStart(2, "0")}`;

  let text = `${now.is_playing ? "Playing" : "Paused"}: ${now.name} by ${now.artist}\nAlbum: ${now.album}\nProgress: ${progress} / ${duration}`;

  try {
    const queue = await provider.getQueue();
    const upcoming = queue.slice(1, 4);
    if (upcoming.length > 0) {
      text +=
        "\n\nUp next:\n" +
        upcoming.map((t, i) => `${i + 1}. ${t.name} by ${t.artist}`).join("\n");
    }
  } catch {
    // Queue not available — that's fine
  }

  return text;
}

async function handleControl(args: Record<string, unknown>): Promise<string> {
  const action = args.action as string;
  const volume = args.volume as number | undefined;

  switch (action) {
    case "pause":
      return provider.pause();
    case "resume":
      return provider.play();
    case "next":
      return provider.next();
    case "previous":
      return provider.previous();
    case "volume_up": {
      const now = await provider.getNowPlaying();
      const cur = now?.volume_percent ?? 50;
      return provider.setVolume(Math.min(100, cur + 10));
    }
    case "volume_down": {
      const now = await provider.getNowPlaying();
      const cur = now?.volume_percent ?? 50;
      return provider.setVolume(Math.max(0, cur - 10));
    }
    case "set_volume":
      return provider.setVolume(volume ?? 50);
    case "shuffle_on":
      return provider.setShuffle(true);
    case "shuffle_off":
      return provider.setShuffle(false);
    default:
      return `Unknown action: ${action}`;
  }
}

// ── Debug logging (goes to stderr, not stdout) ───────────────
function debug(...args: unknown[]) {
  console.error(`[music-ext]`, ...args);
}

// ── MCP JSON-RPC server ───────────────────────────────────────

interface JsonRpcRequest {
  jsonrpc: string;
  id?: number | string;
  method: string;
  params?: Record<string, unknown>;
}

async function handleRequest(
  request: JsonRpcRequest
): Promise<Record<string, unknown> | null> {
  const { method, id, params } = request;

  debug(`<-- ${method}`, params ? JSON.stringify(params).slice(0, 200) : "");

  switch (method) {
    case "initialize":
      debug("initializing");
      return {
        jsonrpc: "2.0",
        id,
        result: {
          protocolVersion: "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "giap-music", version: "0.2.0" },
        },
      };

    case "notifications/initialized":
      debug("initialized OK");
      return null;

    case "tools/list":
      debug(`listing ${TOOLS.length} tools`);
      return { jsonrpc: "2.0", id, result: { tools: TOOLS } };

    case "tools/call": {
      const toolName = (params as Record<string, unknown>)?.name as string;
      const args =
        ((params as Record<string, unknown>)?.arguments as Record<
          string,
          unknown
        >) ?? {};

      debug(`tool call: ${toolName}`, JSON.stringify(args));

      try {
        let text: string;
        switch (toolName) {
          case "play":
            debug("play →", args.query || args.uri || "(resume)");
            text = await handlePlay(args);
            break;
          case "queue":
            debug("queue →", args.query || args.uri || "(nothing)");
            text = await handleQueue(args);
            break;
          case "status":
            debug("status → checking now playing");
            text = await handleStatus();
            break;
          case "control":
            debug("control →", args.action, args.volume ?? "");
            text = await handleControl(args);
            break;
          default:
            debug("unknown tool:", toolName);
            return {
              jsonrpc: "2.0",
              id,
              error: { code: -32601, message: `Unknown tool: ${toolName}` },
            };
        }

        debug(`result (${text.length} chars):`, text.slice(0, 120));
        return {
          jsonrpc: "2.0",
          id,
          result: { content: [{ type: "text", text }] },
        };
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        debug("ERROR:", msg);
        return {
          jsonrpc: "2.0",
          id,
          result: {
            content: [{ type: "text", text: `Error: ${msg}` }],
            isError: true,
          },
        };
      }
    }

    default:
      return {
        jsonrpc: "2.0",
        id,
        error: { code: -32601, message: `Method not found: ${method}` },
      };
  }
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line: string) => {
  try {
    const request = JSON.parse(line) as JsonRpcRequest;
    const response = await handleRequest(request);
    if (response) {
      process.stdout.write(JSON.stringify(response) + "\n");
    }
  } catch {
    // ignore malformed JSON
  }
});
