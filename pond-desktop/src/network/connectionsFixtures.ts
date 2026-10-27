import type { Connections, SetupStatus, WifiNetwork } from "../api/types";

const ready = { available: true, reason: null };

export function makeConnections(over: Partial<Omit<Connections, "wifi">> & { wifi?: Partial<Connections["wifi"]> } = {}): Connections {
  const { wifi, ...rest } = over;
  return {
    hostname: "nano",
    https_port: 4443,
    wifi: {
      state: "connected",
      ssid: "Home",
      address: "192.168.1.11",
      signal: 70,
      capabilities: { scan: ready, join: ready, forget: ready },
      ...wifi,
    },
    ethernet: [{ interface: "eth0", link: "up", address: "192.168.1.20", speed_mbps: 1000 }],
    bluetooth: {
      enabled: true,
      state: "advertising",
      reason: null,
      advertising_as: "Pond 4e81",
      session_open: false,
    },
    can_change: { allowed: true, reason: null },
    ...rest,
  };
}

export function makeNetwork(over: Partial<WifiNetwork> & { ssid: string }): WifiNetwork {
  return { security: "wpa2", signal: 60, known: false, current: false, ...over };
}

export const SCAN: WifiNetwork[] = [
  makeNetwork({ ssid: "Home", signal: 70, known: true, current: true }),
  makeNetwork({ ssid: "Neighbour", signal: 42, security: "wpa3" }),
  makeNetwork({ ssid: "Cafe", signal: 20, security: "open" }),
  makeNetwork({ ssid: "Office", signal: 55, security: "enterprise" }),
  makeNetwork({ ssid: "Old flat", signal: 10, security: "wpa2", known: true }),
];

export const SETUP_CLAIMED: SetupStatus = {
  id: "4e81",
  claimed: true,
  // Midday UTC, so the date reads the same in every time zone.
  claimed_at: "2026-10-18T12:06:12Z",
  claimed_by_this_device: false,
};

export const SETUP_WAITING: SetupStatus = {
  id: "4e81",
  claimed: false,
  claimed_at: null,
  claimed_by_this_device: false,
};
