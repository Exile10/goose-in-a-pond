import { describe, expect, it } from "vitest";
import {
  AppleMusicKitAdapter,
  type AppleDeps,
  type MusicKitGlobal,
  type MusicKitInstance,
} from "./appleMusicKit";
import { PlayerError } from "../types";

type Scenario = "plays" | "license" | "silent" | "announces-only";

/** MusicKit as the live run showed it: it says "playing" first, and a license error can follow. */
class FakeMusic implements MusicKitInstance {
  isAuthorized = false;
  storefrontId = "us";
  volume = 1;
  shuffleMode = 0;
  repeatMode = 0;
  playbackState = 0;
  currentPlaybackTime = 0;
  nowPlayingItem: unknown = null;
  scenario: Scenario = "plays";
  refuseSignIn = false;
  calls: Array<[string, ...unknown[]]> = [];
  apiCalls: Array<[string, Record<string, unknown> | undefined]> = [];
  replies = new Map<string, unknown>();
  private listeners = new Map<string, Set<(e: unknown) => void>>();

  api = {
    music: async (path: string, params?: Record<string, unknown>) => {
      this.apiCalls.push([path, params]);
      const reply = this.replies.get(path);
      if (reply instanceof Error) throw reply;
      return reply ?? { data: {} };
    },
  };

  addEventListener(name: string, l: (e: unknown) => void) {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name)!.add(l);
  }
  removeEventListener(name: string, l: (e: unknown) => void) {
    this.listeners.get(name)?.delete(l);
  }
  emit(name: string, payload: unknown = {}) {
    this.listeners.get(name)?.forEach((l) => l(payload));
  }

  async authorize() {
    if (this.refuseSignIn) throw new Error("the window was closed");
    this.isAuthorized = true;
    this.emit("authorizationStatusDidChange");
    return "user-token";
  }
  async setQueue(o: Record<string, string>) {
    this.calls.push(["setQueue", o]);
    if (o.song === "reject") throw new Error("no such song");
    this.nowPlayingItem = {
      id: Object.values(o)[0],
      title: "Nairobi",
      artistName: "Bensoul",
      albumName: "Qwarantunes",
      attributes: { durationInMillis: 210_000, artwork: { url: "https://x/{w}x{h}.jpg" } },
    };
    this.emit("nowPlayingItemDidChange");
  }
  async play() {
    this.calls.push(["play"]);
    if (this.scenario === "silent") return;
    this.playbackState = 2;
    this.emit("playbackStateDidChange");
    if (this.scenario === "announces-only") return;
    setTimeout(() => {
      if (this.scenario === "license") {
        this.emit("mediaPlaybackError", {
          description: "MEDIA_LICENSE",
          data: { errorCode: -42605 },
        });
      } else {
        this.currentPlaybackTime = 1;
        this.emit("playbackTimeDidChange");
      }
    }, 0);
  }
  pause() {
    this.calls.push(["pause"]);
    this.playbackState = 3;
    this.emit("playbackStateDidChange");
  }
  async skipToNextItem() { this.calls.push(["next"]); }
  async skipToPreviousItem() { this.calls.push(["previous"]); }
  async seekToTime(s: number) { this.calls.push(["seek", s]); }
  async playNext(o: Record<string, string>) { this.calls.push(["playNext", o]); }
  async playLater(o: Record<string, string>) { this.calls.push(["playLater", o]); }
}

function fakeKit(music: FakeMusic) {
  const configured: unknown[] = [];
  const kit: MusicKitGlobal = {
    configure: async (c) => void configured.push(c),
    getInstance: () => music,
    PlaybackStates: {
      playing: 2, paused: 3, loading: 1, waiting: 7, stalled: 8, seeking: 6, ended: 5, completed: 9,
    },
    PlayerShuffleMode: { off: 0, songs: 1 },
    PlayerRepeatMode: { none: 0, one: 1, all: 2 },
  };
  return { kit, configured };
}

