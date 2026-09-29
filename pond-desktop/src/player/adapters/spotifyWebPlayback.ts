// Spotify through the Web Playback SDK. Only this file knows the SDK's vocabulary.
//
// Unlike MusicKit, Spotify's control plane is REST, and the Music extension already drives all of it
// (search, queue, library, playlists, devices) through `SpotifyProvider`. So this adapter is the
// speaker, not the controller: it registers this window as a Spotify Connect device, tells the host
// what is playing, and answers the transport commands that have no reason to leave the window. The
// extension then plays *to* this device with the same calls it makes for any other. Everything the
// REST provider already does stays where it is, which keeps the tool list and its wording unchanged.
//
// What is NOT established: audio. The SDK needs Widevine, and no run of this adapter has been made
// against Spotify with a signed-in Premium account. The state mapping follows the SDK's documented
// event shapes and is tested against a fake built from them, not against Spotify.

import {
  PlayerError,
  type AdapterCapabilities,
  type ItemRef,
  type LibraryKind,
  type PlayerAdapter,
  type PlayerDevice,
  type PlayerState,
  type PlaylistInfo,
  type RepeatMode,
  type Track,
  type Unsubscribe,
} from "../types";

interface SdkTrack {
  id?: string | null;
  uri?: string;
  name?: string;
  duration_ms?: number;
  album?: { name?: string; images?: Array<{ url?: string }> };
  artists?: Array<{ name?: string }>;
}

/** The SDK's `player_state_changed` payload, reduced to what this adapter reads. */
export interface SdkPlaybackState {
  paused: boolean;
  loading?: boolean;
  position: number;
  duration?: number;
  shuffle?: boolean;
  /** 0 off, 1 the context repeats, 2 the track repeats. */
  repeat_mode?: number;
  track_window?: { current_track?: SdkTrack | null };
}

export interface SdkPlayer {
  addListener(name: string, listener: (payload: never) => void): unknown;
  connect(): Promise<boolean>;
  disconnect(): void;
  pause(): Promise<void>;
  resume(): Promise<void>;
  nextTrack(): Promise<void>;
  previousTrack(): Promise<void>;
  seek(positionMs: number): Promise<void>;
  setVolume(volume: number): Promise<void>;
  /** The state right now, position included: the events only fire when something changes. */
  getCurrentState(): Promise<SdkPlaybackState | null>;
  activateElement?(): Promise<void>;
}

export interface SpotifyGlobal {
  Player: new (options: {
    name: string;
    getOAuthToken: (deliver: (token: string) => void) => void;
    volume?: number;
  }) => SdkPlayer;
}

export interface SpotifyDeps {
  loadSdk(): Promise<SpotifyGlobal>;
  /**
   * The person's access token from the host, which holds it. `refresh` asks Spotify for a new one
   * first: the SDK asks again when the last one stopped working.
   */
  fetchUserToken(refresh: boolean): Promise<string>;
  /** A sentence when this window cannot play protected audio, else null; asked before anything loads. */
  checkDrm?(): Promise<string | null>;
  /** What the device is called in Spotify's device list. */
  deviceName?: string;
}

const SDK_URL = "https://sdk.scdn.co/spotify-player.js";
const DEVICE_NAME = "Goose In A Pond";
/** How often the position is read while something plays: the SDK reports changes, not the clock. */
const TICK_MS = 1_000;

/** Loads Spotify's script once and resolves the global it defines. */
export function loadSpotifySdk(): Promise<SpotifyGlobal> {
  return new Promise((resolve, reject) => {
    const w = window as unknown as {
      Spotify?: SpotifyGlobal;
      onSpotifyWebPlaybackSDKReady?: () => void;
    };
    if (w.Spotify) return resolve(w.Spotify);
    // The script calls this global when it has defined `Spotify`, and only then.
    w.onSpotifyWebPlaybackSDKReady = () => resolve(w.Spotify as SpotifyGlobal);
    const script = document.createElement("script");
    script.src = SDK_URL;
    script.async = true;
    script.onerror = () =>
      reject(
        new Error(
          "Could not load Spotify's player. Check this computer's internet connection and the network setting.",
        ),
      );
    document.head.appendChild(script);
  });
}

const SPEAKER_ONLY =
  "Spotify is driven through its Web API by the Music extension; this window is only the speaker.";

