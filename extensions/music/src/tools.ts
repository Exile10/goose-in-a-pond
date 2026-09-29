import type { MusicProvider } from "./providers/types.js";

/** One tool as MCP lists it. */
export interface Tool {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
}

const SPOTIFY_PLAY =
  "Play music on Spotify. Music keeps playing afterwards: a song starts inside its album so the album follows on, and a single is topped up with more by the same artist — do not tell the user playback will stop after the song, and do not queue extra songs yourself to keep it going. Search picks the closest match, which is not always what was asked for — tell the user the track name and artist FROM THE RESULT, never the name they asked for.";

const APPLE_PLAY =
  "Play music in Apple Music. A song in the user's library starts at once; one only in the Apple Music catalog is added to their library first when catalog sign-in is set up, and is otherwise opened in the Music app. Search picks the closest match, which is not always what was asked for — tell the user the track name and artist FROM THE RESULT, never the name they asked for, and repeat what the result says happened rather than assuming playback started.";

function playTool(provider: MusicProvider): Tool {
  const spotify = provider.id === "spotify";

  const properties: Record<string, unknown> = {
    query: {
      type: "string",
      description:
        "What to play, as the user said it — 'Marvin's Room by Drake', 'Randoms', 'jazz'. Omit to resume what is paused.",
    },
    target: {
      type: "string",
      enum: ["track", "playlist"],
      description: spotify
        ? "'playlist' matches the user's own playlists loosely by name, preferring ones they created. Default 'track' searches songs, artists and albums."
        : "'playlist' matches the user's playlists loosely by name. Default 'track' searches songs.",
    },
  };

  if (provider.capabilities.queue) {
    properties.when = {
      type: "string",
      enum: ["now", "next"],
      description:
        "'next' appends to the queue and lets the current track finish; Spotify cannot insert at a chosen position, and cannot queue a whole playlist, so this applies to tracks only. Default 'now' replaces what is playing.",
    };
  }

  properties.uri = {
    type: "string",
    description: spotify
      ? "A pasted Spotify URI or link. Plays it directly, and is the only way to reach a playlist outside the user's library."
      : "A pasted Apple Music song link, or a URI from an earlier result. Plays it directly.",
  };

  return {
    name: "play",
    description: spotify ? SPOTIFY_PLAY : APPLE_PLAY,
    inputSchema: { type: "object", properties },
  };
}

function playlistsTool(provider: MusicProvider): Tool {
  return {
    name: "playlists",
    description:
      provider.id === "spotify"
        ? "List every playlist in the user's Spotify library, separated into ones they created and ones they follow from other people. Use this to answer 'what playlists do I have' or 'which of these are mine', and to find the exact name before playing one with the 'play' tool."
        : "List every playlist in the user's Music library. Use this to answer 'what playlists do I have', and to find the exact name before playing one with the 'play' tool.",
    inputSchema: { type: "object", properties: {} },
  };
}

function libraryTool(provider: MusicProvider): Tool {
  const spotify = provider.id === "spotify";

  const properties: Record<string, unknown> = {
    action: {
      type: "string",
      enum: ["saved", "top_tracks", "top_artists", "recent"],
      description: spotify
        ? "saved = list liked songs; top_tracks / top_artists = what they listen to most; recent = recently played."
        : "saved = list favourite songs; top_tracks / top_artists = what they have played most, by lifetime play count; recent = recently played.",
    },
  };

  if (provider.capabilities.timeRange) {
    properties.time_range = {
      type: "string",
      enum: ["short_term", "medium_term", "long_term"],
      description:
        "How far back top_tracks / top_artists look: short_term is about 4 weeks, medium_term about 6 months, long_term is several years. Defaults to medium_term.",
    };
  }

  properties.limit = {
    type: "number",
    description: "How many to return, 1-50. Defaults to 20.",
  };

  return {
    name: "library",
    description: spotify
      ? "The user's own Spotify library and listening history: their liked songs, what they listen to most, and what they played recently. Read-only — Spotify does not let this app change what is liked. Use for 'what are my liked songs', 'what do I listen to most', 'what was I playing yesterday'."
      : "The user's own Apple Music library and listening history, read from the Music app: their favourite songs, what they have played most, and what they played recently. Read-only. Use for 'what are my favourite songs', 'what do I listen to most', 'what was I playing yesterday'.",
    inputSchema: { type: "object", properties, required: ["action"] },
  };
}

function devicesTool(provider: MusicProvider): Tool {
  const spotify = provider.id === "spotify";
  return {
    name: "devices",
    description: spotify
      ? "List the devices Spotify can play on (phone, computer, speaker, TV), or move playback to one of them. Call with no arguments to see what is available; pass transfer_to with a device name to move the music there without interrupting it. Use this for 'play this on the speaker', 'move it to my phone', 'where can I play this'."
      : "List the AirPlay speakers and devices Music can play to, or switch to one of them. Call with no arguments to see what is available; pass transfer_to with a device name to send the music there. Use this for 'play this on the speaker', 'where can I play this'.",
    inputSchema: {
      type: "object",
      properties: {
        transfer_to: {
          type: "string",
          description:
            "Name of the device to move playback to, as the user said it (e.g. 'my phone', 'kitchen speaker'). Matched loosely against the device list. Omit to just list devices.",
        },
      },
    },
  };
}

function statusTool(provider: MusicProvider): Tool {
  return {
    name: "status",
    description:
      provider.id === "spotify"
        ? "Get what is currently playing on Spotify — track name, artist, album, progress, and upcoming queue."
        : "Get what is currently playing in Apple Music — track name, artist, album and progress.",
    inputSchema: { type: "object", properties: {} },
  };
}

function controlTool(provider: MusicProvider): Tool {
  return {
    name: "control",
    description: `Control ${provider.name} playback: pause, resume, next, previous, set volume, toggle shuffle, jump within the track, or set repeat.`,
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
            "seek",
            "repeat_off",
            "repeat_track",
            "repeat_all",
          ],
          description:
            "The playback action to perform. 'seek' jumps within the current track (give position); 'repeat_track' loops the song, 'repeat_all' loops the album or playlist, 'repeat_off' stops looping.",
        },
        volume: {
          type: "number",
          description: "Exact volume level (0-100). Required when action is 'set_volume'.",
        },
        position: {
          type: "string",
          description:
            "Where to jump to, for action 'seek'. Accepts 'm:ss' like '1:30', or a plain number of seconds like '90'.",
        },
      },
      required: ["action"],
    },
  };
}

/** The tools this provider can honour: a capability it lacks is left out, not advertised and refused. */
export function buildTools(provider: MusicProvider): Tool[] {
  return [
    playTool(provider),
    playlistsTool(provider),
    libraryTool(provider),
    ...(provider.capabilities.devices ? [devicesTool(provider)] : []),
    statusTool(provider),
    controlTool(provider),
  ];
}
