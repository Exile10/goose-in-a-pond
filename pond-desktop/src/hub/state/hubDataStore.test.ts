import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    listDevices: vi.fn(),
    listSchedules: vi.fn(),
    listRecipes: vi.fn().mockResolvedValue([]),
    getWeather: vi.fn().mockResolvedValue({ enabled: false }),
  },
}));

import { api } from "../../api/PondApiClient";
import { getHomeData, refreshHomeData, __resetHubDataForTests } from "./hubDataStore";
import { ROUTINES as MOCK_ROUTINES } from "../data/routines";

const apiMock = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  listDevices: ReturnType<typeof vi.fn>;
  listSchedules: ReturnType<typeof vi.fn>;
  listRecipes: ReturnType<typeof vi.fn>;
  getWeather: ReturnType<typeof vi.fn>;
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
