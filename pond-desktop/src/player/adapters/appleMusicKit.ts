// Apple Music through MusicKit JS. Only this file knows MusicKit's vocabulary; everything it
// hands back is the player's own (types.ts).
//
// What a live run against Apple established, and this leans on: the developer token is enough to
// configure and to search; `authorize()` needs a click; `setQueue({song: id})` then `play()`
// starts a song; and MusicKit reports "playing" for a moment BEFORE a license failure lands as a
// `mediaPlaybackError`, so "playing" alone never counts as success (see `confirmPlaying`).

import {
  PlayerError,
  type AdapterCapabilities,
  type ItemKind,
  type ItemRef,
  type LibraryKind,
  type PlayerAdapter,
  type PlayerState,
  type PlaylistInfo,
  type RepeatMode,
  type Track,
  type Unsubscribe,
} from "../types";

/** The parts of MusicKit's instance this adapter touches. */
export interface MusicKitInstance {
  isAuthorized: boolean;
  storefrontId: string;
  volume: number;
  shuffleMode: number;
  repeatMode: number;
  playbackState: number;
  currentPlaybackTime: number;
  nowPlayingItem: unknown;
  api: { music(path: string, params?: Record<string, unknown>): Promise<unknown> };
  authorize(): Promise<unknown>;
  setQueue(options: Record<string, string>): Promise<unknown>;
  play(): Promise<unknown>;
  pause(): unknown;
  skipToNextItem(): Promise<unknown>;
  skipToPreviousItem(): Promise<unknown>;
  seekToTime(seconds: number): Promise<unknown>;
  playNext(options: Record<string, string>): Promise<unknown>;
  playLater(options: Record<string, string>): Promise<unknown>;
  addEventListener(name: string, listener: (event: unknown) => void): void;
  removeEventListener(name: string, listener: (event: unknown) => void): void;
}

export interface MusicKitGlobal {
  configure(config: {
    developerToken: string;
    app: { name: string; build: string };
  }): Promise<unknown> | unknown;
  getInstance(): MusicKitInstance;
  PlaybackStates: Record<
    | "playing"
    | "paused"
    | "loading"
    | "waiting"
    | "stalled"
    | "seeking"
    | "ended"
    | "completed",
    number
  >;
  PlayerShuffleMode: { off: number; songs: number };
  PlayerRepeatMode: { none: number; one: number; all: number };
}

export interface AppleDeps {
  loadMusicKit(): Promise<MusicKitGlobal>;
  /** Asks the host, which holds the key, for a signed developer token. */
  fetchDeveloperToken(): Promise<string>;
  /** How long a played song has to start before it is reported as not starting. */
  playConfirmMs?: number;
  /** A sentence when this window cannot play protected audio, else null; asked before anything loads. */
  checkDrm?(): Promise<string | null>;
}

const MUSICKIT_URL = "https://js-cdn.music.apple.com/musickit/v3/musickit.js";
const APP = { name: "Goose In A Pond", build: "1.0" };
/** Audio that has advanced this far is really playing, not just announced. */
const PLAYING_AFTER_MS = 500;

/** Loads Apple's script once and resolves the global it defines. */
export function loadMusicKitFromApple(): Promise<MusicKitGlobal> {
  return new Promise((resolve, reject) => {
    const existing = (window as unknown as { MusicKit?: MusicKitGlobal }).MusicKit;
    if (existing) return resolve(existing);
    document.addEventListener(
      "musickitloaded",
      () =>
        resolve((window as unknown as { MusicKit: MusicKitGlobal }).MusicKit),
      { once: true },
    );
    const script = document.createElement("script");
    script.src = MUSICKIT_URL;
    script.async = true;
    script.onerror = () =>
      reject(
        new Error(
          "Could not load MusicKit from Apple. Check this computer's internet connection and the network setting.",
        ),
      );
    document.head.appendChild(script);
  });
}

interface ApiItem {
  id?: string;
  type?: string;
  attributes?: {
    name?: string;
    artistName?: string;
    albumName?: string;
    durationInMillis?: number;
    artwork?: { url?: string };
    playParams?: { id?: string; catalogId?: string };
  };
}

function artwork(url: string | undefined): string | undefined {
  return url?.replace("{w}", "300").replace("{h}", "300");
}

