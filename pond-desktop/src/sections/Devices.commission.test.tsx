import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Devices } from "./Devices";
import { api } from "../api/PondApiClient";
import { ApiError, type MatterStatus } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listDevices: vi.fn(),
    registerDevice: vi.fn(),
    unregisterDevice: vi.fn(),
    commissionDevice: vi.fn(),
    getMatterStatus: vi.fn(),
    updateSettings: vi.fn(),
    invokeTool: vi.fn(),
  },
}));

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

/** A Matter runtime in the given state, as `GET /matter/status` reports it. */
function matterStatus(state: MatterStatus["state"], error?: string): MatterStatus {
  return { enabled: state !== "disabled", url: "ws://127.0.0.1:5580/ws", state, error };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocked(api.listDevices).mockResolvedValue([]);
  // Commissioning is only offered against a live controller, so that is the
  // baseline; the tests that care about the other states set them explicitly.
  mocked(api.getMatterStatus).mockResolvedValue(matterStatus("connected"));
  mocked(api.updateSettings).mockResolvedValue({});
});
afterEach(() => cleanup());

/** Open the add-device modal, once the Matter state has settled. */
async function openModal() {
  render(<Devices />);
  // The empty state and the header both offer it; take the header button.
  const buttons = await screen.findAllByText("Register device");
  await waitFor(() => expect(api.getMatterStatus).toHaveBeenCalled());
  fireEvent.click(buttons[0]);
}

describe("Add device — Matter vs other", () => {
  it("asks for a setup code, not a description, when adding a Matter device", async () => {
    await openModal();

    // Matter is the default: a setup code is what a Matter device needs.
    expect(await screen.findByText("Setup code")).toBeTruthy();
    // The manual-entry fields must not be asked for — a commissioned device
    // reports its own name and type.
    expect(screen.queryByText("Device type")).toBeNull();
    expect(screen.queryByText("Hostname (optional)")).toBeNull();
  });

  it("commissions with the entered code and refreshes the list", async () => {
    mocked(api.commissionDevice).mockResolvedValue({
      id: "matter-2",
      name: "Virtual OnOff Light",
      node_id: 2,
    });
    await openModal();

    const input = await screen.findByPlaceholderText(/20202021/);
    // The Matter Virtual Device's passcode, exactly as its app shows it.
    fireEvent.change(input, { target: { value: "20202021" } });
    fireEvent.click(screen.getByText("Commission"));

    // No name entered → code only, name undefined.
    await waitFor(() => expect(api.commissionDevice).toHaveBeenCalledWith("20202021", undefined));
    // Never a manual registry write: the bridge registers what it commissions.
    expect(api.registerDevice).not.toHaveBeenCalled();
    // List reloads so the new device shows up.
    await waitFor(() => expect(mocked(api.listDevices).mock.calls.length).toBeGreaterThan(1));
  });

  it("passes a chosen name through so the device is named on commission", async () => {
    mocked(api.commissionDevice).mockResolvedValue({
      id: "matter-2",
      name: "Living Room Light",
      node_id: 2,
    });
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    // The optional name field, alongside the code — not a separate mode.
    fireEvent.change(screen.getByPlaceholderText("Living Room Light"), {
      target: { value: "Living Room Light" },
    });
    fireEvent.click(screen.getByText("Commission"));

    await waitFor(() =>
      expect(api.commissionDevice).toHaveBeenCalledWith("20202021", "Living Room Light"),
    );
  });

  it("surfaces a commissioning failure instead of closing silently", async () => {
    mocked(api.commissionDevice).mockRejectedValue(new Error("device not found on the network"));
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    expect(await screen.findByText(/device not found on the network/)).toBeTruthy();
    // Modal stays open so the code can be corrected.
    expect(screen.getByText("Setup code")).toBeTruthy();
  });

  it("switching to 'Other device' restores the manual fields and registers", async () => {
    mocked(api.registerDevice).mockResolvedValue({ id: "d1", name: "Pi" });
    await openModal();

    fireEvent.click(screen.getByText("Other device"));

    // Manual entry is back, and the setup code is gone.
    expect(await screen.findByText("Device type")).toBeTruthy();
    expect(screen.queryByText("Setup code")).toBeNull();

    fireEvent.change(screen.getByPlaceholderText("Living Room Pi"), {
      target: { value: "Pi" },
    });
    fireEvent.click(screen.getByText("Register"));

    await waitFor(() => expect(api.registerDevice).toHaveBeenCalled());
    // A non-Matter device is never commissioned.
    expect(api.commissionDevice).not.toHaveBeenCalled();
  });
});

