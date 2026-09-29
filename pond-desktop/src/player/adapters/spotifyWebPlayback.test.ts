import { afterEach, describe, expect, it, vi } from "vitest";
import {
  SpotifyWebPlaybackAdapter,
  type SdkPlaybackState,
  type SdkPlayer,
  type SpotifyDeps,
  type SpotifyGlobal,
} from "./spotifyWebPlayback";
import { PlayerError } from "../types";

/**
 * The Web Playback SDK as its documentation describes it: a player that emits `ready` with a device
 * id once connected, and `player_state_changed` with the playback state. Built from the documented
 * shapes; no run against Spotify has been made, so these tests say what the adapter does with them,
 * not what Spotify does.
 */
class FakePlayer implements SdkPlayer {
  static instances: FakePlayer[] = [];
  listeners = new Map<string, Array<(payload: never) => void>>();
  calls: Array<[string, ...unknown[]]> = [];
  connectResult = true;
  autoReady: string | null = "device-1";
  getOAuthToken: (deliver: (t: string) => void) => void;
  name: string;

  constructor(options: {
    name: string;
    getOAuthToken: (deliver: (t: string) => void) => void;
  }) {
    this.name = options.name;
    this.getOAuthToken = options.getOAuthToken;
    FakePlayer.instances.push(this);
  }

  addListener(name: string, listener: (payload: never) => void) {
    const list = this.listeners.get(name) ?? [];
    list.push(listener);
    this.listeners.set(name, list);
  }
  emit(name: string, payload: unknown = {}) {
    for (const l of this.listeners.get(name) ?? []) l(payload as never);
  }
  async connect() {
    // The SDK asks for a token as it connects.
    this.getOAuthToken(() => undefined);
    if (this.connectResult && this.autoReady) this.emit("ready", { device_id: this.autoReady });
    return this.connectResult;
  }
  disconnect() {
    this.calls.push(["disconnect"]);
  }
  async pause() {
    this.calls.push(["pause"]);
  }
  async resume() {
    this.calls.push(["resume"]);
  }
  async nextTrack() {
    this.calls.push(["next"]);
  }
  async previousTrack() {
    this.calls.push(["previous"]);
  }
  async seek(ms: number) {
    this.calls.push(["seek", ms]);
  }
  async setVolume(v: number) {
    this.calls.push(["volume", v]);
  }
  async activateElement() {
    this.calls.push(["activate"]);
  }
  /** What the SDK would say the state is right now; a test sets it. */
  current: SdkPlaybackState | null = null;
  async getCurrentState() {
    this.calls.push(["getCurrentState"]);
    return this.current;
  }
}

function sdk(): SpotifyGlobal {
  return { Player: FakePlayer as unknown as SpotifyGlobal["Player"] };
}

function setup(over: Partial<SpotifyDeps> = {}) {
  FakePlayer.instances = [];
  const log = { sdkLoads: 0, tokenAsks: [] as boolean[], drmAsks: 0 };
  const deps: SpotifyDeps = {
    loadSdk: async () => {
      log.sdkLoads += 1;
      return sdk();
    },
    fetchUserToken: async (refresh) => {
      log.tokenAsks.push(refresh);
      return refresh ? "renewed-token" : "first-token";
    },
    ...over,
  };
  const adapter = new SpotifyWebPlaybackAdapter(deps);
  return { adapter, deps, log, player: () => FakePlayer.instances[0]! };
}

const track = {
  id: "4uLU6hMCjMI75M1A2tKUQC",
  uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
  name: "So What",
  duration_ms: 545_000,
  album: { name: "Kind of Blue", images: [{ url: "https://i.scdn.co/image/abc" }] },
  artists: [{ name: "Miles Davis" }, { name: "John Coltrane" }],
};

function playing(over: Partial<SdkPlaybackState> = {}): SdkPlaybackState {
  return {
    paused: false,
    position: 12_345.9,
    duration: 545_000,
    shuffle: false,
    repeat_mode: 0,
    track_window: { current_track: track },
    ...over,
  };
}

