import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup } from "@testing-library/react";
import { Devices } from "./Devices";
import { api } from "../api/PondApiClient";
import type { Device } from "../api/types";

// ── Mock PondApiClient ────────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    listDevices: vi.fn(),
    registerDevice: vi.fn(),
    unregisterDevice: vi.fn(),
    invokeTool: vi.fn(),
  },
}));

// A commissioned Matter light, as the bridge registers it: stable `matter-<id>`
// id, a cluster-inferred `device_type`, online.
const matterLight: Device = {
  id: "matter-2",
  name: "Living Room Light",
  device_type: "light",
  is_online: true,
  last_seen: new Date().toISOString(),
};

const matterLock: Device = {
  id: "matter-9",
  name: "Front Door",
  device_type: "lock",
  is_online: true,
};

beforeEach(() => {
  vi.clearAllMocks();
});
afterEach(() => cleanup());

describe("Devices section — Matter devices", () => {
  it("shows a commissioned Matter device in the list", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([matterLight]);

    render(<Devices />);

    // The device appears by name, and its cluster-inferred type is shown.
    expect(await screen.findByText("Living Room Light")).toBeTruthy();
    expect(screen.getByText("light")).toBeTruthy();
  });

  it("renders type-appropriate icons for Matter device types", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      matterLight,
      matterLock,
    ]);

    const { container } = render(<Devices />);
    await screen.findByText("Living Room Light");

    // lucide-react renders `<svg class="lucide lucide-<name>">`, so a light gets
    // the bulb and a lock gets the lock — not the generic monitor fallback.
    expect(container.querySelector(".lucide-lightbulb")).toBeTruthy();
    expect(container.querySelector(".lucide-lock")).toBeTruthy();
  });
});
