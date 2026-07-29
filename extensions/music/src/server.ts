#!/usr/bin/env node
/**
 * GIAP Music Extension — MCP server for Spotify playback.
 *
 * A small set of intent-shaped tools rather than one per endpoint (this was
 * once 14 tools, which was worse):
 *   play      — play a track, album, or one of the user's playlists, by name or URI
 *   queue     — append to the queue without interrupting the current track
 *   playlists — list the user's playlists by name
 *   status    — what's currently playing + queue
 *   control   — pause, resume, next, previous, volume, shuffle
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
        type: {
          type: "string",
          enum: ["track", "album", "playlist"],
          description:
            "What the query names, defaulting to 'track'. If the user says the word 'playlist' you MUST pass 'playlist', and if they say 'album' you MUST pass 'album' — leaving this unset searches the words as a SONG TITLE, so 'play the sautisol playlist' would start a single Sauti Sol track instead of their playlist. 'playlist' matches against the user's library by name; 'album' plays the whole record in order.",
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
    name: "play_playlist",
    description:
      "Play one of the playlists in the user's Spotify library, by name. Use this for ANY request to play a playlist — 'play my Randoms playlist', 'play the EDM playlist', 'play randoms' when Randoms is one of their playlists. Do NOT use the 'play' tool for a playlist: it searches the words as a song title and starts an unrelated track instead. Matches loosely, so a rough or misspelled name is fine, and prefers a playlist the user created over one they follow.",
    inputSchema: {
      type: "object",
      properties: {
        name: {
          type: "string",
          description:
            "The playlist name as the user said it (e.g. 'Randoms', 'sauti sol kenyan gold', 'EDM').",
        },
      },
      required: ["name"],
    },
  },
  {
    name: "playlists",
    description:
      "List every playlist in the user's Spotify library, separated into ones they created and ones they follow from other people. Use this to answer 'what playlists do I have' or 'which of these are mine', and to find the exact name before playing one with the 'play' tool.",
    inputSchema: { type: "object", properties: {} },
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

  // Playlist: match one of the user's own by name and play it. provider.play
  // sends a non-track URI as context_uri, so playback runs through the whole
  // playlist rather than stopping after one song.
  //
  // The word "playlist" in the query counts even when `type` was left unset.
  // Telling the model to set it did not work: asked to "play randoms playlist
  // in my library" it still searched those words as a song title, played an
  // unrelated track, and reported that it had started the playlist. A request
  // that says "playlist" is not ambiguous enough to justify guessing wrong.
  const saysPlaylist = !!query && /\bplaylists?\b/i.test(query);
  if (query && ((args.type as string | undefined) === "playlist" || saysPlaylist)) {
    const { uri: playlistUri, name } = await resolvePlaylist(query);
    await provider.play(playlistUri);
    return `Now playing playlist: ${name}`;
  }

  // Album: play the whole record in order, same context_uri mechanism.
  if (query && (args.type as string | undefined) === "album") {
    const albums = await provider.searchAlbums(query, 5);
    if (albums.length === 0) {
      return `No album found for "${query}". Try a different search.`;
    }
    const top = albums[0];
    await provider.play(top.uri);

    let text = `Now playing album: ${top.name} by ${top.artist} (${top.total_tracks} tracks, ${top.release_date})`;
    const others = albums.slice(1, 4);
    if (others.length > 0) {
      text +=
        "\n\nOther matches:\n" +
        others.map((a, i) => `${i + 2}. ${a.name} by ${a.artist}`).join("\n");
    }
    return text;
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
    // Say "track" outright. When the user asked for a playlist and this branch
    // ran anyway, a bare "Now playing: X" was reported back as "I started your
    // playlist"; naming what actually started makes the mismatch visible.
    let text = `Now playing track: ${top.name} by ${top.artist} (${top.album})`;
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

/** Lowercase, drop emoji and punctuation, collapse runs of whitespace. */
function normalizeName(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^\p{Letter}\p{Number}]+/gu, " ")
    .trim()
    .replace(/\s+/g, " ");
}

/**
 * Words people wrap around a playlist's actual name — "play the EDM playlist
 * from my library". Scored as content they drag the match around: they dilute
 * the real words, and "playlist" alone matched half of "Scratch Adventure - S*x
 * Playlist". Stripped from the query only; a playlist really called "... Playlist"
 * still matches on its remaining words.
 */