async function ready(over: { authorised?: boolean; scenario?: Scenario; playConfirmMs?: number } = {}) {
  const music = new FakeMusic();
  music.isAuthorized = over.authorised ?? true;
  music.scenario = over.scenario ?? "plays";
  const { kit, configured } = fakeKit(music);
  const deps: AppleDeps = {
    loadMusicKit: async () => kit,
    fetchDeveloperToken: async () => "dev-token",
    playConfirmMs: over.playConfirmMs ?? 200,
  };
  const adapter = new AppleMusicKitAdapter(deps);
  await adapter.init();
  return { adapter, music, configured };
}

describe("AppleMusicKitAdapter: starting up", () => {
  it("configures MusicKit with the host's developer token", async () => {
    const { configured } = await ready();
    expect(configured).toEqual([
      { developerToken: "dev-token", app: { name: "Goose In A Pond", build: "1.0" } },
    ]);
  });

  it("says so, and loads nothing, when this window cannot play protected audio", async () => {
    let loaded = false;
    const adapter = new AppleMusicKitAdapter({
      loadMusicKit: async () => {
        loaded = true;
        return fakeKit(new FakeMusic()).kit;
      },
      fetchDeveloperToken: async () => "t",
      checkDrm: async () => "This build has no Widevine module.",
    });
    await adapter.init();
    expect(loaded).toBe(false);
    expect(adapter.state()).toMatchObject({ ready: false, need: "setup" });
    expect(adapter.state().message).toContain("Widevine");
  });

  it("asks for sign-in when nobody is signed in", async () => {
    const { adapter } = await ready({ authorised: false });
    expect(adapter.state()).toMatchObject({ ready: false, need: "authorization" });
    expect(adapter.state().message).toMatch(/sign in/i);
  });

  it("is ready at once when a sign-in is remembered", async () => {
    const { adapter } = await ready({ authorised: true });
    expect(adapter.state()).toMatchObject({ ready: true, need: "none" });
  });

  it("says what is missing when the host has no key", async () => {
    const adapter = new AppleMusicKitAdapter({
      loadMusicKit: async () => fakeKit(new FakeMusic()).kit,
      fetchDeveloperToken: async () => {
        throw new Error("Apple Music is not set up: add your Team ID.");
      },
    });
    await adapter.init();
    expect(adapter.state()).toMatchObject({ ready: false, need: "setup" });
    expect(adapter.state().message).toContain("Team ID");
  });

  it("does not load Apple's script for a household that has not set up Apple Music", async () => {
    let loads = 0;
    const adapter = new AppleMusicKitAdapter({
      loadMusicKit: async () => {
        loads += 1;
        return fakeKit(new FakeMusic()).kit;
      },
      fetchDeveloperToken: async () => {
        throw new Error("Apple Music is not set up: add your Team ID.");
      },
    });
    // The setup retry asks every ten seconds, so this is asked over and over on a pond with no key.
    await adapter.init();
    await adapter.init();
    await adapter.init();
    expect(loads).toBe(0);
  });

  it("says so when Apple's script cannot be loaded", async () => {
    const adapter = new AppleMusicKitAdapter({
      loadMusicKit: async () => {
        throw new Error("Could not load MusicKit from Apple.");
      },
      fetchDeveloperToken: async () => "t",
    });
    await adapter.init();
    expect(adapter.state().need).toBe("setup");
    expect(adapter.state().message).toContain("MusicKit");
  });

  it("signs in on request and becomes ready", async () => {
    const { adapter } = await ready({ authorised: false });
    await adapter.authorize();
    expect(adapter.state()).toMatchObject({ ready: true, need: "none" });
  });

  it("reports a sign-in window closed early, without throwing", async () => {
    const { adapter, music } = await ready({ authorised: false });
    music.refuseSignIn = true;
    await adapter.authorize();
    expect(adapter.state().ready).toBe(false);
    expect(adapter.state().message).toContain("did not finish");
  });
});