function trackFromApi(item: ApiItem, kind: ItemKind = "song"): Track | null {
  const a = item.attributes;
  if (!a?.name) return null;
  // A library entry's own id is not playable everywhere; the catalog id is.
  const id = a.playParams?.catalogId ?? a.playParams?.id ?? item.id;
  if (!id) return null;
  const art = artwork(a.artwork?.url);
  return {
    id,
    kind,
    title: a.name,
    artist: a.artistName ?? "",
    album: a.albumName ?? "",
    duration_ms: a.durationInMillis ?? 0,
    ...(art ? { artwork_url: art } : {}),
  };
}

/**
 * MusicKit wraps the API's JSON as `{ data: body }`, and a list endpoint's body has its own `data`
 * array. So only an object under `data` is the wrapper; an array there means this is the body.
 */
function body(reply: unknown): Record<string, unknown> {
  const r = (reply ?? {}) as Record<string, unknown>;
  const inner = r.data;
  return inner && typeof inner === "object" && !Array.isArray(inner)
    ? (inner as Record<string, unknown>)
    : r;
}

function describeFailure(event: unknown): PlayerError {
  const e = (event ?? {}) as {
    description?: string;
    message?: string;
    data?: { errorCode?: number };
  };
  const detail = String(e.description ?? e.message ?? "");
  const code = e.data?.errorCode;
  if (/LICENSE/i.test(detail) || code === -42605) {
    return new PlayerError(
      "drm_refused",
      `Apple refused the playback license (${detail || "MEDIA_LICENSE"}${code ? `, code ${code}` : ""}). The Widevine module in this app may not be one Apple accepts yet.`,
    );
  }
  if (/SUBSCRIPTION/i.test(detail)) {
    return new PlayerError(
      "player_error",
      "The Apple ID signed in here has no active Apple Music subscription.",
    );
  }
  return new PlayerError(
    "player_error",
    `Apple Music could not play this: ${detail || "unknown error"}.`,
  );
}

export class AppleMusicKitAdapter implements PlayerAdapter {
  readonly service = "apple";
  readonly label = "Apple Music";
  readonly capabilities: AdapterCapabilities = {
    queue: true,
    playlists: true,
    library: true,
  };

  private mk: MusicKitGlobal | null = null;
  private music: MusicKitInstance | null = null;
  private failure: PlayerError | null = null;
  private readonly listeners = new Set<(s: PlayerState) => void>();
  private snapshot: PlayerState = {
    service: "apple",
    ready: false,
    need: "setup",
    status: "idle",
    track: null,
    position_ms: 0,
    volume: 100,
    shuffle: false,
    repeat: "off",
    message: "Starting Apple Music...",
  };

  constructor(private readonly deps: AppleDeps) {}

  state(): PlayerState {
    return this.snapshot;
  }

  onState(listener: (state: PlayerState) => void): Unsubscribe {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private set(patch: Partial<PlayerState>): void {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const l of [...this.listeners]) l(this.snapshot);
  }

  // ── Starting up ─────────────────────────────────────────────

