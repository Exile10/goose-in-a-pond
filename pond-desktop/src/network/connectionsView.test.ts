import { describe, expect, it } from "vitest";
import { ApiError, type ConnectionErrorCode } from "../api/types";
import {
  ENTERPRISE_NOTE,
  capabilityNotices,
  claimedDate,
  connectionErrorCode,
  connectionErrorText,
  describeBluetooth,
  describeEthernet,
  describeSetup,
  isConnectionLost,
  isConnections,
  isSetupStatus,
  joinLeavesNetwork,
  joinMoved,
  passphraseProblem,
  reachLine,
  securityLabel,
  signalBars,
  signalLabel,
  speedLabel,
  ssidProblem,
} from "./connectionsView";
import { SETUP_CLAIMED, SETUP_WAITING, makeConnections } from "./connectionsFixtures";

const refused = (status: number, error: string) => new ApiError(status, error);

describe("connectionErrorCode", () => {
  it("reads the contract's code from the error message, where the client keeps it", () => {
    expect(connectionErrorCode(refused(401, "wrong_password"))).toBe("wrong_password");
  });

  it("also reads a structured code, as other routes carry", () => {
    expect(connectionErrorCode(new ApiError(409, "Busy", "busy"))).toBe("busy");
  });

  it("knows nothing about a code the contract does not list, or an error that is not the pond's", () => {
    expect(connectionErrorCode(refused(500, "surprise"))).toBeNull();
    expect(connectionErrorCode(new TypeError("Failed to fetch"))).toBeNull();
    expect(connectionErrorCode(null)).toBeNull();
  });
});

