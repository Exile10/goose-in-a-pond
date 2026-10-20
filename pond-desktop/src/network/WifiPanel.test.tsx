import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { ApiError, type WifiJoinResult, type WifiNetwork } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getConnections: vi.fn(),
    getSetupStatus: vi.fn(),
    getSystemInfo: vi.fn(),
    scanWifi: vi.fn(),
    joinWifi: vi.fn(),
    forgetWifi: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { ConnectionsProvider } from "./ConnectionsProvider";
import { WifiPanel } from "./WifiPanel";
import { SCAN, SETUP_CLAIMED, makeConnections, makeNetwork } from "./connectionsFixtures";

const getConnections = vi.mocked(api.getConnections);
const scanWifi = vi.mocked(api.scanWifi);
const joinWifi = vi.mocked(api.joinWifi);
const forgetWifi = vi.mocked(api.forgetWifi);

const off = (reason: string | null) => ({ available: false, reason });
const on = { available: true, reason: null };
const disconnected = { state: "disconnected" as const, ssid: null, address: null, signal: null };

const PASSPHRASE = "swordfish-9000";
const JOINED: WifiJoinResult = { ssid: "Neighbour", address: "192.168.1.30", https_port: 4443, hostname: "nano" };

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function mount(over: Parameters<typeof makeConnections>[0] = {}, variant: "hub" | "catalogue" = "hub") {
  getConnections.mockResolvedValue(makeConnections(over));
  render(
    <ConnectionsProvider>
      <WifiPanel variant={variant} />
    </ConnectionsProvider>,
  );
  await act(async () => {});
}

const click = (el: HTMLElement) =>
  act(async () => {
    fireEvent.click(el);
  });
const type = (el: HTMLElement, value: string) =>
  act(async () => {
    fireEvent.change(el, { target: { value } });
  });
const button = (name: string | RegExp) => screen.getByRole("button", { name }) as HTMLButtonElement;
const rowOf = (ssid: string) =>
  within(screen.getByRole("list", { name: "Wi-Fi networks" })).getByText(ssid, { selector: ".net__title" }).closest("li") as HTMLElement;

async function findNetworks(list: WifiNetwork[] = SCAN) {
  scanWifi.mockResolvedValue(list);
  await click(button("Find networks"));
}

async function openJoin(ssid: string) {
  await click(within(rowOf(ssid)).getByRole("button", { name: `Join ${ssid}` }));
}

/** The form's own submit, since a click on a submit button is how a person sends it. */
async function sendJoin(ssid: string) {
  const form = screen.getByRole("form", { name: `Join ${ssid}` });
  await click(within(form).getByRole("button", { name: `Join ${ssid}` }));
}

beforeEach(() => {
  vi.clearAllMocks();
  getConnections.mockResolvedValue(makeConnections());
  vi.mocked(api.getSetupStatus).mockResolvedValue(SETUP_CLAIMED);
  vi.mocked(api.getSystemInfo).mockResolvedValue({ platform: "linux" } as never);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  localStorage.clear();
  sessionStorage.clear();
});

describe("the network the pond is on", () => {
  it("shows its name, address and signal", async () => {
    await mount();
    expect(screen.getByText("Home")).toBeTruthy();
    expect(screen.getByText("192.168.1.11")).toBeTruthy();
    expect(screen.getByRole("img", { name: "Signal 70 percent" })).toBeTruthy();
    expect(screen.getByText("70%")).toBeTruthy();
  });

  it("says so when the pond is on no Wi-Fi network", async () => {
    await mount({ wifi: disconnected });
    expect(screen.getByText("Not on a Wi-Fi network")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /^Forget/ })).toBeNull();
  });

  it("explains, with the pond's own reason, when this host has no Wi-Fi control", async () => {
    await mount({
      wifi: {
        ...disconnected,
        state: "unavailable",
        reason: "no Wi-Fi hardware on this machine",
        capabilities: { scan: off("no Wi-Fi hardware on this machine"), join: off(null), forget: off(null) },
      },
    });
    expect(screen.getByText("No Wi-Fi control here")).toBeTruthy();
    expect(screen.getByText("no Wi-Fi hardware on this machine.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Find networks" })).toBeNull();
  });

  it("falls back to its own explanation when the pond gave no reason", async () => {
    await mount({
      wifi: { ...disconnected, state: "unavailable", capabilities: { scan: off(null), join: off(null), forget: off(null) } },
    });
    expect(screen.getByText(/no Wi-Fi hardware, or its operating system does not allow/)).toBeTruthy();
  });
});