describe("AppleMusicKitAdapter: guards", () => {
  it("refuses everything before it has started", async () => {
    const adapter = new AppleMusicKitAdapter({
      loadMusicKit: async () => { throw new Error("offline"); },
      fetchDeveloperToken: async () => "t",
    });
    await adapter.init();
    await expect(adapter.play({ id: "1", kind: "song" })).rejects.toMatchObject({ code: "not_ready" });
    await expect(adapter.search("x")).rejects.toMatchObject({ code: "not_ready" });
  });

  it("can search without a sign-in but cannot play or read the library", async () => {
    const { adapter, music } = await ready({ authorised: false });
    music.replies.set("/v1/catalog/us/search", { data: { results: { songs: { data: [] } } } });
    await expect(adapter.search("x")).resolves.toEqual([]);
    await expect(adapter.play({ id: "1", kind: "song" })).rejects.toMatchObject({ code: "needs_authorization" });
    await expect(adapter.playlists()).rejects.toMatchObject({ code: "needs_authorization" });
    await expect(adapter.library("saved", 5)).rejects.toMatchObject({ code: "needs_authorization" });
  });
});

describe("AppleMusicKitAdapter: finding things", () => {
  it("searches the account's store and maps the songs", async () => {
    const { adapter, music } = await ready();
    music.storefrontId = "ke";
    music.replies.set("/v1/catalog/ke/search", {
      data: {
        results: {
          songs: {
            data: [
              {
                id: "1001",
                attributes: {
                  name: "Nairobi",
                  artistName: "Bensoul",
                  albumName: "Qwarantunes",
                  durationInMillis: 210_000,
                  artwork: { url: "https://x/{w}x{h}.jpg" },
                },
              },
              { id: "no-name", attributes: {} },
            ],
          },
        },
      },
    });

    const tracks = await adapter.search("nairobi bensoul", { limit: 3 });

    expect(music.apiCalls[0]).toEqual([
      "/v1/catalog/ke/search",
      { term: "nairobi bensoul", types: "songs", limit: 3 },
    ]);
    expect(tracks).toEqual([
      {
        id: "1001",
        kind: "song",
        title: "Nairobi",
        artist: "Bensoul",
        album: "Qwarantunes",
        duration_ms: 210_000,
        artwork_url: "https://x/300x300.jpg",
      },
    ]);
  });

  it("reads a reply that is not wrapped in data", async () => {
    const { adapter, music } = await ready();
    music.replies.set("/v1/catalog/us/search", {
      results: { songs: { data: [{ id: "7", attributes: { name: "A", artistName: "B" } }] } },
    });
    expect((await adapter.search("a")).map((t) => t.id)).toEqual(["7"]);
  });

  it("keeps the search limit within what Apple allows", async () => {
    const { adapter, music } = await ready();
    await adapter.search("a", { limit: 500 });
    await adapter.search("a", { limit: 0 });
    expect(music.apiCalls.map((c) => c[1]?.limit)).toEqual([25, 1]);
  });

  it("turns an Apple API failure into a player error", async () => {
    const { adapter, music } = await ready();
    music.replies.set("/v1/catalog/us/search", new Error("429"));
    await expect(adapter.search("a")).rejects.toBeInstanceOf(PlayerError);
  });

  it("lists library playlists across pages", async () => {
    const { adapter, music } = await ready();
    // The shape MusicKit really returns: the API's body wrapped in `data`.
    music.replies.set("/v1/me/library/playlists", {
      data: {
        data: [{ id: "p.1", attributes: { name: "Road trip" } }],
        next: "/v1/me/library/playlists?offset=100",
      },
    });
    music.replies.set("/v1/me/library/playlists?offset=100", {
      data: { data: [{ id: "p.2", attributes: { name: "Focus" } }] },
    });
    expect(await adapter.playlists()).toEqual([
      { id: "p.1", name: "Road trip" },
      { id: "p.2", name: "Focus" },
    ]);
  });

  it("also reads a list reply that arrives without the wrapper", async () => {
    const { adapter, music } = await ready();
    music.replies.set("/v1/me/library/playlists", {
      data: [{ id: "p.9", attributes: { name: "Unwrapped" } }],
    });
    expect(await adapter.playlists()).toEqual([{ id: "p.9", name: "Unwrapped" }]);
  });

  it("reads the library and prefers the catalog id of what it finds", async () => {
    const { adapter, music } = await ready();
    music.replies.set("/v1/me/library/songs", {
      data: {
        data: [
          {
            id: "i.abc",
            attributes: { name: "Nairobi", artistName: "Bensoul", playParams: { id: "i.abc", catalogId: "1001" } },
          },
        ],
      },
    });
    music.replies.set("/v1/me/recent/played/tracks", { data: { data: [] } });

    expect((await adapter.library("saved", 10))[0]?.id).toBe("1001");
    await adapter.library("recent", 500);
    expect(music.apiCalls.at(-1)).toEqual(["/v1/me/recent/played/tracks", { limit: 30 }]);
  });
});