  async init(): Promise<void> {
    try {
      const drm = await this.deps.checkDrm?.();
      if (drm) {
        this.set({ ready: false, need: "setup", message: drm });
        return;
      }
      const mk = await this.deps.loadMusicKit();
      const developerToken = await this.deps.fetchDeveloperToken();
      await mk.configure({ developerToken, app: APP });
      this.mk = mk;
      this.music = mk.getInstance();
      this.wire(this.music);
      this.refresh();
    } catch (error) {
      this.set({
        ready: false,
        need: "setup",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }

  async authorize(): Promise<void> {
    if (!this.music) {
      throw new PlayerError("not_ready", this.snapshot.message ?? "Apple Music is not set up.");
    }
    try {
      await this.music.authorize();
    } catch (error) {
      this.set({
        message: `Sign-in did not finish: ${error instanceof Error ? error.message : String(error)}`,
      });
      return;
    }
    this.refresh();
  }

  private wire(music: MusicKitInstance): void {
    const sync = () => this.refresh();
    for (const name of [
      "playbackStateDidChange",
      "nowPlayingItemDidChange",
      "playbackTimeDidChange",
      "playbackVolumeDidChange",
      "shuffleModeDidChange",
      "repeatModeDidChange",
      "authorizationStatusDidChange",
    ]) {
      music.addEventListener(name, sync);
    }
    music.addEventListener("mediaPlaybackError", (event) => {
      this.failure = describeFailure(event);
      this.refresh();
    });
  }

  /** Rebuilds the snapshot from the instance, the one source of truth. */
  private refresh(): void {
    const { mk, music } = this;
    if (!mk || !music) return;

    const S = mk.PlaybackStates;
    const p = music.playbackState;
    let status: PlayerState["status"] = "idle";
    if (p === S.playing) status = "playing";
    else if (p === S.paused) status = "paused";
    else if (p === S.loading || p === S.waiting || p === S.stalled || p === S.seeking)
      status = "buffering";
    else if (p === S.ended || p === S.completed) status = "ended";
    // An error stays until the next attempt: MusicKit walks through paused and seeking after it.
    if (this.failure) status = "error";

    const item = music.nowPlayingItem as
      | (ApiItem & {
          title?: string;
          artistName?: string;
          albumName?: string;
          playbackDuration?: number;
        })
      | null;
    const track: Track | null = item?.id
      ? {
          id: item.id,
          kind: "song",
          title: item.title ?? item.attributes?.name ?? "",
          artist: item.artistName ?? item.attributes?.artistName ?? "",
          album: item.albumName ?? item.attributes?.albumName ?? "",
          duration_ms:
            item.attributes?.durationInMillis ?? item.playbackDuration ?? 0,
          ...(artwork(item.attributes?.artwork?.url)
            ? { artwork_url: artwork(item.attributes?.artwork?.url)! }
            : {}),
        }
      : null;

    const signedIn = music.isAuthorized;
    this.set({
      ready: signedIn,
      need: signedIn ? "none" : "authorization",
      status,
      track,
      position_ms: Math.round((music.currentPlaybackTime || 0) * 1000),
      volume: Math.round((music.volume ?? 1) * 100),
      shuffle: music.shuffleMode === mk.PlayerShuffleMode.songs,
      repeat:
        music.repeatMode === mk.PlayerRepeatMode.one
          ? "one"
          : music.repeatMode === mk.PlayerRepeatMode.all
            ? "all"
            : "off",
      message: this.failure
        ? this.failure.message
        : signedIn
          ? undefined
          : "Sign in to Apple Music to play.",
    });
  }

  // ── Guards ──────────────────────────────────────────────────

  private started(): MusicKitInstance {
    if (!this.music) {
      throw new PlayerError("not_ready", this.snapshot.message ?? "Apple Music is not set up.");
    }
    return this.music;
  }

  private signedIn(): MusicKitInstance {
    const music = this.started();
    if (!music.isAuthorized) {
      throw new PlayerError(
        "needs_authorization",
        "Sign in to Apple Music in the player window first.",
      );
    }
    return music;
  }

  private async api(path: string, params?: Record<string, unknown>): Promise<Record<string, unknown>> {
    try {
      return body(await this.started().api.music(path, params));
    } catch (error) {
      if (error instanceof PlayerError) throw error;
      throw new PlayerError(
        "player_error",
        `Apple Music did not answer: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  // ── Finding things ──────────────────────────────────────────

  async search(query: string, opts: { limit?: number } = {}): Promise<Track[]> {
    const music = this.started();
    const limit = Math.max(1, Math.min(25, opts.limit ?? 5));
    const reply = await this.api(`/v1/catalog/${music.storefrontId}/search`, {
      term: query,
      types: "songs",
      limit,
    });
    const songs = ((reply.results as { songs?: { data?: ApiItem[] } } | undefined)?.songs?.data ?? []);
    return songs.map((s) => trackFromApi(s)).filter((t): t is Track => t !== null);
  }

  async playlists(): Promise<PlaylistInfo[]> {
    this.signedIn();
    const out: PlaylistInfo[] = [];
    let path: string | undefined = "/v1/me/library/playlists";
    let params: Record<string, unknown> | undefined = { limit: 100 };
    // Three pages is 300 playlists; beyond that a spoken name would not find one anyway.
    for (let page = 0; page < 3 && path; page++) {
      const reply = await this.api(path, params);
      for (const p of (reply.data as ApiItem[] | undefined) ?? []) {
        if (p.id && p.attributes?.name) out.push({ id: p.id, name: p.attributes.name });
      }
      path = reply.next as string | undefined;
      params = undefined;
    }
    return out;
  }

  async library(kind: LibraryKind, limit: number): Promise<Track[]> {
    this.signedIn();
    const capped = Math.max(1, Math.min(kind === "recent" ? 30 : 100, limit));
    const path =
      kind === "recent" ? "/v1/me/recent/played/tracks" : "/v1/me/library/songs";
    const reply = await this.api(path, { limit: capped });
    return ((reply.data as ApiItem[] | undefined) ?? [])
      .map((s) => trackFromApi(s))
      .filter((t): t is Track => t !== null);
  }

  // ── Playing ─────────────────────────────────────────────────

  /**
   * Resolves once the position has really advanced, rejects on an error event or a timeout.
   * Registered before `play()` is called, so an error that arrives at once is not missed.
   */
  private confirmPlaying(): { done: Promise<void>; cancel(): void } {
    let cancel = () => undefined as void;
    const done = new Promise<void>((resolve, reject) => {
      const finish = (settle: () => void) => {
        off();
        clearTimeout(timer);
        settle();
      };
      const check = (s: PlayerState) => {
        if (this.failure) finish(() => reject(this.failure));
        else if (s.status === "playing" && s.position_ms >= PLAYING_AFTER_MS)
          finish(resolve);
      };
      const off = this.onState(check);
      const wait = this.deps.playConfirmMs ?? 15_000;
      const timer = setTimeout(
        () =>
          finish(() =>
            reject(
              new PlayerError(
                "player_error",
                `Apple Music did not start playing within ${Math.round(wait / 1000)} seconds.`,
              ),
            ),
          ),
        wait,
      );
      cancel = () => finish(() => undefined);
      check(this.snapshot);
    });
    return { done, cancel };
  }

  private async begin(start: () => Promise<unknown>): Promise<void> {
    this.failure = null;
    const confirm = this.confirmPlaying();
    // Nobody else awaits this until `start` finishes; a rejection meanwhile is handled below.
    confirm.done.catch(() => undefined);
    try {
      await start();
    } catch (error) {
      confirm.cancel();
      throw error instanceof PlayerError
        ? error
        : new PlayerError(
            "player_error",
            `Apple Music could not start: ${error instanceof Error ? error.message : String(error)}`,
          );
    }
    await confirm.done;
  }

  async play(ref: ItemRef): Promise<void> {
    const music = this.signedIn();
    await this.begin(async () => {
      await music.setQueue({ [ref.kind]: ref.id });
      await music.play();
    });
  }

  async resume(): Promise<void> {
    const music = this.signedIn();
    await this.begin(() => music.play());
  }

  async enqueue(ref: ItemRef, where: "next" | "last"): Promise<void> {
    const music = this.signedIn();
    const options = { [ref.kind]: ref.id };
    await (where === "next" ? music.playNext(options) : music.playLater(options));
  }

  async pause(): Promise<void> {
    await this.started().pause();
  }

  async next(): Promise<void> {
    await this.signedIn().skipToNextItem();
  }

  async previous(): Promise<void> {
    await this.signedIn().skipToPreviousItem();
  }

  async seek(positionMs: number): Promise<void> {
    await this.signedIn().seekToTime(Math.max(0, positionMs) / 1000);
  }

  async setVolume(percent: number): Promise<void> {
    this.started().volume = Math.max(0, Math.min(100, percent)) / 100;
    this.refresh();
  }

  async setShuffle(enabled: boolean): Promise<void> {
    const music = this.started();
    const modes = this.mk!.PlayerShuffleMode;
    music.shuffleMode = enabled ? modes.songs : modes.off;
    this.refresh();
  }

  async setRepeat(mode: RepeatMode): Promise<void> {
    const music = this.started();
    const modes = this.mk!.PlayerRepeatMode;
    music.repeatMode = mode === "one" ? modes.one : mode === "all" ? modes.all : modes.none;
    this.refresh();
  }
}