describe("SpotifyWebPlaybackAdapter: starting up", () => {
  it("asks about DRM first and loads nothing when the window cannot play protected audio", async () => {
    const { adapter, log } = setup({ checkDrm: async () => "This build has no Widevine module." });
    await adapter.init();
    expect(adapter.state()).toMatchObject({
      ready: false,
      need: "setup",
      message: "This build has no Widevine module.",
    });
    expect(log.sdkLoads).toBe(0);
    expect(log.tokenAsks).toEqual([]);
  });

  it("does not load the SDK for someone who has not signed in to Spotify", async () => {
    const { adapter, log } = setup({
      fetchUserToken: async () => {
        throw new Error("Spotify is not connected: sign in to Spotify in the Music extension's settings.");
      },
    });
    await adapter.init();
    expect(adapter.state().need).toBe("setup");
    expect(adapter.state().message).toContain("sign in to Spotify");
    expect(log.sdkLoads).toBe(0);
    expect(adapter.device().ready).toBe(false);
  });

  it("becomes a ready Connect device once the SDK reports its id", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    expect(adapter.state()).toMatchObject({ ready: true, need: "none" });
    expect(adapter.device()).toEqual({
      device_id: "device-1",
      name: "Goose In A Pond",
      ready: true,
    });
    expect(player().name).toBe("Goose In A Pond");
  });

  it("names the device what it is told to", async () => {
    const { adapter, player } = setup({ deviceName: "Kitchen" });
    await adapter.init();
    expect(player().name).toBe("Kitchen");
    expect(adapter.device().name).toBe("Kitchen");
  });

  it("gives the SDK the token it already fetched, and renews only when the SDK asks again", async () => {
    const { adapter, player, log } = setup();
    await adapter.init();
    expect(log.tokenAsks).toEqual([false]);

    const delivered: string[] = [];
    player().getOAuthToken((t) => delivered.push(t)); // the SDK found the token stale
    await Promise.resolve();
    await Promise.resolve();
    expect(log.tokenAsks).toEqual([false, true]);
    expect(delivered).toEqual(["renewed-token"]);
  });

  it("says why when a renewal fails, instead of leaving the SDK waiting in silence", async () => {
    const { adapter, player } = setup({
      fetchUserToken: async (refresh) => {
        if (refresh) throw new Error("Spotify would not renew the sign-in. Sign in to Spotify again.");
        return "first-token";
      },
    });
    await adapter.init();
    player().getOAuthToken(() => undefined);
    await new Promise((r) => setTimeout(r, 0));
    expect(adapter.state()).toMatchObject({ ready: false, need: "setup" });
    expect(adapter.state().message).toContain("Sign in to Spotify again");
  });

  it("reports a connection Spotify refused and lets the setup retry try again", async () => {
    const { adapter } = setup();
    // First attempt: the SDK refuses to connect.
    let refuse = true;
    const original = FakePlayer.prototype.connect;
    FakePlayer.prototype.connect = async function (this: FakePlayer) {
      this.connectResult = !refuse;
      return original.call(this);
    };
    try {
      await adapter.init();
      expect(adapter.state()).toMatchObject({ ready: false, need: "setup" });
      expect(adapter.state().message).toContain("would not connect");

      refuse = false;
      await adapter.init();
      expect(adapter.state()).toMatchObject({ ready: true, need: "none" });
      expect(FakePlayer.instances).toHaveLength(2);
    } finally {
      FakePlayer.prototype.connect = original;
    }
  });

  it("opens one device however often the setup retry calls init", async () => {
    const { adapter, log } = setup();
    await adapter.init();
    await adapter.init();
    await adapter.init();
    expect(FakePlayer.instances).toHaveLength(1);
    expect(log.sdkLoads).toBe(1);
  });

  it("goes not-ready, with no device id, when the SDK loses the device", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("not_ready", { device_id: "device-1" });
    expect(adapter.state().ready).toBe(false);
    expect(adapter.device()).toMatchObject({ device_id: null, ready: false });
    expect(adapter.state().message).toContain("reconnects");
  });
});