describe("AppleMusicKitAdapter: playing", () => {
  it("queues the song and resolves once audio has really advanced", async () => {
    const { adapter, music } = await ready();
    await adapter.play({ id: "1001", kind: "song" });
    expect(music.calls).toEqual([["setQueue", { song: "1001" }], ["play"]]);
    expect(adapter.state()).toMatchObject({ status: "playing", track: { title: "Nairobi" } });
  });

  it("queues an album or a playlist under its own key", async () => {
    const { adapter, music } = await ready();
    await adapter.play({ id: "p.1", kind: "playlist" });
    expect(music.calls[0]).toEqual(["setQueue", { playlist: "p.1" }]);
  });

  it("reports a refused license, with its code, even though MusicKit said playing first", async () => {
    const { adapter } = await ready({ scenario: "license" });
    const outcome = adapter.play({ id: "1001", kind: "song" });
    await expect(outcome).rejects.toMatchObject({ code: "drm_refused" });
    await expect(outcome).rejects.toThrow(/Apple refused the playback license/);
    expect(adapter.state().status).toBe("error");
    expect(adapter.state().message).toContain("-42605");
  });

  it("does not count 'playing' as playing until the position moves", async () => {
    const { adapter, music } = await ready({ scenario: "announces-only", playConfirmMs: 500 });
    let settled = false;
    const outcome = adapter.play({ id: "1", kind: "song" }).then(() => (settled = true));

    await new Promise((r) => setTimeout(r, 30));
    expect(settled).toBe(false);

    music.currentPlaybackTime = 2;
    music.emit("playbackTimeDidChange");
    await outcome;
    expect(settled).toBe(true);
  });

  it("times out honestly when nothing starts", async () => {
    const { adapter } = await ready({ scenario: "silent", playConfirmMs: 40 });
    await expect(adapter.play({ id: "1", kind: "song" })).rejects.toThrow(/did not start playing within/);
  });

  it("reports a song Apple cannot queue", async () => {
    const { adapter } = await ready();
    await expect(adapter.play({ id: "reject", kind: "song" })).rejects.toMatchObject({ code: "player_error" });
  });

  it("clears an earlier failure when the next attempt works", async () => {
    const { adapter, music } = await ready({ scenario: "license" });
    await adapter.play({ id: "1", kind: "song" }).catch(() => undefined);
    expect(adapter.state().status).toBe("error");

    music.scenario = "plays";
    music.currentPlaybackTime = 0;
    await adapter.play({ id: "2", kind: "song" });
    expect(adapter.state()).toMatchObject({ status: "playing" });
    expect(adapter.state().message).toBeUndefined();
  });

  it("explains a missing subscription", async () => {
    const { adapter, music } = await ready();
    music.emit("mediaPlaybackError", { description: "SUBSCRIPTION_ERROR" });
    expect(adapter.state().message).toMatch(/subscription/i);
  });

  it("puts a song at the front or the back of the queue", async () => {
    const { adapter, music } = await ready();
    await adapter.enqueue({ id: "9", kind: "song" }, "next");
    await adapter.enqueue({ id: "8", kind: "song" }, "last");
    expect(music.calls).toEqual([["playNext", { song: "9" }], ["playLater", { song: "8" }]]);
  });
});

