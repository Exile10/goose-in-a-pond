import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listSessions: vi.fn(),
  },
}));

import { PrivacyDetail } from "./Privacy";
import { api } from "../../../api/PondApiClient";

async function renderPrivacy(go: (route: string) => void = () => {}) {
  render(<PrivacyDetail go={go} />);
  await waitFor(() => expect(api.listSessions).toHaveBeenCalled());
  await screen.findByText("Bluetooth and pairing");
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listSessions).mockResolvedValue([] as never);
  vi.mocked(api.getSettings).mockResolvedValue({} as never);
  vi.mocked(api.updateSettings).mockImplementation(async (patch) => patch as never);
});

describe("Hub privacy: Bluetooth and pairing", () => {
  it("points to Connections instead of holding the two switches", async () => {
    await renderPrivacy();
    expect(screen.queryByRole("button", { name: "Say pairing codes aloud" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Set up over Bluetooth" })).toBeNull();
    expect(screen.getByText(/are under Connections/)).toBeTruthy();
  });

  it("opens Connections from the pointer", async () => {
    const go = vi.fn();
    await renderPrivacy(go);
    fireEvent.click(screen.getByRole("button", { name: "Open Connections" }));
    expect(go).toHaveBeenCalledWith("network");
  });
});