describe("controls the pond says are off", () => {
  const reason = "the pond's user may not change Wi-Fi (run: bash scripts/giap.sh wifi-grant)";

  it("disables Find networks and shows why when scanning is unavailable", async () => {
    await mount({ wifi: { capabilities: { scan: off("Location is not allowed for this app"), join: on, forget: on } } });
    expect(button("Find networks").disabled).toBe(true);
    expect(screen.getByText("Finding networks is unavailable: Location is not allowed for this app.")).toBeTruthy();
  });

  it("disables joining and forgetting, and says why once, when the OS refuses them for one reason", async () => {
    await mount({ wifi: { capabilities: { scan: on, join: off(reason), forget: off(reason) } } });
    expect(button("Join a hidden network").disabled).toBe(true);
    expect(button("Forget Home").disabled).toBe(true);
    expect(screen.getAllByText(/wifi-grant/)).toHaveLength(1);
    expect(screen.getByText(`Joining a network and forgetting a network are unavailable: ${reason}.`)).toBeTruthy();

    await findNetworks();
    expect(within(rowOf("Neighbour")).getByRole("button", { name: "Join Neighbour" })).toHaveProperty("disabled", true);
    expect(within(rowOf("Old flat")).getByRole("button", { name: "Forget Old flat" })).toHaveProperty("disabled", true);
    // Looking is still allowed.
    expect(button("Find networks").disabled).toBe(false);
  });

  it("disables changes, and shows the reason, for a caller who may not change the network", async () => {
    await mount({
      can_change: {
        allowed: false,
        reason: "Only the phone that set up this pond, or a household member's device, can change its network",
      },
    });
    expect(
      screen.getByText(
        "Changing the network is off for this device: Only the phone that set up this pond, or a household member's device, can change its network.",
      ),
    ).toBeTruthy();
    expect(button("Join a hidden network").disabled).toBe(true);
    expect(button("Forget Home").disabled).toBe(true);
    expect(button("Find networks").disabled).toBe(false);

    await findNetworks();
    expect(within(rowOf("Cafe")).getByRole("button", { name: "Join Cafe" })).toHaveProperty("disabled", true);
  });

  it("still says why when the pond gives no reason for the refusal", async () => {
    await mount({ can_change: { allowed: false, reason: null } });
    expect(screen.getByText(/Changing the network is off for this device: only the phone that set up this pond/)).toBeTruthy();
  });

  it("leaves every control on when the caller may change the network", async () => {
    await mount();
    expect(button("Find networks").disabled).toBe(false);
    expect(button("Join a hidden network").disabled).toBe(false);
    expect(button("Forget Home").disabled).toBe(false);
    expect(document.querySelector(".net__notes")).toBeNull();
  });
});