const QUERY_FILLER = new Set([
  "the", "a", "an", "my", "our", "from", "in", "on", "of", "please", "playlist",
  "playlists", "list", "library", "spotify", "called", "named", "one",
]);

/** Drops filler, keeping the original if that would leave nothing to match on. */
function contentWords(normalized: string): string {
  const kept = normalized.split(" ").filter(w => w && !QUERY_FILLER.has(w));
  return kept.length > 0 ? kept.join(" ") : normalized;
}

/**
 * Scores how well a spoken name matches a playlist's real one, 0 (no) to 1.
 *
 * Real playlist names are messy — "Sauti sol/Kenyan gold" with an emoji on the
 * end — and people say "sautisol". Plain substring matching fails on the missing
 * space alone, so compare with spaces removed as well, and fall back to how many
 * of the query's words the name actually contains, which survives a typo in one
 * of them.
 */
function playlistMatchScore(query: string, playlistName: string): number {
  const q = contentWords(normalizeName(query));
  const n = normalizeName(playlistName);
  if (!q || !n) return 0;
  if (q === n) return 1;

  const qSquashed = q.replace(/ /g, "");
  const nSquashed = n.replace(/ /g, "");
  if (qSquashed === nSquashed) return 0.95;
  if (nSquashed.includes(qSquashed) || qSquashed.includes(nSquashed)) return 0.9;

  const qWords = q.split(" ");
  const nSet = new Set(n.split(" "));
  const overlap = qWords.filter(w => nSet.has(w)).length;
  return overlap / qWords.length;
}

/**
 * Finds a playlist in the user's library by name. The model will have a name,
 * not an id, so requiring an id would make this unusable in practice.
 */
async function resolvePlaylist(name: string): Promise<{ uri: string; name: string }> {
  const playlists = await provider.getPlaylists();

  const ranked = playlists
    .map(p => ({ p, score: playlistMatchScore(name, p.name) }))
    // On a tie, prefer a playlist the user made. Libraries contain both a
    // followed "EDM" and their own "EDM", which score identically, and picking
    // the stranger's copy is the wrong guess every time.
    .sort((a, b) => b.score - a.score || Number(b.p.is_own) - Number(a.p.is_own));

  // Half the words matching is enough to be confident; below that the request
  // is better refused than answered with an arbitrary playlist.
  const best = ranked[0];
  if (best && best.score >= 0.5) return { uri: best.p.uri, name: best.p.name };

  const suggestions = ranked
    .slice(0, 8)
    .map(r => r.p.name)
    .join(", ");
  throw new Error(`No playlist matching "${name}". Closest: ${suggestions || "(none)"}`);
}

async function handlePlayPlaylist(args: Record<string, unknown>): Promise<string> {
  const name = (args.name ?? args.query) as string | undefined;
  if (!name) return "Which playlist? Give me its name.";

  const { uri, name: actual } = await resolvePlaylist(name);
  await provider.play(uri);
  return `Now playing playlist: ${actual}`;
}

async function handlePlaylists(): Promise<string> {
  const playlists = await provider.getPlaylists();
  if (playlists.length === 0) return "No playlists found on this Spotify account.";

  // Split them: "which of these did I make" is a question the raw list cannot
  // answer, and Spotify hands us the owner on every entry anyway.
  const mine = playlists.filter(p => p.is_own);
  const followed = playlists.filter(p => !p.is_own);

  let text = `${playlists.length} playlist(s) in the library: ${mine.length} created by the user, ${followed.length} followed from others.`;
  if (mine.length > 0) {
    text += `\n\nCreated by the user (${mine.length}):\n` + mine.map(p => `- ${p.name}`).join("\n");
  }
  if (followed.length > 0) {
    text +=
      `\n\nFollowed from other people (${followed.length}):\n` +
      followed.map(p => `- ${p.name} (by ${p.owner})`).join("\n");
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
          case "play_playlist":
            debug("play_playlist →", args.name ?? args.query ?? "");
            text = await handlePlayPlaylist(args);
            break;
          case "playlists":
            debug("playlists → listing");
            text = await handlePlaylists();
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
