import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import "../i18n";
import { ApiError } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    remoteStatus: vi.fn(),
    remoteRecoveryRequests: vi.fn(),
    // Not called by every version of the page; mocked anyway, because an unmocked call
    // throws during render and fails this test for a reason that has nothing to do with it.
    remoteDevice: vi.fn(async () => ({ provisioned: false, registered: true })),
  },
}));

import { api } from "../api/PondApiClient";
import { RemoteAccess } from "./RemoteAccess";

describe("RemoteAccess once this tab is signed out", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("says so and stops asking for recovery requests", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Running" });
    vi.mocked(api.remoteRecoveryRequests).mockRejectedValue(new ApiError(403, "host_credential_required"));
    render(<RemoteAccess />);
    await waitFor(() => expect(screen.getByText(/no longer signed in/)).toBeTruthy());
    await vi.advanceTimersByTimeAsync(30_000);
    // Polled every five seconds before; each refusal was logged by the Pond.
    expect(api.remoteRecoveryRequests).toHaveBeenCalledTimes(1);
  });

  it("keeps asking after any other failure", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Running" });
    vi.mocked(api.remoteRecoveryRequests).mockRejectedValue(new Error("unreachable"));
    render(<RemoteAccess />);
    await waitFor(() => expect(api.remoteRecoveryRequests).toHaveBeenCalled());
    await vi.advanceTimersByTimeAsync(11_000);
    expect(vi.mocked(api.remoteRecoveryRequests).mock.calls.length).toBeGreaterThanOrEqual(3);
  });
});
