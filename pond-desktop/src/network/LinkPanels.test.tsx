import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { ApiError } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getConnections: vi.fn(),
    getSetupStatus: vi.fn(),
    getSystemInfo: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { ConnectionsProvider } from "./ConnectionsProvider";
import { BluetoothStatus, EthernetPanel, PairingStatus } from "./LinkPanels";
import { SETUP_CLAIMED, SETUP_WAITING, makeConnections } from "./connectionsFixtures";

const getConnections = vi.mocked(api.getConnections);
const getSetupStatus = vi.mocked(api.getSetupStatus);

async function renderPanel(ui: React.ReactElement) {
  render(<ConnectionsProvider>{ui}</ConnectionsProvider>);
  await act(async () => {});
}

beforeEach(() => {
  vi.clearAllMocks();
  getConnections.mockResolvedValue(makeConnections());
  getSetupStatus.mockResolvedValue(SETUP_CLAIMED);
  vi.mocked(api.getSystemInfo).mockResolvedValue({ platform: "linux" } as never);
});
afterEach(cleanup);

describe("Ethernet", () => {
  it("lists each wired interface with its link, address and speed", async () => {
    getConnections.mockResolvedValue(
      makeConnections({
        ethernet: [
          { interface: "eth0", link: "up", address: "192.168.1.20", speed_mbps: 1000 },
          { interface: "eth1", link: "down", address: null, speed_mbps: null },
        ],
      }),
    );
    await renderPanel(<EthernetPanel variant="catalogue" />);
    const items = screen.getAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(within(items[0]).getByText("eth0")).toBeTruthy();
    expect(within(items[0]).getByText("Connected")).toBeTruthy();
    expect(within(items[0]).getByText("192.168.1.20")).toBeTruthy();
    expect(within(items[0]).getByText("1 Gbps")).toBeTruthy();
    expect(within(items[1]).getByText("eth1")).toBeTruthy();
    expect(within(items[1]).getByText("No link")).toBeTruthy();
  });

  it("says a cable that is up but has no address has none yet", async () => {
    getConnections.mockResolvedValue(
      makeConnections({ ethernet: [{ interface: "eth0", link: "up", address: null, speed_mbps: null }] }),
    );
    await renderPanel(<EthernetPanel variant="hub" />);
    expect(screen.getByText("No address yet")).toBeTruthy();
  });

  it("is a quiet invitation, not an error, when there is no wired connection", async () => {
    getConnections.mockResolvedValue(makeConnections({ ethernet: [] }));
    await renderPanel(<EthernetPanel variant="hub" />);
    expect(screen.getByText("No wired connection")).toBeTruthy();
    expect(screen.getByText(/Plug in a cable/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("offers nothing to change: it is read-only", async () => {
    await renderPanel(<EthernetPanel variant="catalogue" />);
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
  });
});

describe("Bluetooth status", () => {
  const bt = makeConnections().bluetooth;

  it("says which name phones see while advertising", async () => {
    await renderPanel(<BluetoothStatus variant="catalogue" />);
    expect(screen.getByText("Visible to phones")).toBeTruthy();
    expect(screen.getByText("Pond 4e81")).toBeTruthy();
    expect(screen.getByText(/Phones nearby can find this pond as/)).toBeTruthy();
  });

  it("says it is off, and why, when the switch is off", async () => {
    getConnections.mockResolvedValue(
      makeConnections({ bluetooth: { ...bt, enabled: false, state: "off", advertising_as: null } }),
    );
    await renderPanel(<BluetoothStatus variant="catalogue" />);
    expect(screen.getByText("Off")).toBeTruthy();
    expect(screen.getByText(/Setup over Bluetooth is switched off/)).toBeTruthy();
  });

  it("says it is serving, ready but not showing itself", async () => {
    getConnections.mockResolvedValue(makeConnections({ bluetooth: { ...bt, state: "serving", advertising_as: null } }));
    await renderPanel(<BluetoothStatus variant="hub" />);
    expect(screen.getByText("Ready")).toBeTruthy();
    expect(screen.getByText(/not showing itself/)).toBeTruthy();
  });

  it("gives the reason it is unavailable", async () => {
    getConnections.mockResolvedValue(
      makeConnections({ bluetooth: { ...bt, state: "unavailable", reason: "no adapter found", advertising_as: null } }),
    );
    await renderPanel(<BluetoothStatus variant="hub" />);
    expect(screen.getByText("Unavailable")).toBeTruthy();
    expect(screen.getByText("Bluetooth is not available here: no adapter found.")).toBeTruthy();
  });

  it("says when a phone is connected, and not otherwise", async () => {
    getConnections.mockResolvedValue(makeConnections({ bluetooth: { ...bt, session_open: true } }));
    await renderPanel(<BluetoothStatus variant="catalogue" />);
    expect(screen.getByText("A phone is connected now.")).toBeTruthy();
    cleanup();
    getConnections.mockResolvedValue(makeConnections());
    await renderPanel(<BluetoothStatus variant="catalogue" />);
    expect(screen.queryByText("A phone is connected now.")).toBeNull();
  });
});

describe("Pairing status", () => {
  it("reads Claimed on a date", async () => {
    await renderPanel(<PairingStatus variant="catalogue" />);
    expect(screen.getByText(/^Claimed on .*2026$/)).toBeTruthy();
  });

  it("reads Waiting to be claimed, with the card's id", async () => {
    getSetupStatus.mockResolvedValue(SETUP_WAITING);
    await renderPanel(<PairingStatus variant="hub" />);
    expect(screen.getByText("Waiting to be claimed (card 4e81)")).toBeTruthy();
  });

  it("says when this device made the claim", async () => {
    getSetupStatus.mockResolvedValue({ ...SETUP_CLAIMED, claimed_by_this_device: true });
    await renderPanel(<PairingStatus variant="hub" />);
    expect(screen.getByText("This device claimed it.")).toBeTruthy();
  });

  it("never shows a code, even if the pond sent one by mistake", async () => {
    getSetupStatus.mockResolvedValue({ ...SETUP_WAITING, code: "123456", pairing_code: "654321" } as never);
    await renderPanel(<PairingStatus variant="catalogue" />);
    expect(document.body.textContent).not.toMatch(/123456|654321/);
    expect(screen.getByText(/never shown here/)).toBeTruthy();
  });

  it("offers no way to ask for a code", async () => {
    await renderPanel(<PairingStatus variant="catalogue" />);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("says the card's state is not known when the pond has not said", async () => {
    getSetupStatus.mockRejectedValue(new ApiError(404, "Not Found"));
    await renderPanel(<PairingStatus variant="catalogue" />);
    expect(screen.getByText(/has not said whether its setup card was claimed/)).toBeTruthy();
  });
});

describe("when the pond cannot be read", () => {
  it("says it is finding out while the first answer is awaited", async () => {
    getConnections.mockReturnValue(new Promise(() => {}));
    render(
      <ConnectionsProvider>
        <EthernetPanel variant="catalogue" />
      </ConnectionsProvider>,
    );
    await act(async () => {});
    expect(screen.getByText("Finding out what is connected.")).toBeTruthy();
  });

  it("says the pond is not answering, and offers to try now", async () => {
    getConnections.mockRejectedValue(new TypeError("Failed to fetch"));
    await renderPanel(<EthernetPanel variant="catalogue" />);
    expect(screen.getByText(/The pond is not answering/)).toBeTruthy();
    getConnections.mockResolvedValue(makeConnections());
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Try now" }));
    });
    expect(screen.getByText("eth0")).toBeTruthy();
  });

  it("says the pond answered in a form it does not understand, rather than drawing nothing", async () => {
    getConnections.mockResolvedValue("<!doctype html>" as never);
    await renderPanel(<BluetoothStatus variant="catalogue" />);
    expect(screen.getByText(/in a form this app does not understand/)).toBeTruthy();
  });

  it("refuses the v1 shape, where can_change was a bare boolean", async () => {
    getConnections.mockResolvedValue({ ...makeConnections(), can_change: true } as never);
    await renderPanel(<EthernetPanel variant="catalogue" />);
    expect(screen.getByText(/in a form this app does not understand/)).toBeTruthy();
  });
});