describe("SpotifyWebPlaybackAdapter: what it reports", () => {
  it("maps a playing state to the player's own vocabulary", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("player_state_changed", playing());
    expect(adapter.state()).toMatchObject({
      status: "playing",
      position_ms: 12_345,
      track: {
        id: "4uLU6hMCjMI75M1A2tKUQC",
        kind: "song",
        title: "So What",
        artist: "Miles Davis, John Coltrane",
        album: "Kind of Blue",
        duration_ms: 545_000,
        artwork_url: "https://i.scdn.co/image/abc",
      },
    });
  });

  it("tells paused, loading and empty apart", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("player_state_changed", playing({ paused: true }));
    expect(adapter.state().status).toBe("paused");
    player().emit("player_state_changed", playing({ loading: true }));
    expect(adapter.state().status).toBe("buffering");
    player().emit("player_state_changed", playing({ track_window: { current_track: null } }));
    expect(adapter.state()).toMatchObject({ status: "idle", track: null });
  });

  it("goes idle when the SDK reports null because this device is no longer the active one", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("player_state_changed", playing());
    player().emit("player_state_changed", null);
    expect(adapter.state()).toMatchObject({ status: "idle", track: null, position_ms: 0 });
  });

  it("maps shuffle and the SDK's numeric repeat modes", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("player_state_changed", playing({ shuffle: true, repeat_mode: 1 }));
    expect(adapter.state()).toMatchObject({ shuffle: true, repeat: "all" });
    player().emit("player_state_changed", playing({ repeat_mode: 2 }));
    expect(adapter.state()).toMatchObject({ shuffle: false, repeat: "one" });
    player().emit("player_state_changed", playing({ repeat_mode: 0 }));
    expect(adapter.state().repeat).toBe("off");
  });

  it("falls back to the state's duration and the track's uri when the track lacks them", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit(
      "player_state_changed",
      playing({
        duration: 200_000,
        track_window: {
          current_track: { id: null, uri: "spotify:track:x", name: "Untitled", artists: [] },
        },
      }),
    );
    expect(adapter.state().track).toMatchObject({
      id: "spotify:track:x",
      duration_ms: 200_000,
      artist: "",
      album: "",
    });
    expect(adapter.state().track).not.toHaveProperty("artwork_url");
  });

  it("notifies listeners and stops when they unsubscribe", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    const seen: string[] = [];
    const stop = adapter.onState((s) => seen.push(s.status));
    player().emit("player_state_changed", playing());
    stop();
    player().emit("player_state_changed", playing({ paused: true }));
    expect(seen).toEqual(["playing"]);
  });
});

describe("SpotifyWebPlaybackAdapter: the clock", () => {
  afterEach(() => vi.useRealTimers());

  it("reads the position every second while playing, since the SDK only reports changes", async () => {
    vi.useFakeTimers();
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("player_state_changed", playing({ position: 1_000 }));
    expect(adapter.state().position_ms).toBe(1_000);

    player().current = playing({ position: 4_000 });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(adapter.state().position_ms).toBe(4_000);

    player().current = playing({ position: 5_000 });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(adapter.state().position_ms).toBe(5_000);
  });

  it("stops reading when playback pauses, when the device is lost, and when it is no longer active", async () => {
    vi.useFakeTimers();
    const { adapter, player } = setup();
    await adapter.init();
    const reads = () => player().calls.filter((c) => c[0] === "getCurrentState").length;

    player().emit("player_state_changed", playing());
    await vi.advanceTimersByTimeAsync(2_000);
    const whilePlaying = reads();
    expect(whilePlaying).toBe(2);

    player().emit("player_state_changed", playing({ paused: true }));
    await vi.advanceTimersByTimeAsync(5_000);
    expect(reads()).toBe(whilePlaying);

    player().emit("player_state_changed", playing());
    player().emit("not_ready", { device_id: "device-1" });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(reads()).toBe(whilePlaying);

    player().emit("player_state_changed", playing());
    player().emit("player_state_changed", null);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(reads()).toBe(whilePlaying);
  });

  it("runs one clock however many playing events arrive", async () => {
    vi.useFakeTimers();
    const { adapter, player } = setup();
    await adapter.init();
    for (let i = 0; i < 5; i++) player().emit("player_state_changed", playing());
    await vi.advanceTimersByTimeAsync(1_000);
    expect(player().calls.filter((c) => c[0] === "getCurrentState")).toHaveLength(1);
  });

  it("survives a failed read and keeps the last position", async () => {
    vi.useFakeTimers();
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("player_state_changed", playing({ position: 2_000 }));
    player().getCurrentState = async () => {
      throw new Error("the SDK is busy");
    };
    await vi.advanceTimersByTimeAsync(3_000);
    expect(adapter.state()).toMatchObject({ status: "playing", position_ms: 2_000 });
  });
});

