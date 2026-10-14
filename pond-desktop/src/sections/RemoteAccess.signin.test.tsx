import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import "../i18n";
import { ApiError } from "../api/types";

const shell = vi.hoisted(() => ({ desktop: false }));
vi.mock("../shell", () => ({ isDesktopShell: () => shell.desktop }));

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
  beforeEach(() => {
    shell.desktop = false;
  });
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
    // Asking again would only be refused again, and the Pond logs each refusal.
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

describe("RemoteAccess in the desktop shell, which reads a fresh credential for every request", () => {
  const refused = () => new ApiError(403, "host_credential_required");
  const mismatch = /does not match the Pond/;
  // Inside `act`, so React has rendered what the timers' callbacks set before the next assertion.
  const tick = (ms: number) => act(() => vi.advanceTimersByTimeAsync(ms));

  beforeEach(() => {
    shell.desktop = true;
    vi.useFakeTimers();
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Running" });
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("asks again at the usual pace and keeps quiet for the first refusals", async () => {
    vi.mocked(api.remoteRecoveryRequests).mockRejectedValue(refused());
    render(<RemoteAccess />);
    await tick(0);
    expect(api.remoteRecoveryRequests).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(mismatch)).toBeNull();

    await tick(5_000);
    expect(api.remoteRecoveryRequests).toHaveBeenCalledTimes(2);
    expect(screen.queryByText(mismatch)).toBeNull();
    expect(screen.queryByText(/no longer signed in/)).toBeNull();
  });

  it("says its credential does not match once it keeps being refused", async () => {
    vi.mocked(api.remoteRecoveryRequests).mockRejectedValue(refused());
    render(<RemoteAccess />);
    await tick(10_000);
    expect(screen.getByText(mismatch)).toBeTruthy();
    expect(screen.queryByText(/pond-server dashboard/)).toBeNull();
  });

  it("asks less and less often while it stays refused, up to a minute", async () => {
    const asked: number[] = [];
    vi.mocked(api.remoteRecoveryRequests).mockImplementation(async () => {
      asked.push(Date.now());
      throw refused();
    });
    render(<RemoteAccess />);
    await tick(400_000);
    const gaps = asked.slice(1).map((at, i) => at - asked[i]!);
    expect(gaps.slice(0, 8)).toEqual([5_000, 5_000, 10_000, 20_000, 40_000, 60_000, 60_000, 60_000]);
  });

  it("clears the note and goes back to the usual pace once the Pond admits it", async () => {
    vi.mocked(api.remoteRecoveryRequests)
      .mockRejectedValueOnce(refused())
      .mockRejectedValueOnce(refused())
      .mockRejectedValueOnce(refused())
      .mockResolvedValue([]);
    render(<RemoteAccess />);
    await tick(10_000);
    expect(screen.getByText(mismatch)).toBeTruthy();

    // The next ask is 10 s on, and it is admitted.
    await tick(10_000);
    expect(screen.queryByText(mismatch)).toBeNull();
    const asked = vi.mocked(api.remoteRecoveryRequests).mock.calls.length;
    await tick(5_000);
    expect(vi.mocked(api.remoteRecoveryRequests).mock.calls.length).toBe(asked + 1);
  });

  it("keeps asking at the same pace after any other failure", async () => {
    vi.mocked(api.remoteRecoveryRequests).mockRejectedValue(new Error("unreachable"));
    render(<RemoteAccess />);
    await tick(25_000);
    expect(vi.mocked(api.remoteRecoveryRequests).mock.calls.length).toBe(6);
    expect(screen.queryByText(mismatch)).toBeNull();
  });
});