function trackFrom(item: SdkTrack | null | undefined, fallbackDuration: number): Track | null {
  if (!item?.name) return null;
  const id = item.id ?? item.uri;
  if (!id) return null;
  const art = item.album?.images?.[0]?.url;
  return {
    id,
    kind: "song",
    title: item.name,
    artist: (item.artists ?? [])
      .map((a) => a.name)
      .filter(Boolean)
      .join(", "),
    album: item.album?.name ?? "",
    duration_ms: item.duration_ms ?? fallbackDuration,
    ...(art ? { artwork_url: art } : {}),
  };
}

function repeatFrom(mode: number | undefined): RepeatMode {
  return mode === 2 ? "one" : mode === 1 ? "all" : "off";
}

export class SpotifyWebPlaybackAdapter implements PlayerAdapter {
  readonly service = "spotify";
  readonly label = "Spotify";
  // Nothing here searches, queues or lists: see the note at the top of the file.
  readonly capabilities: AdapterCapabilities = {
    queue: false,
    playlists: false,
    library: false,
  };

  private player: SdkPlayer | null = null;
  private deviceId: string | null = null;
  private tokenCalls = 0;
  private ticker: ReturnType<typeof setInterval> | null = null;
  private readonly listeners = new Set<(s: PlayerState) => void>();
  private snapshot: PlayerState = {
    service: "spotify",
    ready: false,
    need: "setup",
    status: "idle",
    track: null,
    position_ms: 0,
    volume: 100,
    shuffle: false,
    repeat: "off",
    message: "Starting Spotify...",
  };

  constructor(private readonly deps: SpotifyDeps) {}

  state(): PlayerState {
    return this.snapshot;
  }

  onState(listener: (state: PlayerState) => void): Unsubscribe {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  device(): PlayerDevice {
    return {
      device_id: this.deviceId,
      name: this.deps.deviceName ?? DEVICE_NAME,
      ready: this.snapshot.ready && this.deviceId !== null,
    };
  }

  private set(patch: Partial<PlayerState>): void {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const l of [...this.listeners]) l(this.snapshot);
  }

  // ── Starting up ─────────────────────────────────────────────

