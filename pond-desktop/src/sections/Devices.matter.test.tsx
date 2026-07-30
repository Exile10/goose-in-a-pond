import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Devices } from "./Devices";
import { api } from "../api/PondApiClient";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listDevices: vi.fn(),
    registerDevice: vi.fn(),
    unregisterDevice: vi.fn(),
    commissionDevice: vi.fn(),
    invokeTool: vi.fn(),
    getSettings: vi.fn(),
    getMatterController: vi.fn(),
    restartMatterController: vi.fn(),
    updateSettings: vi.fn(),
  },
}));

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  mocked(api.listDevices).mockResolvedValue([]);
  mocked(api.getSettings).mockResolvedValue({});
  mocked(api.updateSettings).mockResolvedValue({});
  mocked(api.getMatterController).mockRejectedValue(new Error("not configured"));
});
afterEach(() => cleanup());

/** Render the screen and wait for the Matter panel to appear. */
async function renderDevices() {
  render(<Devices />);
  await waitFor(() => {
    if (!screen.queryByText("Enable Matter")) throw new Error("panel not rendered");
  });
}

/** The switch inside the row carrying `label` — Row does not link the two. */
function controlIn(label: string): HTMLInputElement {
  const row = screen.getByText(label).closest(".row") as HTMLElement;
  return row.querySelector("input") as HTMLInputElement;
}

// The control lives on this screen rather than under Settings for one reason:
// this is where the failure happens. Commissioning refuses a setup code when no
// controller is connected, and the error names "Devices > Matter" — so the panel
// has to be on the same screen as "Register device", or the message lies. It
// pointed at a Settings control that did not exist before, and then at one
// buried under a row named for MCP servers, next to an unrelated sidebar item
// of the same name.
describe("Matter panel on the Devices screen", () => {
  it("is on the same screen as Register device, where the error points", async () => {
    await renderDevices();

    expect(screen.getByText("Matter")).toBeTruthy();
    expect(screen.getByText("Enable Matter")).toBeTruthy();
    expect(screen.getByPlaceholderText("ws://127.0.0.1:5580/ws")).toBeTruthy();
    // Both on one screen: the knob and the button that needs it.
    expect(screen.getAllByText("Register device").length).toBeGreaterThan(0);
  });

  it("shows what is already stored rather than an empty box", async () => {
    mocked(api.getSettings).mockResolvedValue({
      matter_enabled: true,
      matter_ws_url: "ws://10.0.0.7:5580/ws",
    });
    await renderDevices();

    expect(controlIn("Enable Matter").checked).toBe(true);
    const url = screen.getByDisplayValue("ws://10.0.0.7:5580/ws") as HTMLInputElement;
    expect(url.disabled).toBe(false);
  });

  it("keeps the address locked until Matter is switched on", async () => {
    await renderDevices();
    // Nothing stored, so Matter is off and its address cannot be edited yet.
    expect((screen.getByPlaceholderText("ws://127.0.0.1:5580/ws") as HTMLInputElement).disabled)
      .toBe(true);
  });

  it("saves only the key that changed when switched on", async () => {
    await renderDevices();

    fireEvent.click(controlIn("Enable Matter"));

    // Exactly one key: PUT /api/v1/settings records every key it carries as
    // deliberately chosen, so a wider patch would mark settings nobody touched.
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ matter_enabled: true }));
    expect(mocked(api.updateSettings).mock.calls.length).toBe(1);
  });

  it("saves the address once, on blur, not on every keystroke", async () => {
    mocked(api.getSettings).mockResolvedValue({ matter_enabled: true, matter_ws_url: "" });
    await renderDevices();

    const url = screen.getByPlaceholderText("ws://127.0.0.1:5580/ws");
    fireEvent.change(url, { target: { value: "ws://10.0.0.7:5580/ws" } });
    // Still nothing sent — typing is not a decision.
    expect(api.updateSettings).not.toHaveBeenCalled();

    fireEvent.blur(url);
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({ matter_ws_url: "ws://10.0.0.7:5580/ws" }),
    );
    expect(mocked(api.updateSettings).mock.calls.length).toBe(1);
  });

  it("does not write the address back when it was not edited", async () => {
    mocked(api.getSettings).mockResolvedValue({
      matter_enabled: true,
      matter_ws_url: "ws://127.0.0.1:5580/ws",
    });
    await renderDevices();

    // Focusing and leaving is not an edit, so it must not mark the key as chosen.
    fireEvent.blur(screen.getByDisplayValue("ws://127.0.0.1:5580/ws"));
    expect(api.updateSettings).not.toHaveBeenCalled();
  });

  it("surfaces a failed save instead of showing a value that was not stored", async () => {
    mocked(api.updateSettings).mockRejectedValue(new Error("settings write failed"));
    await renderDevices();

    fireEvent.click(controlIn("Enable Matter"));

    expect(await screen.findByText(/settings write failed/)).toBeTruthy();
  });

  it("does not break the device list it sits above", async () => {
    mocked(api.listDevices).mockResolvedValue([
      { id: "matter-2", name: "Living Room Light", device_type: "light", is_online: true },
    ]);
    await renderDevices();

    expect(await screen.findByText("Living Room Light")).toBeTruthy();
  });
});