describe("Matter section — turning the fabric on", () => {
  it("shows the runtime's state, not just the saved setting", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(matterStatus("connecting"));
    render(<Devices />);

    // "Saved and enabled" is not the same fact as "the controller is up".
    expect((await screen.findByTestId("matter-state")).textContent).toBe("Starting…");
  });

  it("carries the controller address as a value, not just a placeholder", async () => {
    render(<Devices />);
    const input = (await screen.findByLabelText("Controller address")) as HTMLInputElement;

    // The placeholder is the same string as the default address, so anything
    // that asserts on displayed text passes even when nothing is set. The value
    // is the only thing separating "configured" from "blank".
    await waitFor(() => expect(input.value).toBe("ws://127.0.0.1:5580/ws"));
    expect(input.placeholder).toBe(input.value);
  });

  it("leaves the field visibly empty when the runtime reports no address", async () => {
    mocked(api.getMatterStatus).mockResolvedValue({
      enabled: false,
      url: "",
      state: "disabled",
    });
    render(<Devices />);
    const input = (await screen.findByLabelText("Controller address")) as HTMLInputElement;

    // Supplying the default is the server's job (an empty stored value resolves
    // to it there). The UI must not paper over a genuinely empty address, or
    // the user is back to a filled-looking field that fails to save.
    await waitFor(() => expect(input.value).toBe(""));
  });

  it("saves the toggle and re-reads what actually happened", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(matterStatus("disabled"));
    render(<Devices />);
    await waitFor(() => expect(api.getMatterStatus).toHaveBeenCalled());

    fireEvent.click(screen.getByLabelText("Enable Matter"));

    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({
        matter_enabled: true,
        matter_ws_url: "ws://127.0.0.1:5580/ws",
      }),
    );
    // The runtime is asked again rather than the UI assuming the save worked.
    await waitFor(() => expect(mocked(api.getMatterStatus).mock.calls.length).toBeGreaterThan(1));
  });

  it("blocks commissioning while Matter is off, and says where to turn it on", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(matterStatus("disabled"));
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    // The old build let this through and failed on submit with advice that
    // pointed at a Settings control which did not exist.
    expect(screen.getByText("Commission").closest("button")?.disabled).toBe(true);
    expect(screen.getByTestId("matter-not-ready").textContent).toMatch(/Matter is off/);
  });

  it("reports an unreachable controller as its own problem, and retries in place", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(
      matterStatus("unreachable", "connection refused"),
    );
    render(<Devices />);

    expect((await screen.findByTestId("matter-state")).textContent).toBe(
      "Cannot reach controller",
    );
    expect(screen.getByText(/connection refused/)).toBeTruthy();

    // Retry is the same save: the runtime reconnects because it compares
    // against what is running, not against the request.
    fireEvent.click(screen.getByText("Retry"));
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({
        matter_enabled: true,
        matter_ws_url: "ws://127.0.0.1:5580/ws",
      }),
    );
  });

  it("shows the server's message without the ApiError class name", async () => {
    mocked(api.commissionDevice).mockRejectedValue(
      new ApiError(503, "Matter is off on this Pond."),
    );
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    const message = await screen.findByText(/Matter is off on this Pond/);
    expect(message.textContent).toBe("Matter is off on this Pond.");
    expect(message.textContent).not.toMatch(/ApiError/);
  });
});