describe("connectionErrorText", () => {
  const ssid = "Home";

  it("names the giap.sh wifi-grant fix for not_permitted on Linux", () => {
    const text = connectionErrorText(refused(403, "not_permitted"), { action: "join", ssid, platform: "linux" });
    expect(text).toContain("bash scripts/giap.sh wifi-grant");
    expect(text).not.toContain("Location");
  });

  it("names Location Services for not_permitted on a Mac", () => {
    const text = connectionErrorText(refused(403, "not_permitted"), { action: "scan", platform: "macos" });
    expect(text).toContain("Location Services");
    expect(text).not.toContain("wifi-grant");
  });

  it("gives both fixes when the pond's system is not known", () => {
    const text = connectionErrorText(refused(403, "not_permitted"), { action: "scan", platform: null });
    expect(text).toContain("wifi-grant");
    expect(text).toContain("Location Services");
  });

  it("explains that this host has no Wi-Fi control for unsupported", () => {
    const text = connectionErrorText(refused(503, "unsupported"), { action: "scan" });
    expect(text).toMatch(/cannot control Wi-Fi/);
    expect(text).toMatch(/no Wi-Fi hardware/);
  });

  it("says a wrong passphrase was not accepted, by network name", () => {
    expect(connectionErrorText(refused(401, "wrong_password"), { action: "join", ssid })).toBe(
      "“Home” did not accept that passphrase. Check it and try again.",
    );
  });

  it("says a network the pond could not see was not found, and a forgotten one is already gone", () => {
    expect(connectionErrorText(refused(404, "not_found"), { action: "join", ssid })).toMatch(/could not find/);
    expect(connectionErrorText(refused(404, "not_found"), { action: "forget", ssid })).toMatch(
      /no saved network called “Home”/,
    );
  });

  it("explains a timeout and a missing address in turn", () => {
    expect(connectionErrorText(refused(504, "timeout"), { action: "join", ssid })).toMatch(/did not answer in time/);
    expect(connectionErrorText(refused(502, "no_address"), { action: "join", ssid })).toMatch(/gave it no address/);
  });

  it("says why an enterprise network cannot be joined", () => {
    const text = connectionErrorText(refused(400, "unsupported_security"), { action: "join", ssid: "Office" });
    expect(text).toMatch(/work or school network/);
    expect(text).toMatch(/username and certificate/);
    expect(text).toMatch(/only join with a passphrase/);
  });

  it("states the limits of a valid name and passphrase for invalid_request", () => {
    expect(connectionErrorText(refused(400, "invalid_request"), { action: "join", ssid })).toMatch(/1 to 32.*8 to 63/);
  });

  it("explains busy and in_use", () => {
    expect(connectionErrorText(refused(409, "busy"), { action: "scan" })).toMatch(/busy/);
    expect(connectionErrorText(refused(409, "in_use"), { action: "forget", ssid })).toMatch(/off your network/);
  });

  it("prefers the reason can_change gives for not_allowed, and has its own words without one", () => {
    expect(
      connectionErrorText(refused(403, "not_allowed"), {
        action: "join",
        changeReason: "Only the phone that set up this pond can change its network",
      }),
    ).toBe("Only the phone that set up this pond can change its network");
    expect(connectionErrorText(refused(403, "not_allowed"), { action: "join" })).toMatch(/may not change the pond's network/);
  });

  it("covers every code in the contract with a sentence of its own", () => {
    const codes: ConnectionErrorCode[] = [
      "not_allowed",
      "not_permitted",
      "unsupported",
      "busy",
      "in_use",
      "wrong_password",
      "not_found",
      "timeout",
      "no_address",
      "invalid_request",
      "unsupported_security",
    ];
    const texts = codes.map((code) => connectionErrorText(refused(400, code), { action: "join", ssid, platform: "linux" }));
    expect(new Set(texts).size).toBe(codes.length);
    for (const t of texts) expect(t).not.toMatch(/^\w+_\w+$/);
  });

  it("words our own timeout by what was being done", () => {
    const timedOut = new ApiError(408, "Request timed out");
    expect(connectionErrorText(timedOut, { action: "scan" })).toMatch(/scan took too long/);
    expect(connectionErrorText(timedOut, { action: "join" })).toMatch(/may still be joining/);
    expect(connectionErrorText(timedOut, { action: "forget" })).toBe("The pond did not answer.");
  });

  it("says the pond is not answering when nothing came back", () => {
    expect(connectionErrorText(new TypeError("Failed to fetch"), { action: "read" })).toMatch(/connections are unknown/);
    expect(connectionErrorText(new TypeError("Failed to fetch"), { action: "join" })).toBe("The pond is not answering.");
  });

  it("does not leave a household with a bare code for a refusal it has not heard of", () => {
    expect(connectionErrorText(refused(500, "surprise"), { action: "join" })).toBe("The pond could not do that (surprise).");
  });

  it("sends a refused host credential to the sign-in words the other host-only screens use", () => {
    expect(connectionErrorText(refused(403, "host_credential_required"), { action: "forget" })).toMatch(/signed in/);
  });
});

describe("isConnectionLost", () => {
  it("is a failure to reach the pond at all, not a refusal from it", () => {
    expect(isConnectionLost(new TypeError("Failed to fetch"))).toBe(true);
    expect(isConnectionLost(refused(409, "busy"))).toBe(false);
    expect(isConnectionLost(new ApiError(408, "Request timed out"))).toBe(false);
  });
});

describe("signalBars", () => {
  it.each([
    [0, 0],
    [1, 1],
    [25, 1],
    [26, 2],
    [50, 2],
    [51, 3],
    [75, 3],
    [76, 4],
    [100, 4],
    [140, 4],
    [-10, 0],
  ])("draws %i percent as %i bars", (signal, bars) => {
    expect(signalBars(signal)).toBe(bars);
  });

  it("draws nothing for no reading", () => {
    expect(signalBars(null)).toBe(0);
    expect(signalBars(Number.NaN)).toBe(0);
  });

  it("says the reading in words for a screen reader", () => {
    expect(signalLabel(70)).toBe("Signal 70 percent");
    expect(signalLabel(null)).toBe("No signal reading");
  });
});

describe("securityLabel", () => {
  it("names every security the contract lists", () => {
    expect(["open", "wep", "wpa", "wpa2", "wpa3", "enterprise"].map((s) => securityLabel(s as never))).toEqual([
      "Open",
      "WEP",
      "WPA",
      "WPA2",
      "WPA3",
      "Work or school",
    ]);
    expect(ENTERPRISE_NOTE).toMatch(/username and certificate/);
  });
});

describe("capabilityNotices", () => {
  const off = (reason: string | null) => ({ available: false, reason });
  const on = { available: true, reason: null };

  it("says nothing when the OS allows everything", () => {
    expect(capabilityNotices(makeConnections().wifi)).toEqual([]);
  });

  it("gives actions refused for one reason one sentence, with the reason", () => {
    const reason = "the pond's user may not change Wi-Fi (run: bash scripts/giap.sh wifi-grant)";
    const wifi = makeConnections({ wifi: { capabilities: { scan: on, join: off(reason), forget: off(reason) } } }).wifi;
    expect(capabilityNotices(wifi)).toEqual([
      `Joining a network and forgetting a network are unavailable: ${reason}.`,
    ]);
  });

  it("gives each different reason its own sentence", () => {
    const wifi = makeConnections({ wifi: { capabilities: { scan: off("no radio"), join: off("no radio"), forget: off("read-only") } } }).wifi;
    expect(capabilityNotices(wifi)).toEqual([
      "Finding networks and joining a network are unavailable: no radio.",
      "Forgetting a network is unavailable: read-only.",
    ]);
  });

  it("does not make up a reason the pond did not give", () => {
    const wifi = makeConnections({ wifi: { capabilities: { scan: off(null), join: on, forget: on } } }).wifi;
    expect(capabilityNotices(wifi)).toEqual(["Finding networks is unavailable."]);
  });
});

describe("joining another network", () => {
  const home = makeConnections().wifi;
  const result = { ssid: "Cafe", address: "10.0.0.7", https_port: 4443, hostname: "nano" };

  it("warns only when the pond is on another network now", () => {
    expect(joinLeavesNetwork(home, "Cafe")).toBe(true);
    expect(joinLeavesNetwork(home, "Home")).toBe(false);
    expect(joinLeavesNetwork(makeConnections({ wifi: { state: "disconnected", ssid: null, address: null, signal: null } }).wifi, "Cafe")).toBe(false);
  });

  it("calls a join a move when the pond came up on another network or address", () => {
    expect(joinMoved(home, result)).toBe(true);
    expect(joinMoved(home, { ...result, ssid: "Home", address: "192.168.1.30" })).toBe(true);
    expect(joinMoved(home, { ...result, ssid: "Home", address: "192.168.1.11" })).toBe(false);
  });

  it("does not call it a move when the pond was on no Wi-Fi, as another link carried this app", () => {
    const off = makeConnections({ wifi: { state: "disconnected", ssid: null, address: null, signal: null } }).wifi;
    expect(joinMoved(off, result)).toBe(false);
  });

  it("says where to find the pond afterwards", () => {
    expect(reachLine(result)).toBe("Find it at nano.local or 10.0.0.7, port 4443.");
    expect(reachLine({ ...result, hostname: "nano.lan", https_port: null })).toBe("Find it at nano.lan or 10.0.0.7.");
  });
});

describe("what the household types", () => {
  it("wants a network name, at most 32 bytes", () => {
    expect(ssidProblem("")).toMatch(/Enter the network's name/);
    expect(ssidProblem("   ")).toMatch(/Enter the network's name/);
    expect(ssidProblem("a".repeat(32))).toBeNull();
    expect(ssidProblem("a".repeat(33))).toMatch(/32 bytes/);
    // 17 two-byte letters are 17 characters and 34 bytes.
    expect(ssidProblem("é".repeat(17))).toMatch(/32 bytes/);
  });

  it("wants 8 to 63 characters, or a 64-digit key, for WPA", () => {
    for (const security of ["wpa", "wpa2", "wpa3"] as const) {
      expect(passphraseProblem("", security)).toMatch(/Enter the passphrase/);
      expect(passphraseProblem("short", security)).toMatch(/8 to 63/);
      expect(passphraseProblem("long enough", security)).toBeNull();
      expect(passphraseProblem("x".repeat(63), security)).toBeNull();
      expect(passphraseProblem("x".repeat(64), security)).toMatch(/8 to 63/);
      expect(passphraseProblem("ab".repeat(32), security)).toBeNull();
    }
  });

  it("wants a passphrase for WEP but leaves its odd lengths to the pond", () => {
    expect(passphraseProblem("", "wep")).toMatch(/Enter the passphrase/);
    expect(passphraseProblem("12345", "wep")).toBeNull();
  });

  it("wants none for an open network, and allows none for a hidden one that is open", () => {
    expect(passphraseProblem("", "open")).toBeNull();
    expect(passphraseProblem("", null)).toBeNull();
    expect(passphraseProblem("x".repeat(65), null)).toMatch(/at most 64/);
  });
});

describe("describeBluetooth", () => {
  const bt = makeConnections().bluetooth;

  it("says which name phones see while advertising", () => {
    const line = describeBluetooth(bt);
    expect(line.headline).toBe("Visible to phones");
    expect(line.detail).toContain("“Pond 4e81”");
    expect(line.problem).toBe(false);
  });

  it("says it is ready but not showing itself while serving", () => {
    expect(describeBluetooth({ ...bt, state: "serving", advertising_as: null }).detail).toMatch(/not showing itself/);
  });

  it("tells a switched-off setup from a radio that is off", () => {
    expect(describeBluetooth({ ...bt, state: "off", enabled: false, advertising_as: null }).detail).toMatch(/switched off/);
    expect(describeBluetooth({ ...bt, state: "off", enabled: true, advertising_as: null }).detail).toMatch(/off on this machine/);
  });

  it("gives the reason Bluetooth is unavailable, and marks it as a problem", () => {
    const line = describeBluetooth({ ...bt, state: "unavailable", reason: "no adapter found", advertising_as: null });
    expect(line.detail).toBe("Bluetooth is not available here: no adapter found.");
    expect(line.problem).toBe(true);
    expect(describeBluetooth({ ...bt, state: "unavailable", reason: null }).detail).toMatch(/not available on this machine/);
  });
});

describe("describeSetup", () => {
  it("reads Claimed on a date", () => {
    expect(describeSetup(SETUP_CLAIMED).detail).toMatch(/^Claimed on .*2026$/);
    expect(describeSetup(SETUP_CLAIMED).detail).toContain("18");
  });

  it("reads Waiting to be claimed, with the card's id, while unclaimed", () => {
    expect(describeSetup(SETUP_WAITING).detail).toBe("Waiting to be claimed (card 4e81)");
  });

  it("does not make up a date for a claim with no readable stamp", () => {
    expect(describeSetup({ ...SETUP_CLAIMED, claimed_at: null }).detail).toBe("Claimed");
    expect(claimedDate("not a date")).toBeNull();
  });
});

describe("describeEthernet", () => {
  it("reads a live link with its address and speed", () => {
    expect(describeEthernet({ interface: "eth0", link: "up", address: "192.168.1.20", speed_mbps: 1000 })).toEqual({
      state: "Connected",
      address: "192.168.1.20",
      speed: "1 Gbps",
    });
  });

  it("says a cable that is up with no address has none yet, and one that is down has no link", () => {
    expect(describeEthernet({ interface: "eth0", link: "up", address: null, speed_mbps: null }).address).toBe("No address yet");
    expect(describeEthernet({ interface: "eth0", link: "down", address: null, speed_mbps: 1000 })).toEqual({
      state: "No link",
      address: null,
      speed: null,
    });
  });

  it("writes speeds in the unit a person says them in", () => {
    expect(speedLabel(100)).toBe("100 Mbps");
    expect(speedLabel(2500)).toBe("2.5 Gbps");
    expect(speedLabel(null)).toBeNull();
    expect(speedLabel(0)).toBeNull();
  });
});

describe("the shape guards", () => {
  it("accepts the contract's example", () => {
    expect(isConnections(makeConnections())).toBe(true);
  });

  it("refuses an HTML body or an empty one, which the client casts through", () => {
    expect(isConnections(undefined)).toBe(false);
    expect(isConnections("<!doctype html>")).toBe(false);
    expect(isConnections({})).toBe(false);
  });

  it("refuses a pond that answers with the v1 boolean can_change", () => {
    expect(isConnections({ ...makeConnections(), can_change: true })).toBe(false);
  });

  it("refuses a Wi-Fi block with no capabilities", () => {
    const { capabilities: _gone, ...wifi } = makeConnections().wifi;
    expect(isConnections({ ...makeConnections(), wifi })).toBe(false);
  });

  it("accepts a setup status and refuses nonsense", () => {
    expect(isSetupStatus(SETUP_CLAIMED)).toBe(true);
    expect(isSetupStatus({ id: 4 })).toBe(false);
    expect(isSetupStatus(undefined)).toBe(false);
  });
});
