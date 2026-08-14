import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    listDevices: vi.fn(),
    listSchedules: vi.fn(),
    listRecipes: vi.fn().mockResolvedValue([]),
    getWeather: vi.fn().mockResolvedValue({ enabled: false }),
    getNowPlaying: vi.fn().mockResolvedValue({ connected: false }),
  },
}));

import { api } from "../../api/PondApiClient";
import { getHomeData, refreshHomeData, refreshWeather, refreshNowPlaying,
         __resetHubDataForTests, __nowPlayingBackoffForTests,
         __tickNowPlayingPollForTests, __BACKOFF_TICKS_FOR_TESTS } from "./hubDataStore";
import { ROUTINES as MOCK_ROUTINES } from "../data/routines";

const apiMock = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  listDevices: ReturnType<typeof vi.fn>;
  listSchedules: ReturnType<typeof vi.fn>;
  listRecipes: ReturnType<typeof vi.fn>;
  getWeather: ReturnType<typeof vi.fn>;
  getNowPlaying: ReturnType<typeof vi.fn>;
};

describe("hubDataStore", () => {
  beforeEach(() => {
    __resetHubDataForTests();
    apiMock.getSettings.mockReset();
    apiMock.listDevices.mockReset();
    apiMock.listSchedules.mockReset();
    apiMock.listRecipes.mockReset();
    apiMock.listRecipes.mockResolvedValue([]);
    apiMock.getWeather.mockReset();
    apiMock.getWeather.mockResolvedValue({ enabled: false });
    apiMock.getNowPlaying.mockReset();
    apiMock.getNowPlaying.mockResolvedValue({ connected: false });
  });

  it("falls back to mock data when API returns empty devices", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.devices.length).toBeGreaterThan(0);
    expect(home.cameras.length).toBeGreaterThan(0);
    expect(home.user).toBe("Jerry");
  });

  it("uses settings.user_name when present", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    expect(getHomeData().user).toBe("Ada");
  });

  it("derives categories and rooms from real devices", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "lr1", name: "Lamp", device_type: "light",      is_online: true, room: "Living Room", metadata: { on: true } },
      { id: "fd",  name: "Door", device_type: "lock",       is_online: true, room: "Outdoor",     metadata: { locked: true } },
      { id: "th",  name: "Nest", device_type: "thermostat", is_online: true, room: "Living Room", metadata: { target: 72 } },
      { id: "cm",  name: "Cam",  device_type: "camera",     is_online: true, last_seen: "2026-06-01T13:48:00Z" },
    ]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.devices.map((d) => d.id).sort()).toEqual(["fd", "lr1", "th"]);
    expect(home.cameras.map((c) => c.id)).toEqual(["cm"]);
    expect(home.rooms.map((r) => r.name)).toContain("Home");
    expect(home.rooms.map((r) => r.name)).toContain("Living Room");
    expect(home.rooms.map((r) => r.name)).toContain("Outdoor");
    const lights = home.categories.find((c) => c.id === "lights");
    expect(lights?.status).toBe("1 on");
    const climate = home.categories.find((c) => c.id === "climate");
    expect(climate?.status).toBe("Heat to 72°");
  });

  it("uses real weather when the API reports enabled", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({
      enabled: true,
      temp: 71,
      cond: "Clear sky",
      icon: "sun",
      hi: 75,
      lo: 60,
      hum: 40,
      wind: 8,
      forecast: [{ d: "Wed", i: "rain", t: 55 }],
    });

    await refreshHomeData();
    const home = getHomeData();
    expect(home.weather.temp).toBe(71);
    expect(home.weather.icon).toBe("sun");
    expect(home.weather.forecast).toEqual([{ d: "Wed", i: "rain", t: 55 }]);
  });

  it("falls back to mock weather when the API reports disabled", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: false });

    await refreshHomeData();
    const home = getHomeData();
    expect(home.weather.temp).toBe(64);
    expect(home.weather.cond).toBe("Partly cloudy");
  });

  it("refreshWeather updates the weather slice without a full reload", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 16, cond: "Overcast", icon: "cloud" });

    await refreshHomeData();
    expect(getHomeData().weather.temp).toBe(16);

    // The sky changed while the dashboard sat open; only the poll re-runs.
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 21, cond: "Clear sky", icon: "sun" });
    apiMock.listDevices.mockClear();

    await refreshWeather();
    const home = getHomeData();
    expect(home.weather.temp).toBe(21);
    expect(home.weather.cond).toBe("Clear sky");
    expect(apiMock.listDevices).not.toHaveBeenCalled();
  });

  it("refreshWeather keeps the last reading when the fetch fails", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 16, cond: "Overcast", icon: "cloud" });

    await refreshHomeData();
    apiMock.getWeather.mockRejectedValue(new Error("server offline"));

    await refreshWeather();
    expect(getHomeData().weather.temp).toBe(16);
  });

  it("falls back to mock routines when no recipes returned", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([]);

    await refreshHomeData();
    const { useRoutines: _u, getHomeData: _g } = await import("./hubDataStore");
    // Sample directly via the module instance
    const { __resetHubDataForTests: _r } = await import("./hubDataStore");
    void _u; void _g; void _r;
    // Read routines through the singleton snapshot
    const mod = await import("./hubDataStore");
    // routines aren't on HomeData — read via the snapshot used by useRoutines
    const snapshot = (mod as unknown as { __getRoutinesForTests?: () => unknown[] }).__getRoutinesForTests?.()
      ?? MOCK_ROUTINES;
    expect(Array.isArray(snapshot)).toBe(true);
    expect((snapshot as { name: string }[]).map((r) => r.name)).toContain("Good Morning");
  });

  it("maps recipes to routines (known names reuse mock visual templates)", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([
      { name: "Good Morning", description: "wake up macro", yaml: "" },
      { name: "Sunset Bath",  description: "Run tub, dim lights, play jazz", yaml: "" },
    ]);

    await refreshHomeData();
    const mod = await import("./hubDataStore");
    const snapshot = (mod as unknown as { __getRoutinesForTests?: () => unknown[] }).__getRoutinesForTests?.() ?? [];
    expect(snapshot.length).toBe(2);
    // Known name should get the mock "Good Morning" template (with rich does list)
    const morning = (snapshot as { name: string; does: string[] }[]).find((r) => r.name === "Good Morning");
    expect(morning?.does.length).toBeGreaterThan(1);
    // Unknown name should derive does from description
    const sunset = (snapshot as { name: string; does: string[] }[]).find((r) => r.name === "Sunset Bath");
    expect(sunset?.does).toEqual(["Run tub", "dim lights", "play jazz"]);
  });

  // ── Now Playing ──────────────────────────────────────────────
  // Spotify refusing a call must never look like a paused player: a
  // development-mode app serves only allowlisted accounts, and everyone else
  // completes the whole OAuth flow before every API call 403s.

  async function loadWithNowPlaying(np: unknown) {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getNowPlaying.mockResolvedValue(np);
    await refreshHomeData();
    return getHomeData().nowPlaying;
  }

  it("surfaces a Spotify authorisation failure instead of an idle player", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      playing: false,
      error: "forbidden",
      message: "This Spotify account is not authorised for the app GIAP signs in with.",
    });

    expect(np.error).toBe("forbidden");
    expect(np.track).toBe("Spotify not authorised");
    expect(np.artist).toContain("not authorised");
    expect(np.track).not.toBe("Nothing playing");
    expect(np.playing).toBe(false);
  });

  it("labels non-403 Spotify failures without claiming an authorisation problem", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      playing: false,
      error: "rate_limited",
      message: "Spotify is rate-limiting requests.",
    });

    expect(np.error).toBe("rate_limited");
    expect(np.track).toBe("Spotify unavailable");
  });

  it("still shows an honest idle state when nothing is playing", async () => {
    const np = await loadWithNowPlaying({ connected: true, playing: false });

    expect(np.error).toBeUndefined();
    expect(np.track).toBe("Nothing playing");
    expect(np.connected).toBe(true);
  });

  it("keeps real playback untouched", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      playing: true,
      track: "Blinding Lights",
      artist: "The Weeknd",
      progress_ms: 60_000,
      duration_ms: 200_000,
    });

    expect(np.error).toBeUndefined();
    expect(np.track).toBe("Blinding Lights");
    expect(np.artist).toBe("The Weeknd");
    expect(np.playing).toBe(true);
    expect(np.elapsed).toBeCloseTo(0.3);
  });

  it("derives scenes from schedules", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "lr1", name: "Lamp", device_type: "light", is_online: true, room: "Living Room" },
    ]);
    apiMock.listSchedules.mockResolvedValue([
      { id: "s1", name: "morning_routine", label: "Wake Up", cron: "0 7 * * *", prompt: "", enabled: true },
      { id: "s2", name: "bedtime",         label: "Bedtime", cron: "0 22 * * *", prompt: "", enabled: true },
    ]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.scenes.length).toBe(2);
    expect(home.scenes[0].name).toBe("Wake Up");
    expect(home.scenes[1].name).toBe("Bedtime");
  });
});