  async init(): Promise<void> {
    if (this.player) return; // Already up; the setup retry must not open a second device.
    try {
      const drm = await this.deps.checkDrm?.();
      if (drm) {
        this.set({ ready: false, need: "setup", message: drm });
        return;
      }
      // Asked before the SDK loads: not signed in is the common case, and it needs no script.
      const first = await this.deps.fetchUserToken(false);
      const sdk = await this.deps.loadSdk();
      const player = new sdk.Player({
        name: this.deps.deviceName ?? DEVICE_NAME,
        // The first ask is answered with the token just fetched; any later one means the SDK found
        // it stale, so those go to Spotify for a new one.
        getOAuthToken: (deliver) => {
          const call = this.tokenCalls++;
          if (call === 0) return deliver(first);
          this.deps.fetchUserToken(true).then(deliver, (error) =>
            this.set({
              ready: false,
              need: "setup",
              message: error instanceof Error ? error.message : String(error),
            }),
          );
        },
        volume: 1,
      });
      this.wire(player);
      this.player = player;
      const connected = await player.connect();
      if (!connected) {
        this.player = null;
        this.set({
          ready: false,
          need: "setup",
          message:
            "Spotify's player would not connect. Check the network setting and that this account has Premium.",
        });
        return;
      }
      // Some webviews need this before the first sound; the window's autoplay policy usually makes
      // it a no-op, and it is never worth failing over.
      await player.activateElement?.().catch(() => undefined);
    } catch (error) {
      this.player = null;
      this.set({
        ready: false,
        need: "setup",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }

  /** Spotify signs in through the Music extension's settings, not in this window. */
  async authorize(): Promise<void> {
    throw new PlayerError(
      "unsupported",
      "Sign in to Spotify in the Music extension's settings; there is nothing to sign in to here.",
    );
  }

  private wire(player: SdkPlayer): void {
    player.addListener("ready", ((payload: { device_id: string }) => {
      this.deviceId = payload.device_id;
      this.set({ ready: true, need: "none", message: undefined });
    }) as (p: never) => void);
    player.addListener("not_ready", (() => {
      this.deviceId = null;
      this.syncTicker(false);
      this.set({
        ready: false,
        need: "none",
        message: "Spotify lost this player. It reconnects on its own when the network is back.",
      });
    }) as (p: never) => void);
    player.addListener("player_state_changed", ((state: SdkPlaybackState | null) => {
      this.fromSdk(state);
    }) as (p: never) => void);
    // These three fail before any sound, and each has a different fix, so each says its own.
    player.addListener("initialization_error", ((e: { message?: string }) => {
      this.set({
        ready: false,
        need: "setup",
        message: `Spotify's player could not start here${e?.message ? `: ${e.message}` : ""}. This build may have no Widevine module.`,
      });
    }) as (p: never) => void);
    player.addListener("authentication_error", (() => {
      this.set({
        ready: false,
        need: "setup",
        message:
          "Spotify refused the sign-in. Sign in to Spotify again in the Music extension's settings: the in-app player needs a permission the earlier sign-in did not ask for.",
      });
    }) as (p: never) => void);
    player.addListener("account_error", (() => {
      this.set({
        ready: false,
        need: "setup",
        message: "The in-app Spotify player needs a Premium account.",
      });
    }) as (p: never) => void);
    player.addListener("playback_error", ((e: { message?: string }) => {
      this.set({
        status: "error",
        message: `Spotify could not play this${e?.message ? `: ${e.message}` : ""}.`,
      });
      // Spotify moves on to the next track when one fails, and if the cause is not the track (a
      // module the service refuses, say) every one fails in turn: a few seconds of each song, then
      // the next. Stopping here keeps the error where it can be read.
      void player.pause().catch(() => undefined);
    }) as (p: never) => void);
  }

  /** Reads the position every second while playing, so the clock and the host's view move. */
  private syncTicker(playing: boolean): void {
    if (!playing) {
      if (this.ticker) clearInterval(this.ticker);
      this.ticker = null;
      return;
    }
    if (this.ticker || !this.player) return;
    this.ticker = setInterval(() => {
      const player = this.player;
      if (!player) return;
      // A failed read is not worth a message: the next event or tick sets it right.
      void player.getCurrentState().then(
        (state) => state && this.fromSdk(state),
        () => undefined,
      );
    }, TICK_MS);
  }

  /** The SDK sends null when this device stops being the active one. */
  private fromSdk(state: SdkPlaybackState | null): void {
    if (!state) {
      this.syncTicker(false);
      this.set({ status: "idle", track: null, position_ms: 0 });
      return;
    }
    const track = trackFrom(state.track_window?.current_track, state.duration ?? 0);
    let status: PlayerState["status"];
    if (!track) status = "idle";
    else if (!state.paused) status = state.loading ? "buffering" : "playing";
    else status = "paused";
    this.syncTicker(status === "playing");
    this.set({
      status,
      track,
      position_ms: Math.max(0, Math.floor(state.position)),
      shuffle: state.shuffle === true,
      repeat: repeatFrom(state.repeat_mode),
      // A playing state clears an earlier error message; an error stays until something plays.
      ...(status === "playing" || status === "paused" ? { message: undefined } : {}),
    });
  }

  // ── Transport: the SDK does these without a round trip through the Web API ────

  private ready(): SdkPlayer {
    if (!this.player || !this.snapshot.ready) {
      throw new PlayerError(
        "not_ready",
        this.snapshot.message ?? "Spotify's player is not ready yet.",
      );
    }
    return this.player;
  }

  async resume(): Promise<void> {
    await this.ready().resume();
  }
  async pause(): Promise<void> {
    await this.ready().pause();
  }
  async next(): Promise<void> {
    await this.ready().nextTrack();
  }
  async previous(): Promise<void> {
    await this.ready().previousTrack();
  }
  async seek(positionMs: number): Promise<void> {
    await this.ready().seek(Math.max(0, Math.floor(positionMs)));
  }
  async setVolume(percent: number): Promise<void> {
    const clamped = Math.min(100, Math.max(0, percent));
    await this.ready().setVolume(clamped / 100);
    this.set({ volume: clamped });
  }

  // ── The speaker does not choose music ────────────────────────────────────────

  async search(): Promise<Track[]> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
  async play(_ref: ItemRef): Promise<void> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
  async enqueue(): Promise<void> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
  async setShuffle(): Promise<void> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
  async setRepeat(): Promise<void> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
  async playlists(): Promise<PlaylistInfo[]> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
  async library(_kind: LibraryKind, _limit: number): Promise<Track[]> {
    throw new PlayerError("unsupported", SPEAKER_ONLY);
  }
}