describe("Find networks", () => {
  it("invites a scan before there is a list, and does not scan on its own", async () => {
    await mount({ wifi: disconnected });
    expect(screen.getByText(/Find networks to see what the pond can join/)).toBeTruthy();
    expect(scanWifi).not.toHaveBeenCalled();
  });

  it("lists networks with security, signal and the known and current markers", async () => {
    await mount();
    await findNetworks();
    expect(scanWifi).toHaveBeenCalledTimes(1);
    expect(screen.getByText("5 networks found.")).toBeTruthy();

    const home = rowOf("Home");
    expect(within(home).getByText("Connected")).toBeTruthy();
    expect(within(home).getByText("WPA2")).toBeTruthy();
    expect(within(home).getByRole("img", { name: "Signal 70 percent" })).toBeTruthy();
    expect(within(home).queryByRole("button", { name: /^Join/ })).toBeNull();

    expect(within(rowOf("Old flat")).getByText("Saved")).toBeTruthy();
    expect(within(rowOf("Neighbour")).getByText("WPA3")).toBeTruthy();
    expect(within(rowOf("Neighbour")).queryByText("Saved")).toBeNull();
    expect(within(rowOf("Cafe")).getByText("Open")).toBeTruthy();
    expect(within(rowOf("Cafe")).getByRole("img", { name: "Signal 20 percent" })).toBeTruthy();
  });

  it("lists an enterprise network with a note and no way to join it", async () => {
    await mount();
    await findNetworks();
    const office = rowOf("Office");
    expect(within(office).getByText("Work or school")).toBeTruthy();
    expect(within(office).getByText(/username and certificate/)).toBeTruthy();
    expect(within(office).queryByRole("button", { name: /^Join/ })).toBeNull();
  });

  it("says no networks were found, with something to do next", async () => {
    await mount();
    await findNetworks([]);
    expect(screen.getByText("No networks found")).toBeTruthy();
    expect(screen.getByText(/join a hidden network by its name/)).toBeTruthy();
  });

  it("shows progress, and holds off joining, while it scans", async () => {
    await mount();
    await findNetworks();
    const pending = deferred<WifiNetwork[]>();
    scanWifi.mockReturnValue(pending.promise);
    await click(button("Find networks"));
    expect(button("Finding networks").disabled).toBe(true);
    expect(button("Finding networks").getAttribute("aria-busy")).toBe("true");
    expect(within(rowOf("Cafe")).getByRole("button", { name: "Join Cafe" })).toHaveProperty("disabled", true);
    expect(screen.getByText(/can take up to 15 seconds/)).toBeTruthy();
    await act(async () => pending.resolve(SCAN));
    expect(button("Find networks").disabled).toBe(false);
  });

  it.each([
    ["unsupported", 503, /cannot control Wi-Fi/],
    ["not_permitted", 403, /bash scripts\/giap\.sh wifi-grant/],
    ["busy", 409, /busy with another Wi-Fi scan or join/],
  ])("explains a %s refusal in plain words", async (code, status, text) => {
    await mount();
    scanWifi.mockRejectedValue(new ApiError(status, code));
    await click(button("Find networks"));
    expect(screen.getByRole("alert").textContent).toMatch(text);
  });

  it("names Location Services when a Mac's pond refuses the scan", async () => {
    vi.mocked(api.getSystemInfo).mockResolvedValue({ platform: "macos" } as never);
    await mount();
    scanWifi.mockRejectedValue(new ApiError(403, "not_permitted"));
    await click(button("Find networks"));
    expect(screen.getByRole("alert").textContent).toMatch(/Location Services/);
    expect(screen.getByRole("alert").textContent).not.toMatch(/wifi-grant/);
  });

  it("says the scan took too long when our own timeout ends it", async () => {
    await mount();
    scanWifi.mockRejectedValue(new ApiError(408, "Request timed out"));
    await click(button("Find networks"));
    expect(screen.getByRole("alert").textContent).toMatch(/scan took too long/);
  });
});

