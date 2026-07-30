import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Devices, formatUptime } from "./Devices";
import { api } from "../api/PondApiClient";
import type { MatterControllerStatus } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listDevices: vi.fn(),
    registerDevice: vi.fn(),
    unregisterDevice: vi.fn(),
    commissionDevice: vi.fn(),
    invokeTool: vi.fn(),
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    getMatterController: vi.fn(),
    restartMatterController: vi.fn(),
  },
}));

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

/** A controller left behind by an earlier Pond, up for 27 hours. */
const stale: MatterControllerStatus = {
  origin: "adopted_from_earlier_run",
  description: "left running by an earlier Pond and adopted",
  pid: 4242,
  uptime_secs: 97_200,
  restartable: true,
};

/** Someone else's controller: reported, never touched. */
const external: MatterControllerStatus = {
  origin: "external",
  description: "not managed by this Pond",
  pid: null,
  uptime_secs: null,
  restartable: false,
};

beforeEach(() => {
  vi.clearAllMocks();
  mocked(api.listDevices).mockResolvedValue([]);
  // Matter on, so the panel asks about the controller behind it.
  mocked(api.getSettings).mockResolvedValue({
    matter_enabled: true,
    matter_ws_url: "ws://127.0.0.1:5580/ws",
  });
  mocked(api.updateSettings).mockResolvedValue({});
  mocked(api.getMatterController).mockResolvedValue(stale);
  mocked(api.restartMatterController).mockResolvedValue({
    ...stale,
    origin: "started_by_pond",
    description: "started by this Pond",
    uptime_secs: 2,
  });
});
afterEach(() => cleanup());

async function renderDevices() {
  render(<Devices />);
  await waitFor(() => {
    if (!screen.queryByText("Enable Matter")) throw new Error("panel not rendered");
  });
}

// GIAP adopts whatever is listening on the controller port. That is deliberate,
// but it means a controller from an earlier run can serve its port for days while
// its mDNS state is dead — every pairing fails with a discovery timeout, and
// restarting the Pond re-adopts the very same process. These cover the two things
// that make it recoverable: seeing the controller's age, and restarting it.
describe("Matter controller status on the Devices screen", () => {
  it("shows where the controller came from and how long it has been up", async () => {
    await renderDevices();

    await waitFor(() => {
      if (!screen.queryByText(/left running by an earlier Pond/)) throw new Error("no status");
    });
    // The age is the clue: 97200s is 27 hours, shown as a day and change.
    expect(screen.getByText(/up 1d 3h/)).toBeTruthy();
  });

  it("offers a Restart for a controller the Pond owns", async () => {
    await renderDevices();
    await waitFor(() => {
      if (!screen.queryByText("Restart")) throw new Error("no restart button");
    });
    expect((screen.getByText("Restart").closest("button") as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  it("restarts on click and shows the fresh controller", async () => {
    await renderDevices();
    fireEvent.click(await screen.findByText("Restart"));

    await waitFor(() => expect(api.restartMatterController).toHaveBeenCalled());
    // The reply is rendered, so the stale age is replaced without a reload.
    await waitFor(() => {
      if (!screen.queryByText(/started by this Pond/)) throw new Error("status not refreshed");
    });
    expect(screen.queryByText(/up 1d 3h/)).toBeNull();
  });

  it("will not offer to restart a controller the Pond does not own", async () => {
    mocked(api.getMatterController).mockResolvedValue(external);
    await renderDevices();

    await waitFor(() => {
      if (!screen.queryByText(/not managed by this Pond/)) throw new Error("no status");
    });
    // Present but refused, so the reason is visible rather than the action hidden.
    expect((screen.getByText("Restart").closest("button") as HTMLButtonElement).disabled).toBe(
      true,
    );
    fireEvent.click(screen.getByText("Restart"));
    expect(api.restartMatterController).not.toHaveBeenCalled();
  });

  it("surfaces a failed restart instead of pretending it worked", async () => {
    mocked(api.restartMatterController).mockRejectedValue(
      new Error("the old controller is still holding port 5580"),
    );
    await renderDevices();

    fireEvent.click(await screen.findByText("Restart"));

    expect(await screen.findByText(/still holding port 5580/)).toBeTruthy();
  });

  it("says nothing about a controller when Matter is off", async () => {
    mocked(api.getSettings).mockResolvedValue({ matter_enabled: false });
    await renderDevices();

    // No controller exists to report, so the row is absent rather than empty —
    // and the status is never requested.
    expect(screen.queryByText("Controller process")).toBeNull();
    expect(api.getMatterController).not.toHaveBeenCalled();
  });

  it("keeps the settings controls usable when the status cannot be fetched", async () => {
    mocked(api.getMatterController).mockRejectedValue(new Error("boom"));
    await renderDevices();

    // The status is a diagnostic extra; losing it must not break the toggle.
    expect(screen.getByText("Enable Matter")).toBeTruthy();
    expect(screen.queryByText("Controller process")).toBeNull();
    expect(screen.queryByText(/boom/)).toBeNull();
  });
});

describe("formatUptime", () => {
  it("reads as a duration at every scale a controller reaches", () => {
    // Rounded up, so a just-restarted controller never reads as "0".
    expect(formatUptime(0)).toBe("1 min");
    expect(formatUptime(59)).toBe("1 min");
    expect(formatUptime(60)).toBe("1 min");
    expect(formatUptime(90 * 60)).toBe("1h");
    expect(formatUptime(23 * 3600)).toBe("23h");
    expect(formatUptime(24 * 3600)).toBe("1d");
    // The case that prompted all this: over a day, with the remainder kept.
    expect(formatUptime(97_200)).toBe("1d 3h");
  });
});
