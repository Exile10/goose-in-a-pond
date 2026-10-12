import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import { RemoteAccess } from "./RemoteAccess";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    remoteStatus: vi.fn(),
    remoteRecoveryRequests: vi.fn(),
    remoteDevice: vi.fn(),
    prepareRemoteIdentity: vi.fn(),
    enableRemoteAccess: vi.fn(),
    registerRemotePond: vi.fn(),
    disableRemoteAccess: vi.fn(),
    approveRemoteRecovery: vi.fn(),
  },
}));

describe("RemoteAccess admission", () => {
  beforeEach(() => {
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Stopped" });
    vi.mocked(api.remoteRecoveryRequests).mockResolvedValue([]);
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("asks a provisioned Pond for no invite and shows the serial to revoke it by", async () => {
    const serial = "0f".repeat(16);
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: true, serial, registered: false });
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText(new RegExp(serial))).toBeTruthy());
    expect(screen.queryByPlaceholderText("giap-inv1-...")).toBeNull();
  });

  it("asks a household already registered for no invite", async () => {
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: false, registered: true });
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText(/already registered/)).toBeTruthy());
    expect(screen.queryByPlaceholderText("giap-inv1-...")).toBeNull();
  });

  it("offers the invite field to a Pond nobody provisioned that never registered", async () => {
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: false, registered: false });
    render(<RemoteAccess />);
    await waitFor(() => expect(api.remoteDevice).toHaveBeenCalled());
    expect(screen.getByPlaceholderText("giap-inv1-...")).toBeTruthy();
  });

  it("shows phone recovery only when there is a request to approve", async () => {
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: false, registered: true });
    const first = render(<RemoteAccess />);
    await waitFor(() => expect(api.remoteRecoveryRequests).toHaveBeenCalled());
    expect(screen.queryByText("Phone recovery requests")).toBeNull();
    first.unmount();

    vi.mocked(api.remoteRecoveryRequests).mockResolvedValue([
      { id: "a1b2c3d4", device: "d", approved: false, deviceName: "A57", keyPreview: "0123456789abcdef" },
    ]);
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText("Phone recovery requests")).toBeTruthy());
  });

  it("says so when recovery requests cannot be read", async () => {
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: false, registered: true });
    vi.mocked(api.remoteRecoveryRequests).mockRejectedValue(new Error("unavailable"));
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());
    expect(screen.getByText("Phone recovery requests")).toBeTruthy();
  });

  it("keeps the invite field when provisioning cannot be read", async () => {
    vi.mocked(api.remoteDevice).mockRejectedValue(new Error("unavailable"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    render(<RemoteAccess />);
    await waitFor(() => expect(warn).toHaveBeenCalled());
    expect(screen.getByPlaceholderText("giap-inv1-...")).toBeTruthy();
    warn.mockRestore();
  });

  it("shows the phone name and key preview for the household to compare", async () => {
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: false, registered: true });
    vi.mocked(api.remoteRecoveryRequests).mockResolvedValue([
      { id: "e5f6a7b8", device: "d", approved: false, deviceName: "Kitchen phone", keyPreview: "nodekey:4c1e" },
    ]);
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText("Phone: Kitchen phone")).toBeTruthy());
    expect(screen.getByText("Key begins: nodekey:4c1e")).toBeTruthy();
  });
});

describe("RemoteAccess after the coordinator answers", () => {
  const invite = () => screen.queryByPlaceholderText("giap-inv1-...");

  beforeEach(() => {
    // Not running yet, and the coordinator wants this Pond registered.
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Stopped", authUrl: "https://control.example/register" });
    vi.mocked(api.remoteRecoveryRequests).mockResolvedValue([]);
    vi.mocked(api.enableRemoteAccess).mockResolvedValue(undefined);
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: true, serial: "0f".repeat(16), registered: false });
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("keeps the invite field when an invite typo follows a refused certificate", async () => {
    vi.mocked(api.registerRemotePond)
      .mockRejectedValueOnce(new ApiError(403, "device_used"))
      .mockRejectedValueOnce(new ApiError(403, "invite_invalid"));
    const user = userEvent.setup();
    render(<RemoteAccess />);
    await waitFor(() => expect(api.remoteDevice).toHaveBeenCalled());
    expect(invite()).toBeNull();

    await user.click(screen.getByRole("switch"));
    await waitFor(() => expect(screen.getByText(/already admitted another household/)).toBeTruthy());
    await user.type(invite()!, "giap-inv1-TYPO");
    await user.click(screen.getByRole("switch"));

    await waitFor(() => expect(screen.getByText(/invite was not recognised/)).toBeTruthy());
    expect(api.registerRemotePond).toHaveBeenLastCalledWith("giap-inv1-TYPO");
    expect(invite()).toBeTruthy();
  });

  it("asks a provisioned Pond for an invite when the coordinator requires one", async () => {
    vi.mocked(api.registerRemotePond).mockRejectedValue(new ApiError(403, "invite_required"));
    const user = userEvent.setup();
    render(<RemoteAccess />);
    await waitFor(() => expect(api.remoteDevice).toHaveBeenCalled());

    await user.click(screen.getByRole("switch"));

    await waitFor(() => expect(screen.getByText(/admits new households by invite/)).toBeTruthy());
    expect(invite()).toBeTruthy();
  });

  it("reads the device status again after disabling", async () => {
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Running" });
    vi.mocked(api.disableRemoteAccess).mockResolvedValue(undefined);
    vi.mocked(api.remoteDevice)
      .mockResolvedValueOnce({ provisioned: false, registered: false })
      .mockResolvedValueOnce({ provisioned: false, registered: true });
    const user = userEvent.setup();
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText(/Remote access is enabled/)).toBeTruthy());
    expect(invite()).toBeTruthy();

    await user.click(screen.getByRole("switch"));

    await waitFor(() => expect(screen.getByText(/already registered/)).toBeTruthy());
    expect(api.remoteDevice).toHaveBeenCalledTimes(2);
    expect(invite()).toBeNull();
  });
});