describe("AppleMusicKitAdapter: transport", () => {
  it("maps each control onto MusicKit and reflects it in the state", async () => {
    const { adapter, music } = await ready();

    await adapter.next();
    await adapter.previous();
    await adapter.seek(90_000);
    await adapter.pause();
    await adapter.setVolume(30);
    await adapter.setShuffle(true);
    await adapter.setRepeat("all");

    expect(music.calls).toEqual([["next"], ["previous"], ["seek", 90], ["pause"]]);
    expect(music.volume).toBeCloseTo(0.3);
    expect(adapter.state()).toMatchObject({ status: "paused", volume: 30, shuffle: true, repeat: "all" });

    await adapter.setShuffle(false);
    await adapter.setRepeat("off");
    expect(adapter.state()).toMatchObject({ shuffle: false, repeat: "off" });
    await adapter.setRepeat("one");
    expect(adapter.state().repeat).toBe("one");
  });

  it("keeps volume within 0 to 100 and seek from going negative", async () => {
    const { adapter, music } = await ready();
    await adapter.setVolume(500);
    expect(music.volume).toBe(1);
    await adapter.setVolume(-5);
    expect(music.volume).toBe(0);
    await adapter.seek(-1_000);
    expect(music.calls.at(-1)).toEqual(["seek", 0]);
  });

  it("lets the user pause even before signing in", async () => {
    const { adapter, music } = await ready({ authorised: false });
    await adapter.pause();
    expect(music.calls).toEqual([["pause"]]);
  });

  it("notifies listeners, and stops when they unsubscribe", async () => {
    const { adapter, music } = await ready();
    const seen: string[] = [];
    const off = adapter.onState((s) => seen.push(s.status));
    music.playbackState = 3;
    music.emit("playbackStateDidChange");
    off();
    music.playbackState = 2;
    music.emit("playbackStateDidChange");
    expect(seen).toEqual(["paused"]);
  });
});

/** An adapter that sleeps until someone signs in, with everything that could leave the pond counted. */
function sleeper(
  over: {
    remembered?: boolean;
    authorised?: boolean;
    refuseSignIn?: boolean;
    probe?: () => Promise<void>;
    token?: () => Promise<string>;
  } = {},
) {
  const log = { tokenAsks: 0, loads: 0, probes: 0, remembered: over.remembered ?? false, remembers: 0 };
  const music = new FakeMusic();
  music.isAuthorized = over.authorised ?? false;
  music.refuseSignIn = over.refuseSignIn ?? false;
  const { kit, configured } = fakeKit(music);
  const adapter = new AppleMusicKitAdapter({
    loadMusicKit: async () => {
      log.loads += 1;
      return kit;
    },
    fetchDeveloperToken: async () => {
      log.tokenAsks += 1;
      return over.token ? over.token() : "dev-token";
    },
    lazy: {
      remembered: () => log.remembered,
      remember: () => {
        log.remembered = true;
        log.remembers += 1;
      },
      probe: async () => {
        log.probes += 1;
        await over.probe?.();
      },
    },
  });
  return { adapter, music, log, configured };
}

