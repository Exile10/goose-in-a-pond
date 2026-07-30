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
    // The Matter panel reads these two settings when the screen mounts.
    getSettings: vi.fn().mockResolvedValue({}),
    getMatterController: vi.fn(),
    restartMatterController: vi.fn(),
    updateSettings: vi.fn().mockResolvedValue({}),
  },
}));

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  mocked(api.listDevices).mockResolvedValue([]);
});
afterEach(() => cleanup());

/** Open the add-device modal. */
async function openModal() {
  render(<Devices />);
  // The empty state and the header both offer it; take the header button.
  const buttons = await screen.findAllByText("Register device");
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

  it("shows the server's reason and remedy when Matter is unavailable", async () => {
    // The server distinguishes four causes (off / no address / controller down /
    // unsupported build). Whichever it sends must reach the user intact — the
    // old single message told people to flip a setting that had no control, and
    // said nothing about the restart the connection actually needs.
    mocked(api.commissionDevice).mockRejectedValue(
      new Error(
        "Cannot commission a device. Matter is on, but the controller at " +
          "ws://127.0.0.1:5580/ws did not answer when the Pond started. Check that it is " +
          "running and reachable, then restart the Pond.",
      ),
    );
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    // Asserted against the error element itself, not the screen: the Matter
    // panel on this same screen legitimately mentions the default address and
    // the restart too, so a screen-wide query would match either one.
    const shown = await screen.findByText(/did not answer when the Pond started/);
    // The specific cause, the address that failed, and the restart step.
    expect(shown.textContent).toContain("ws://127.0.0.1:5580/ws");
    expect(shown.textContent).toContain("restart the Pond");
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
