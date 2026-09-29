// The player's vocabulary. Nothing here names a music service: an adapter maps one service's
// SDK onto these types, and the bridge, the extension and the UI only ever see these. The shapes
// are wire DTOs (snake_case, like the rest of the API) because the host relays them verbatim.

export type PlaybackStatus =
  | "idle"
  | "buffering"
  | "playing"
  | "paused"
  | "ended"
  | "error";

/** What the user has to do before commands can work. */
export type Need = "none" | "authorization" | "setup";

export type ItemKind = "song" | "album" | "playlist";

/** A thing to play, by the service's own id. */
export interface ItemRef {
  id: string;
  kind: ItemKind;
}

export interface Track {
  id: string;
  kind: ItemKind;
  title: string;
  artist: string;
  album: string;
  duration_ms: number;
  artwork_url?: string;
}

export interface PlaylistInfo {
  id: string;
  name: string;
}

export type RepeatMode = "off" | "one" | "all";

export interface PlayerState {
  service: string;
  /** Configured and signed in, so commands will be attempted. */
  ready: boolean;
  need: Need;
  status: PlaybackStatus;
  track: Track | null;
  position_ms: number;
  /** 0 to 100. */
  volume: number;
  shuffle: boolean;
  repeat: RepeatMode;
  /** The last problem, in words a person can act on. */
  message?: string;
  /**
   * Set up and asleep: nothing has been fetched and nothing has left the pond, because nobody has
   * pressed Sign in and nobody has signed in here before. `need` is "authorization" so a sign-in is
   * offered, but the window is not raised for it.
   */
  dormant?: boolean;
}

export interface AdapterCapabilities {
  queue: boolean;
  playlists: boolean;
  library: boolean;
}

export type LibraryKind = "saved" | "recent";

/**
 * A service that plays through a device other software chooses to play *to* (Spotify Connect)
 * reports it here: the id that software addresses, and whether it is ready to be addressed.
 */
export interface PlayerDevice {
  device_id: string | null;
  name: string;
  ready: boolean;
}

export type PlayerErrorCode =
  | "needs_authorization"
  | "not_ready"
  | "drm_refused"
  | "unsupported"
  | "bad_request"
  | "player_error";

/** A failure the caller can act on; `code` is stable and `message` is for a person. */
export class PlayerError extends Error {
  constructor(
    readonly code: PlayerErrorCode,
    message: string,
  ) {
    super(message);
    this.name = "PlayerError";
  }
}

export type Unsubscribe = () => void;

/** One music service, driven from inside the shell. */
export interface PlayerAdapter {
  /** The label the host and the extension use for this service, e.g. "apple". */
  readonly service: string;
  /** What a person calls it, e.g. "Apple Music". */
  readonly label: string;
  readonly capabilities: AdapterCapabilities;

  /** Loads the service's SDK and configures it. Leaves `need` set when the user must sign in. */
  init(): Promise<void>;
  /** Needs a user gesture: called from a button, never from a command. */
  authorize(): Promise<void>;

  state(): PlayerState;
  onState(listener: (state: PlayerState) => void): Unsubscribe;

  search(query: string, opts?: { limit?: number }): Promise<Track[]>;
  /** Resolves once audio is confirmed playing, and throws with a code if it is not. */
  play(ref: ItemRef): Promise<void>;
  enqueue(ref: ItemRef, where: "next" | "last"): Promise<void>;
  resume(): Promise<void>;
  pause(): Promise<void>;
  next(): Promise<void>;
  previous(): Promise<void>;
  seek(positionMs: number): Promise<void>;
  setVolume(percent: number): Promise<void>;
  setShuffle(enabled: boolean): Promise<void>;
  setRepeat(mode: RepeatMode): Promise<void>;
  playlists(): Promise<PlaylistInfo[]>;
  library(kind: LibraryKind, limit: number): Promise<Track[]>;
  /** Only for a service whose control plane is not in this window: the speaker it registered. */
  device?(): PlayerDevice;
  /**
   * For a service that sleeps until wanted: does the network work a sign-in needs (a token, the
   * SDK), and throws with words for a person if it cannot. Idempotent, and a no-op once awake.
   */
  prepare?(): Promise<void>;
}

/** A command as the host sends it. */
export interface PlayerCommand {
  id: string;
  service: string;
  op: string;
  args?: Record<string, unknown>;
}

/** What the page posts back for a command. */
export interface PlayerReply {
  id: string;
  ok: boolean;
  result?: unknown;
  error?: string;
  code?: PlayerErrorCode;
}