describe("AppleMusicKitAdapter: asleep until someone signs in", () => {
  it("fetches nothing and loads nothing at launch when nobody has signed in here", async () => {
    const { adapter, log } = sleeper();
    await adapter.init();

    expect(adapter.state()).toMatchObject({ ready: false, need: "authorization", dormant: true });
    expect(adapter.state().message).toBeUndefined();
    expect(log).toMatchObject({ tokenAsks: 0, loads: 0, probes: 1 });
  });

  it("stays asleep, and quiet, however often the setup retry asks", async () => {
    const { adapter, log } = sleeper();
    await adapter.init();
    await adapter.init();
    await adapter.init();

    expect(adapter.state().dormant).toBe(true);
    expect(log.tokenAsks).toBe(0);
    expect(log.loads).toBe(0);
  });

  it("still says early that Apple Music cannot work, in the pond's words, and does not sleep", async () => {
    const { adapter, log } = sleeper({
      probe: async () => {
        throw new Error("Apple Music sign-in is not available on this pond yet.");
      },
    });
    await adapter.init();

    expect(adapter.state()).toMatchObject({ ready: false, need: "setup", dormant: false });
    expect(adapter.state().message).toContain("not available on this pond");
    expect(log.tokenAsks).toBe(0);
    expect(log.loads).toBe(0);
  });

  it("answers a command with a sign-in instruction, which is what lets the extension fall back and say why", async () => {
    const { adapter } = sleeper();
    await adapter.init();

    for (const run of [
      () => adapter.play({ id: "1", kind: "song" }),
      () => adapter.search("nairobi"),
      () => adapter.pause(),
    ]) {
      const error = await run().catch((e) => e);
      expect(error).toBeInstanceOf(PlayerError);
      expect(error.code).toBe("needs_authorization");
      expect(error.message).toContain("Music extension's settings");
    }
  });

  it("wakes on prepare: a token, the script, and a configured player, and no longer dormant", async () => {
    const { adapter, log, configured } = sleeper();
    await adapter.init();
    await adapter.prepare();

    expect(log).toMatchObject({ tokenAsks: 1, loads: 1 });
    expect(configured).toHaveLength(1);
    expect(adapter.state()).toMatchObject({ need: "authorization", dormant: false });
  });

  it("wakes once however many ask at the same moment", async () => {
    const { adapter, log } = sleeper();
    await adapter.init();
    await Promise.all([adapter.prepare(), adapter.prepare(), adapter.prepare()]);
    await adapter.prepare();

    expect(log.tokenAsks).toBe(1);
    expect(log.loads).toBe(1);
  });

  it("stays asleep, and can be woken again, when waking fails", async () => {
    let fail = true;
    const { adapter, log } = sleeper({
      token: async () => {
        if (fail) throw new Error("Apple Music could not get its sign-in token: no network.");
        return "dev-token";
      },
    });
    await adapter.init();

    const error = await adapter.prepare().catch((e) => e);
    expect(error).toBeInstanceOf(PlayerError);
    expect(error.message).toContain("no network");
    expect(adapter.state().dormant).toBe(true);

    fail = false;
    await adapter.prepare();
    expect(adapter.state().dormant).toBe(false);
    expect(log.tokenAsks).toBe(2);
  });

  it("signing in wakes it, signs in, and remembers, so the next launch starts by itself", async () => {
    const { adapter, log, music } = sleeper();
    await adapter.init();
    await adapter.authorize();

    expect(music.isAuthorized).toBe(true);
    expect(adapter.state()).toMatchObject({ ready: true, need: "none", dormant: false });
    expect(log.remembers).toBe(1);
    expect(log.remembered).toBe(true);
  });

  it("does not remember a sign-in that did not finish", async () => {
    const { adapter, log } = sleeper({ refuseSignIn: true });
    await adapter.init();
    await adapter.authorize();

    expect(adapter.state().message).toContain("Sign-in did not finish");
    expect(log.remembers).toBe(0);
  });

  it("starts at launch, with no probe, for someone who has signed in here before", async () => {
    const { adapter, log } = sleeper({ remembered: true, authorised: true });
    await adapter.init();

    expect(log).toMatchObject({ tokenAsks: 1, loads: 1, probes: 0 });
    expect(adapter.state()).toMatchObject({ ready: true, need: "none", dormant: false });
  });

  it("counts a session MusicKit already held as a sign-in, so it is remembered from then on", async () => {
    const { adapter, log } = sleeper({ authorised: true });
    await adapter.init(); // asleep
    await adapter.prepare(); // woken by a sign-in click; MusicKit finds its stored session

    expect(log.remembers).toBe(1);
    expect(adapter.state().need).toBe("none");
  });

  it("asks a remembered service to sign in again, in the window, if its session is gone", async () => {
    const { adapter } = sleeper({ remembered: true, authorised: false });
    await adapter.init();

    // Started, not asleep: the person did use it, so the window may ask.
    expect(adapter.state()).toMatchObject({ need: "authorization", dormant: false });
  });

  it("behaves as it always did when nothing asks it to sleep", async () => {
    let tokenAsks = 0;
    const music = new FakeMusic();
    const { kit } = fakeKit(music);
    const adapter = new AppleMusicKitAdapter({
      loadMusicKit: async () => kit,
      fetchDeveloperToken: async () => {
        tokenAsks += 1;
        return "t";
      },
    });
    await adapter.init();
    expect(tokenAsks).toBe(1);
    expect(adapter.state().dormant).toBe(false);
  });
});
