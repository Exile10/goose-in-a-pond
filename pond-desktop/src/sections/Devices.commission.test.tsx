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

  it("shows the pairing-mode guidance verbatim, since it is the fix as well as the reason", async () => {
    // The server's wording for the commonest commissioning failure — a device
    // whose 15-minute pairing window has closed. It is actionable copy, so the
    // UI must not summarise or truncate it.
    const guidance =
      "No device found in pairing mode. Put the device into pairing mode and try again — " +
      "a Matter device stops accepting new connections about 15 minutes after it starts.";
    mocked(api.commissionDevice).mockRejectedValue(new Error(guidance));
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    // Matched in fragments rather than as one string: the copy contains an em
    // dash and wraps, so an exact-node match would be asserting the layout.
    expect(await screen.findByText(/No device found in pairing mode/)).toBeTruthy();
    expect(screen.getByText(/15 minutes after it starts/)).toBeTruthy();
    // The code stays put: the device is what needs attention, not the input.
    expect((screen.getByPlaceholderText(/20202021/) as HTMLInputElement).value).toBe("20202021");
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
