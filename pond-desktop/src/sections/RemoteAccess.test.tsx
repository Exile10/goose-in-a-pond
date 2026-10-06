import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup } from "@testing-library/react";
import "../i18n";
import { RemoteAccess } from "./RemoteAccess";
import { api } from "../api/PondApiClient";

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
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: true, serial });
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText(new RegExp(serial))).toBeTruthy());
    expect(screen.queryByPlaceholderText("giap-inv1-...")).toBeNull();
  });

  it("offers the invite field to a Pond nobody provisioned", async () => {
    vi.mocked(api.remoteDevice).mockResolvedValue({ provisioned: false });
    render(<RemoteAccess />);
    await waitFor(() => expect(api.remoteDevice).toHaveBeenCalled());
    expect(screen.getByPlaceholderText("giap-inv1-...")).toBeTruthy();
  });

  it("keeps the invite field when provisioning cannot be read", async () => {
    vi.mocked(api.remoteDevice).mockRejectedValue(new Error("unavailable"));
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    render(<RemoteAccess />);
    await waitFor(() => expect(warn).toHaveBeenCalled());
    expect(screen.getByPlaceholderText("giap-inv1-...")).toBeTruthy();
    warn.mockRestore();
  });
});
