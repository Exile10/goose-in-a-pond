import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    listDevices: vi.fn(),
    listSchedules: vi.fn(),
  },
}));

import { api } from "../../api/PondApiClient";
import { getHomeData, refreshHomeData, __resetHubDataForTests } from "./hubDataStore";

const apiMock = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  listDevices: ReturnType<typeof vi.fn>;
  listSchedules: ReturnType<typeof vi.fn>;
};

describe("hubDataStore", () => {
  beforeEach(() => {
    __resetHubDataForTests();
    apiMock.getSettings.mockReset();
    apiMock.listDevices.mockReset();
    apiMock.listSchedules.mockReset();
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