describe("now-playing polling", () => {
  beforeEach(() => {
    __resetHubDataForTests();
    apiMock.getNowPlaying.mockReset();
  });

  /// The widget polls every ten seconds and the dashboard is left open for
  /// days. An answer that cannot change without somebody doing something costs
  /// ~8,600 requests a day, each one a round trip the server makes to Spotify.
  it("backs off on an answer only a person can change", async () => {
    for (const [response, reason] of [
      [{ connected: true, error: "unauthorized" }, "unauthorized"],
      [{ connected: true, error: "forbidden" }, "forbidden"],
      [{ connected: false, error: "network_refused" }, "network_refused"],
      [{ connected: false }, "not_connected"],
    ] as const) {
      __resetHubDataForTests();
      apiMock.getNowPlaying.mockResolvedValue(response);
      await refreshNowPlaying();
      expect(__nowPlayingBackoffForTests()).toBe(reason);
    }
  });

  /// Spotify's own words: rate limiting "should reappear shortly", and
  /// `unavailable` is whatever it was doing at the time. Both clear themselves,
  /// so giving up on them would strand a widget that was about to recover.
  it("stays at full rate through failures that clear themselves", async () => {
    for (const response of [
      { connected: true, error: "rate_limited" },
      { connected: true, error: "unavailable" },
      { connected: true, playing: false },
      { connected: true, playing: true, track: "Blue Train", artist: "John Coltrane" },
    ]) {
      __resetHubDataForTests();
      apiMock.getNowPlaying.mockResolvedValue(response);
      await refreshNowPlaying();
      expect(__nowPlayingBackoffForTests()).toBeNull();
    }
  });

  /// A throw is the server being unreachable, not Spotify refusing. Halting
  /// here would leave the widget dead until the app was relaunched.
  it("does not back off when the server itself is unreachable", async () => {
    apiMock.getNowPlaying.mockRejectedValue(new Error("ECONNREFUSED"));
    await refreshNowPlaying();
    expect(__nowPlayingBackoffForTests()).toBeNull();
  });

  /// The halt gates the timer, not the function. Coming back to the dashboard,
  /// refreshing, or pressing a control all route through here — which is what
  /// makes the widget recover instead of staying stopped forever.
  it("recovers when asked directly after the problem is fixed", async () => {
    apiMock.getNowPlaying.mockResolvedValue({ connected: true, error: "unauthorized" });
    await refreshNowPlaying();
    expect(__nowPlayingBackoffForTests()).toBe("unauthorized");

    apiMock.getNowPlaying.mockResolvedValue({ connected: true, playing: true, track: "Blue Train" });
    await refreshNowPlaying();

    expect(__nowPlayingBackoffForTests()).toBeNull();
    expect(getHomeData().nowPlaying.track).toBe("Blue Train");
  });
});