describe("joining a network", () => {
  it("asks for a masked passphrase, which can be shown and hidden again", async () => {
    await mount();
    await findNetworks();
    await openJoin("Neighbour");
    const field = screen.getByLabelText("Passphrase for Neighbour") as HTMLInputElement;
    expect(field.type).toBe("password");
    expect(field.autocomplete).toBe("off");
    expect(document.activeElement).toBe(field);

    await click(button("Show passphrase"));
    expect(field.type).toBe("text");
    await click(button("Hide passphrase"));
    expect(field.type).toBe("password");
  });

  it("sends the passphrase and reports the join, then closes the form", async () => {
    await mount();
    await findNetworks();
    await openJoin("Neighbour");
    await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
    joinWifi.mockResolvedValue({ ...JOINED, address: "192.168.1.11", ssid: "Neighbour" });
    const reads = getConnections.mock.calls.length;
    await sendJoin("Neighbour");

    expect(joinWifi).toHaveBeenCalledWith({ ssid: "Neighbour", hidden: false, passphrase: PASSPHRASE });
    expect(screen.queryByRole("form", { name: "Join Neighbour" })).toBeNull();
    expect(screen.getByRole("status").textContent).toMatch(/Joined .Neighbour. at 192\.168\.1\.11\./);
    expect(getConnections.mock.calls.length).toBeGreaterThan(reads);
    expect(document.body.textContent).not.toContain(PASSPHRASE);
  });

  it("joins an open network with no passphrase at all", async () => {
    await mount();
    await findNetworks();
    await openJoin("Cafe");
    expect(screen.queryByLabelText(/Passphrase/)).toBeNull();
    expect(screen.getByText(/has no passphrase, so anyone nearby can connect/)).toBeTruthy();
    joinWifi.mockResolvedValue({ ...JOINED, ssid: "Cafe" });
    await sendJoin("Cafe");
    expect(joinWifi).toHaveBeenCalledWith({ ssid: "Cafe", hidden: false });
    expect(Object.keys(joinWifi.mock.calls[0][0])).not.toContain("passphrase");
  });

  it("checks the passphrase before asking the pond", async () => {
    await mount();
    await findNetworks();
    await openJoin("Neighbour");
    await sendJoin("Neighbour");
    expect(screen.getByRole("alert").textContent).toBe("Enter the passphrase.");
    await type(screen.getByLabelText("Passphrase for Neighbour"), "short");
    expect(screen.getByRole("alert").textContent).toBe("A passphrase is 8 to 63 characters.");
    await sendJoin("Neighbour");
    expect(joinWifi).not.toHaveBeenCalled();
  });

  it("closes the form on Cancel and on Escape without sending anything", async () => {
    await mount();
    await findNetworks();
    await openJoin("Neighbour");
    await click(button("Cancel"));
    expect(screen.queryByRole("form", { name: "Join Neighbour" })).toBeNull();

    await openJoin("Neighbour");
    await act(async () => {
      fireEvent.keyDown(screen.getByLabelText("Passphrase for Neighbour"), { key: "Escape" });
    });
    expect(screen.queryByRole("form", { name: "Join Neighbour" })).toBeNull();
    expect(joinWifi).not.toHaveBeenCalled();
  });

  it("warns that joining another network may cut this window off", async () => {
    await mount();
    await findNetworks();
    await openJoin("Neighbour");
    expect(screen.getByText(/moves the pond off .Home./)).toBeTruthy();
    expect(screen.getByText(/may lose contact with it for a moment/)).toBeTruthy();
  });

  it("does not warn when the pond is on no Wi-Fi network", async () => {
    await mount({ wifi: disconnected });
    await findNetworks();
    await openJoin("Neighbour");
    expect(screen.queryByText(/moves the pond off/)).toBeNull();
  });

  describe("while it waits", () => {
    it("shows progress without blocking the window", async () => {
      await mount();
      await findNetworks();
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
      const pending = deferred<WifiJoinResult>();
      joinWifi.mockReturnValue(pending.promise);
      await sendJoin("Neighbour");

      const banner = screen.getByText(/Joining .Neighbour./).closest("[role=status]") as HTMLElement;
      expect(banner.textContent).toMatch(/up to 45 seconds, and you can keep using the app/);
      expect(screen.queryByRole("dialog")).toBeNull();
      expect(document.querySelector("[aria-modal=true]")).toBeNull();

      // The pond answers busy to a second request, so the others wait; nothing is hidden or trapped.
      expect(button("Find networks").disabled).toBe(true);
      expect(button("Join a hidden network").disabled).toBe(true);
      expect(button("Forget Home").disabled).toBe(true);
      expect(within(rowOf("Cafe")).getByRole("button", { name: "Join Cafe" })).toHaveProperty("disabled", true);
      expect((screen.getByLabelText("Passphrase for Neighbour") as HTMLInputElement).disabled).toBe(true);

      await act(async () => pending.resolve(JOINED));
      expect(screen.queryByText(/up to 45 seconds/)).toBeNull();
      expect(button("Find networks").disabled).toBe(false);
    });

    it("counts the seconds on its own clock", async () => {
      vi.useFakeTimers();
      await mount();
      await findNetworks();
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
      joinWifi.mockReturnValue(new Promise(() => {}));
      await sendJoin("Neighbour");
      expect(screen.getByText("0 s")).toBeTruthy();
      await act(() => vi.advanceTimersByTimeAsync(3_000));
      expect(screen.getByText("3 s")).toBeTruthy();
    });
  });

  describe("when it fails", () => {
    async function failWith(error: unknown, ssid = "Neighbour") {
      await mount();
      await findNetworks();
      await openJoin(ssid);
      if (ssid !== "Cafe") await type(screen.getByLabelText(`Passphrase for ${ssid}`), PASSPHRASE);
      joinWifi.mockRejectedValue(error);
      await sendJoin(ssid);
      return screen.getByRole("form", { name: `Join ${ssid}` });
    }

    it.each([
      ["wrong_password", 401, /did not accept that passphrase/],
      ["not_found", 404, /could not find .Neighbour./],
      ["timeout", 504, /did not answer in time/],
      ["no_address", 502, /gave it no address/],
      ["invalid_request", 400, /not valid/],
      ["unsupported", 503, /cannot control Wi-Fi/],
      ["busy", 409, /busy with another Wi-Fi scan or join/],
      ["not_permitted", 403, /bash scripts\/giap\.sh wifi-grant/],
      ["not_allowed", 403, /may not change the pond's network/],
      ["unsupported_security", 400, /username and certificate/],
    ])("explains %s beside the form, and keeps the form open", async (code, status, text) => {
      const form = await failWith(new ApiError(status, code));
      expect(within(form).getByRole("alert").textContent).toMatch(text);
      expect((screen.getByLabelText("Passphrase for Neighbour") as HTMLInputElement).value).toBe(PASSPHRASE);
      expect(button("Join Neighbour")).toBeTruthy();
    });

    it("names Location Services, not wifi-grant, when a Mac refuses", async () => {
      vi.mocked(api.getSystemInfo).mockResolvedValue({ platform: "macos" } as never);
      const form = await failWith(new ApiError(403, "not_permitted"));
      expect(within(form).getByRole("alert").textContent).toMatch(/Location Services/);
    });

    it("says the pond did not answer when our own timeout ends the wait", async () => {
      const form = await failWith(new ApiError(408, "Request timed out"));
      expect(within(form).getByRole("alert").textContent).toMatch(/may still be joining/);
    });

    it("lets the household correct the passphrase and try again", async () => {
      const form = await failWith(new ApiError(401, "wrong_password"));
      expect(form).toBeTruthy();
      await type(screen.getByLabelText("Passphrase for Neighbour"), "a better passphrase");
      joinWifi.mockResolvedValue(JOINED);
      await sendJoin("Neighbour");
      expect(joinWifi).toHaveBeenLastCalledWith({ ssid: "Neighbour", hidden: false, passphrase: "a better passphrase" });
      expect(screen.queryByRole("form", { name: "Join Neighbour" })).toBeNull();
    });

    it("clears the failure when the household cancels", async () => {
      await failWith(new ApiError(401, "wrong_password"));
      await click(button("Cancel"));
      expect(screen.queryByRole("alert")).toBeNull();
    });

    it("never logs or stores the passphrase, whatever happens", async () => {
      const spies = (["log", "info", "warn", "error", "debug"] as const).map((m) => vi.spyOn(console, m).mockImplementation(() => {}));
      await failWith(new ApiError(401, "wrong_password"));
      await click(button("Cancel"));
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
      joinWifi.mockResolvedValue(JOINED);
      await sendJoin("Neighbour");

      for (const spy of spies) {
        expect(JSON.stringify(spy.mock.calls)).not.toContain(PASSPHRASE);
        spy.mockRestore();
      }
      expect(JSON.stringify({ ...localStorage })).not.toContain(PASSPHRASE);
      expect(JSON.stringify({ ...sessionStorage })).not.toContain(PASSPHRASE);
      expect(document.body.innerHTML).not.toContain(PASSPHRASE);
    });
  });

  describe("a hidden network", () => {
    async function openHidden() {
      await mount();
      await click(button("Join a hidden network"));
      return screen.getByRole("form", { name: "Join a hidden network" });
    }

    it("asks for the name and a masked passphrase and sends hidden: true", async () => {
      await openHidden();
      expect((screen.getByLabelText("Passphrase") as HTMLInputElement).type).toBe("password");
      await type(screen.getByLabelText("Network name"), "Back room");
      await type(screen.getByLabelText("Passphrase"), PASSPHRASE);
      joinWifi.mockResolvedValue({ ...JOINED, ssid: "Back room" });
      await click(screen.getByRole("button", { name: "Join" }));
      expect(joinWifi).toHaveBeenCalledWith({ ssid: "Back room", hidden: true, passphrase: PASSPHRASE });
    });

    it("leaves the passphrase out when it is empty, for an open network", async () => {
      await openHidden();
      await type(screen.getByLabelText("Network name"), "Back room");
      joinWifi.mockResolvedValue({ ...JOINED, ssid: "Back room" });
      await click(screen.getByRole("button", { name: "Join" }));
      expect(joinWifi).toHaveBeenCalledWith({ ssid: "Back room", hidden: true });
    });

    it("wants a name, and no more than 32 bytes of it, before asking the pond", async () => {
      await openHidden();
      await click(screen.getByRole("button", { name: "Join" }));
      expect(screen.getByRole("alert").textContent).toBe("Enter the network's name.");
      await type(screen.getByLabelText("Network name"), "x".repeat(33));
      expect(screen.getByRole("alert").textContent).toMatch(/32 bytes/);
      expect(joinWifi).not.toHaveBeenCalled();
    });

    it("shows what the pond refused beside the form", async () => {
      const form = await openHidden();
      await type(screen.getByLabelText("Network name"), "Back room");
      joinWifi.mockRejectedValue(new ApiError(404, "not_found"));
      await click(screen.getByRole("button", { name: "Join" }));
      expect(within(form).getByRole("alert").textContent).toMatch(/could not find .Back room./);
    });

    it("closes from its own button", async () => {
      await openHidden();
      await click(button("Join a hidden network"));
      expect(screen.queryByRole("form", { name: "Join a hidden network" })).toBeNull();
    });
  });

  describe("that moves the pond to another network", () => {
    const moved: WifiJoinResult = { ssid: "Neighbour", address: "10.0.0.7", https_port: 4443, hostname: "nano" };

    it("says the connection may drop, and where to find the pond afterwards", async () => {
      await mount();
      await findNetworks();
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
      joinWifi.mockResolvedValue(moved);
      await sendJoin("Neighbour");
      const note = screen.getByRole("status");
      expect(note.textContent).toMatch(/Joined .Neighbour. at 10\.0\.0\.7\./);
      expect(note.textContent).toMatch(/moved to another network, so this window may lose contact with it/);
      expect(note.textContent).toContain("Find it at nano.local or 10.0.0.7, port 4443.");
    });

    it("keeps asking while the pond does not answer, and says so when it does", async () => {
      vi.useFakeTimers();
      await mount();
      await findNetworks();
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);

      joinWifi.mockResolvedValue(moved);
      getConnections.mockRejectedValue(new TypeError("Failed to fetch"));
      await sendJoin("Neighbour");
      expect(screen.getByText(/The pond is not answering/)).toBeTruthy();

      // Still gone: it keeps trying, and does not give up or throw.
      await act(() => vi.advanceTimersByTimeAsync(2_000));
      expect(screen.getByText(/The pond is not answering/)).toBeTruthy();

      getConnections.mockResolvedValue(
        makeConnections({ wifi: { ssid: "Neighbour", address: "10.0.0.7", signal: 55 } }),
      );
      await act(() => vi.advanceTimersByTimeAsync(3_000));
      expect(screen.queryByText(/The pond is not answering/)).toBeNull();
      expect(screen.getByText("Back in touch with the pond.")).toBeTruthy();
      expect(screen.getByText("10.0.0.7")).toBeTruthy();

      await act(() => vi.advanceTimersByTimeAsync(9_000));
      expect(screen.queryByText("Back in touch with the pond.")).toBeNull();
    });

    it("reads a dropped connection mid-join as a pond that moved, not as a failure", async () => {
      vi.useFakeTimers();
      await mount();
      await findNetworks();
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
      joinWifi.mockRejectedValue(new TypeError("Failed to fetch"));
      getConnections.mockRejectedValue(new TypeError("Failed to fetch"));
      await sendJoin("Neighbour");

      expect(screen.getByText(/Lost contact with the pond while it changed network/)).toBeTruthy();
      expect(screen.queryByRole("form", { name: "Join Neighbour" })).toBeNull();

      getConnections.mockResolvedValue(makeConnections({ wifi: { ssid: "Neighbour", address: "10.0.0.7" } }));
      await act(() => vi.advanceTimersByTimeAsync(4_000));
      expect(screen.getByText("Back in touch with the pond.")).toBeTruthy();
      expect(screen.queryByText(/Lost contact with the pond/)).toBeNull();
    });

    it("does not say the connection may drop when the pond stays where it was", async () => {
      await mount();
      await findNetworks();
      await openJoin("Neighbour");
      await type(screen.getByLabelText("Passphrase for Neighbour"), PASSPHRASE);
      joinWifi.mockResolvedValue({ ...moved, ssid: "Home", address: "192.168.1.11" });
      await sendJoin("Neighbour");
      expect(screen.getByRole("status").textContent).not.toMatch(/lose contact/);
    });
  });
});