describe("SpotifyWebPlaybackAdapter: the SDK's failures", () => {
  it("says what each setup failure needs", async () => {
    const { adapter, player } = setup();
    await adapter.init();

    player().emit("initialization_error", { message: "EME is not supported" });
    expect(adapter.state()).toMatchObject({ ready: false, need: "setup" });
    expect(adapter.state().message).toContain("EME is not supported");
    expect(adapter.state().message).toContain("Widevine");

    player().emit("authentication_error", { message: "Invalid token scopes." });
    expect(adapter.state().message).toContain("Sign in to Spotify again");

    player().emit("account_error", { message: "premium required" });
    expect(adapter.state().message).toContain("Premium");
  });

  it("marks a playback error and clears it once something plays", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    player().emit("playback_error", { message: "Cannot perform operation" });
    expect(adapter.state().status).toBe("error");
    expect(adapter.state().message).toContain("Cannot perform operation");

    player().emit("player_state_changed", playing());
    expect(adapter.state().status).toBe("playing");
    expect(adapter.state().message).toBeUndefined();
  });
});

describe("SpotifyWebPlaybackAdapter: transport", () => {
  it("drives the SDK directly", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    await adapter.resume();
    await adapter.pause();
    await adapter.next();
    await adapter.previous();
    await adapter.seek(90_000.7);
    await adapter.setVolume(30);
    expect(player().calls).toEqual([
      ["activate"],
      ["resume"],
      ["pause"],
      ["next"],
      ["previous"],
      ["seek", 90_000],
      ["volume", 0.3],
    ]);
    expect(adapter.state().volume).toBe(30);
  });

  it("keeps volume between 0 and 100", async () => {
    const { adapter, player } = setup();
    await adapter.init();
    await adapter.setVolume(250);
    await adapter.setVolume(-5);
    expect(player().calls.filter((c) => c[0] === "volume")).toEqual([
      ["volume", 1],
      ["volume", 0],
    ]);
  });

  it("refuses a command with not_ready, and the reason, when there is no device", async () => {
    const { adapter } = setup({
      fetchUserToken: async () => {
        throw new Error("Spotify is not connected.");
      },
    });
    await adapter.init();
    for (const run of [
      () => adapter.resume(),
      () => adapter.pause(),
      () => adapter.next(),
      () => adapter.setVolume(10),
    ]) {
      const error = await run().catch((e) => e);
      expect(error).toBeInstanceOf(PlayerError);
      expect(error.code).toBe("not_ready");
      expect(error.message).toContain("not connected");
    }
  });
});

describe("SpotifyWebPlaybackAdapter: the speaker does not choose music", () => {
  it("leaves search, queue, library and modes to the Music extension's Web API provider", async () => {
    const { adapter } = setup();
    await adapter.init();
    for (const run of [
      () => adapter.search(),
      () => adapter.play({ id: "x", kind: "song" }),
      () => adapter.enqueue(),
      () => adapter.setShuffle(),
      () => adapter.setRepeat(),
      () => adapter.playlists(),
      () => adapter.library("saved", 5),
      () => adapter.authorize(),
    ]) {
      const error = await run().catch((e) => e);
      expect(error).toBeInstanceOf(PlayerError);
      expect(error.code).toBe("unsupported");
    }
    expect(adapter.capabilities).toEqual({ queue: false, playlists: false, library: false });
  });
});