describe("now-playing backoff cadence", () => {
  beforeEach(() => {
    __resetHubDataForTests();
    apiMock.getNowPlaying.mockReset();
  });

  it("asks on every tick while everything is healthy", () => {
    for (let i = 0; i < 5; i++) expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  /// The whole point of backing off rather than stopping: nobody has to press
  /// anything for a fixed Spotify to be noticed. A hard stop was unrecoverable
  /// in practice — a full reload only happens on app start or server reconnect,
  /// visibilitychange is unreliable in a desktop webview, and the transport
  /// controls are disabled in exactly the state that would need them.
  it("skips most ticks while backed off, but always comes back", async () => {
    apiMock.getNowPlaying.mockResolvedValue({ connected: true, error: "unauthorized" });
    await refreshNowPlaying();
    expect(__nowPlayingBackoffForTests()).toBe("unauthorized");

    // Two full cycles, so this cannot pass by retrying once and giving up.
    for (let cycle = 0; cycle < 2; cycle++) {
      for (let i = 0; i < __BACKOFF_TICKS_FOR_TESTS - 1; i++) {
        expect(__tickNowPlayingPollForTests()).toBe(false);
      }
      expect(__tickNowPlayingPollForTests()).toBe(true);
    }
  });

  it("returns to full rate the moment the answer changes", async () => {
    apiMock.getNowPlaying.mockResolvedValue({ connected: false });
    await refreshNowPlaying();
    expect(__tickNowPlayingPollForTests()).toBe(false);

    apiMock.getNowPlaying.mockResolvedValue({ connected: true, playing: true, track: "Blue Train" });
    await refreshNowPlaying();

    expect(__nowPlayingBackoffForTests()).toBeNull();
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  /// Recovering and failing again must wait the full interval, not fire
  /// immediately on a counter left part-way through the previous outage.
  it("restarts the interval rather than resuming a half-spent one", async () => {
    apiMock.getNowPlaying.mockResolvedValue({ connected: false });
    await refreshNowPlaying();
    for (let i = 0; i < 10; i++) __tickNowPlayingPollForTests();

    apiMock.getNowPlaying.mockResolvedValue({ connected: true, playing: false });
    await refreshNowPlaying();
    apiMock.getNowPlaying.mockResolvedValue({ connected: false });
    await refreshNowPlaying();

    for (let i = 0; i < __BACKOFF_TICKS_FOR_TESTS - 1; i++) {
      expect(__tickNowPlayingPollForTests()).toBe(false);
    }
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });
});