describe("forgetting a network", () => {
  it("asks first, and sends nothing until it is confirmed", async () => {
    await mount();
    await findNetworks();
    await click(within(rowOf("Old flat")).getByRole("button", { name: "Forget Old flat" }));
    const confirm = screen.getByRole("group", { name: "Forget Old flat" });
    expect(confirm.textContent).toMatch(/stops joining it on its own, and joining it again needs its passphrase/);
    expect(forgetWifi).not.toHaveBeenCalled();

    await click(within(confirm).getByRole("button", { name: "Keep it" }));
    expect(screen.queryByRole("group", { name: "Forget Old flat" })).toBeNull();
    expect(forgetWifi).not.toHaveBeenCalled();
  });

  it("forgets a network the pond is not on, without confirm", async () => {
    await mount();
    await findNetworks();
    await click(within(rowOf("Old flat")).getByRole("button", { name: "Forget Old flat" }));
    forgetWifi.mockResolvedValue(undefined);
    await click(within(screen.getByRole("group", { name: "Forget Old flat" })).getByRole("button", { name: /^Forget/ }));
    expect(forgetWifi).toHaveBeenCalledWith("Old flat", false);
    expect(screen.getByRole("status").textContent).toBe("Forgot “Old flat”.");
    expect(within(rowOf("Old flat")).queryByText("Saved")).toBeNull();
    expect(within(rowOf("Old flat")).queryByRole("button", { name: /^Forget/ })).toBeNull();
  });

  it("explains that the pond may drop off the network, then sends confirm for the one it is on", async () => {
    await mount();
    await click(button("Forget Home"));
    const confirm = screen.getByRole("group", { name: "Forget Home" });
    expect(confirm.textContent).toMatch(/is the network the pond is on now/);
    expect(confirm.textContent).toMatch(/may drop off your network/);
    expect(confirm.textContent).toMatch(/set it up again from your phone, over Bluetooth/);
    expect(forgetWifi).not.toHaveBeenCalled();

    forgetWifi.mockResolvedValue(undefined);
    await click(within(confirm).getByRole("button", { name: /anyway/ }));
    expect(forgetWifi).toHaveBeenCalledWith("Home", true);
    expect(screen.getByRole("status").textContent).toMatch(/The pond may have left your network/);
  });

  it("keeps asking when the pond stops answering after it forgot its own network", async () => {
    vi.useFakeTimers();
    await mount();
    await click(button("Forget Home"));
    forgetWifi.mockResolvedValue(undefined);
    getConnections.mockRejectedValue(new TypeError("Failed to fetch"));
    await click(screen.getByRole("button", { name: /anyway/ }));
    expect(screen.getByText(/The pond is not answering/)).toBeTruthy();

    getConnections.mockResolvedValue(makeConnections({ wifi: disconnected }));
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(screen.getByText("Back in touch with the pond.")).toBeTruthy();
    expect(screen.getByText("Not on a Wi-Fi network")).toBeTruthy();
  });

  it("asks again, more strongly, when the pond says the network is in use", async () => {
    await mount({ wifi: { ssid: "Elsewhere", address: "192.168.1.9" } });
    await findNetworks([makeNetwork({ ssid: "Old flat", known: true })]);
    await click(within(rowOf("Old flat")).getByRole("button", { name: "Forget Old flat" }));
    forgetWifi.mockRejectedValueOnce(new ApiError(409, "in_use"));
    await click(within(screen.getByRole("group", { name: "Forget Old flat" })).getByRole("button", { name: /^Forget/ }));

    const confirm = screen.getByRole("group", { name: "Forget Old flat" });
    expect(confirm.textContent).toMatch(/is the network the pond is on now/);
    expect(within(confirm).getByRole("alert").textContent).toMatch(/off your network/);
    expect(forgetWifi).toHaveBeenCalledWith("Old flat", false);

    forgetWifi.mockResolvedValue(undefined);
    await click(within(confirm).getByRole("button", { name: /anyway/ }));
    expect(forgetWifi).toHaveBeenLastCalledWith("Old flat", true);
  });

  it("says a network is already gone when the pond has no such saved network", async () => {
    await mount();
    await findNetworks();
    await click(within(rowOf("Old flat")).getByRole("button", { name: "Forget Old flat" }));
    forgetWifi.mockRejectedValue(new ApiError(404, "not_found"));
    await click(within(screen.getByRole("group", { name: "Forget Old flat" })).getByRole("button", { name: /^Forget/ }));
    expect(screen.getByRole("alert").textContent).toMatch(/no saved network called .Old flat./);
    expect(within(rowOf("Old flat")).queryByText("Saved")).toBeNull();
  });

  it("explains a refusal beside the confirm and leaves it open", async () => {
    await mount();
    await click(button("Forget Home"));
    forgetWifi.mockRejectedValue(new ApiError(403, "not_permitted"));
    await click(screen.getByRole("button", { name: /anyway/ }));
    const confirm = screen.getByRole("group", { name: "Forget Home" });
    expect(within(confirm).getByRole("alert").textContent).toMatch(/wifi-grant/);
  });
});
