import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import "../i18n";
import { ApiError } from "../api/types";

const shell = vi.hoisted(() => ({ desktop: false }));
vi.mock("../shell", () => ({ isDesktopShell: () => shell.desktop }));

vi.mock("../api/PondApiClient", () => ({
  api: {
    getPairingCode: vi.fn(),
    issuePairingCode: vi.fn(),
    // Remote access sits on this page and asks for these as soon as it mounts.
    remoteStatus: vi.fn(async () => ({ state: "Stopped" })),
    remoteRecoveryRequests: vi.fn(async () => []),
    remoteDevice: vi.fn(async () => ({ provisioned: false, registered: true })),
  },
}));

import { api } from "../api/PondApiClient";
import { Pairing } from "./Pairing";

describe("Pairing when the Pond refuses the host credential", () => {
  beforeEach(() => {
    vi.mocked(api.getPairingCode).mockRejectedValue(new ApiError(403, "host_credential_required"));
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("sends a browser to a new sign-in link", async () => {
    shell.desktop = false;
    render(<Pairing />);
    expect(await screen.findByText(/no longer signed in/)).toBeTruthy();
    expect(screen.getByText(/pond-server dashboard/)).toBeTruthy();
  });

  it("tells the desktop shell its credential does not match, with no link to open", async () => {
    shell.desktop = true;
    render(<Pairing />);
    expect(await screen.findByText(/does not match the Pond/)).toBeTruthy();
    expect(screen.queryByText(/pond-server dashboard/)).toBeNull();
    expect(screen.queryByText(/no longer signed in/)).toBeNull();
  });

  it("shows any other failure as it is", async () => {
    shell.desktop = true;
    vi.mocked(api.getPairingCode).mockRejectedValue(new Error("connection refused"));
    render(<Pairing />);
    expect(await screen.findByText(/connection refused/)).toBeTruthy();
    expect(screen.queryByText(/does not match the Pond/)).toBeNull();
  });
});
