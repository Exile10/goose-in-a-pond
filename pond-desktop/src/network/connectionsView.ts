import i18n from "../i18n";
import { isSignInRequired } from "../api/hostCredential";
import { signInRefusalKey } from "../api/signInRefusal";
import {
  ApiError,
  type BluetoothLink,
  type ConnectionErrorCode,
  type Connections,
  type EthernetLink,
  type SetupStatus,
  type WifiJoinResult,
  type WifiLink,
  type WifiSecurity,
} from "../api/types";

export type ConnectionAction = "read" | "scan" | "join" | "forget";

const CODES: readonly ConnectionErrorCode[] = [
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

/** The contract answers `{"error": "<code>"}`, which the client keeps as the message, not as `code`. */
export function connectionErrorCode(e: unknown): ConnectionErrorCode | null {
  if (!(e instanceof ApiError)) return null;
  const raw = e.code ?? e.message;
  return (CODES as readonly string[]).includes(raw) ? (raw as ConnectionErrorCode) : null;
}

/** No answer at all, as when a join moves the pond off the network this app reached it by. */
export function isConnectionLost(e: unknown): boolean {
  return !(e instanceof ApiError);
}

export interface ErrorContext {
  action: ConnectionAction;
  ssid?: string;
  /** The pond host's operating system, from `GET /system/info`. */
  platform?: string | null;
  /** `can_change.reason`, which says who may change the network better than a status code can. */
  changeReason?: string | null;
}

function isMac(platform: string | null | undefined): boolean {
  return /mac|darwin/i.test(platform ?? "");
}

const GRANT_FIX = "On the pond, run: bash scripts/giap.sh wifi-grant, then try again.";
const LOCATION_FIX =
  "Open System Settings, then Privacy & Security, then Location Services, and allow Goose In A Pond. Then try again.";

function notPermitted(platform: string | null | undefined): string {
  if (isMac(platform)) return `macOS has not let the pond use Wi-Fi. ${LOCATION_FIX}`;
  if (platform) return `The pond's user may not change Wi-Fi. ${GRANT_FIX}`;
  return `The pond's operating system refused. On Linux: ${GRANT_FIX} On a Mac: ${LOCATION_FIX}`;
}

export function connectionErrorText(e: unknown, ctx: ErrorContext): string {
  if (isSignInRequired(e)) return i18n.t(signInRefusalKey());

  const name = ctx.ssid ? `“${ctx.ssid}”` : "That network";
  switch (connectionErrorCode(e)) {
    case "not_allowed":
      return (
        ctx.changeReason ||
        "This device may not change the pond's network. Use the phone that set the pond up, or a household member's device."
      );
    case "not_permitted":
      return notPermitted(ctx.platform);
    case "unsupported":
      return "This pond cannot control Wi-Fi: it has no Wi-Fi hardware, or its operating system does not allow it. A cable still works.";
    case "busy":
      return "The pond is busy with another Wi-Fi scan or join. Wait a moment and try again.";
    case "in_use":
      return `The pond is on ${name} right now. Forgetting it could take the pond off your network.`;
    case "wrong_password":
      return `${name} did not accept that passphrase. Check it and try again.`;
    case "not_found":
      return ctx.action === "forget"
        ? `The pond has no saved network called ${name}. It may already have been forgotten.`
        : `The pond could not find ${name}. Move it closer to the router, or check the name, then find networks again.`;
    case "timeout":
      return `${name} did not answer in time. The pond may still be connecting, so check the status above in a moment.`;
    case "no_address":
      return `The pond joined ${name}, but the router gave it no address. Check that the router hands out addresses, then try again.`;
    case "invalid_request":
      return "That name or passphrase is not valid. A network name is 1 to 32 characters and a passphrase is 8 to 63.";
    case "unsupported_security":
      return `${name} is a work or school network. It needs a username and certificate, and the pond can only join with a passphrase.`;
  }

  if (e instanceof ApiError) {
    if (e.status === 408) {
      if (ctx.action === "scan") return "The scan took too long. Try again.";
      if (ctx.action === "join") {
        return "The pond did not answer within a minute. It may still be joining, so check the status above in a moment.";
      }
      return "The pond did not answer.";
    }
    return `The pond could not do that (${e.message || e.status}).`;
  }
  return ctx.action === "read"
    ? "The pond is not answering, so its connections are unknown."
    : "The pond is not answering.";
}

export const ENTERPRISE_NOTE =
  "Needs a username and certificate, which the pond cannot send. It can only join with a passphrase.";

const SECURITY_LABEL: Record<WifiSecurity, string> = {
  open: "Open",
  wep: "WEP",
  wpa: "WPA",
  wpa2: "WPA2",
  wpa3: "WPA3",
  enterprise: "Work or school",
};

export function securityLabel(security: WifiSecurity): string {
  return SECURITY_LABEL[security] ?? String(security);
}

/** The height of each bar carries the reading, not a hue. */
export function signalBars(signal: number | null): 0 | 1 | 2 | 3 | 4 {
  if (signal === null || !Number.isFinite(signal)) return 0;
  return Math.min(4, Math.max(0, Math.ceil(signal / 25))) as 0 | 1 | 2 | 3 | 4;
}

export function signalLabel(signal: number | null): string {
  return signal === null || !Number.isFinite(signal)
    ? "No signal reading"
    : `Signal ${Math.round(signal)} percent`;
}

export function isConnections(v: unknown): v is Connections {
  const c = v as Partial<Connections> | null | undefined;
  if (!c || typeof c !== "object") return false;
  const caps = c.wifi?.capabilities;
  return (
    typeof c.wifi?.state === "string" &&
    !!caps?.scan &&
    !!caps.join &&
    !!caps.forget &&
    Array.isArray(c.ethernet) &&
    typeof c.bluetooth?.state === "string" &&
    !!c.can_change &&
    typeof c.can_change.allowed === "boolean"
  );
}

export function isSetupStatus(v: unknown): v is SetupStatus {
  const s = v as Partial<SetupStatus> | null | undefined;
  return !!s && typeof s === "object" && typeof s.id === "string" && typeof s.claimed === "boolean";
}

export function endSentence(text: string): string {
  const t = text.trim();
  return /[.!?]$/.test(t) ? t : `${t}.`;
}

const CAPABILITY_NAME = {
  scan: "Finding networks",
  join: "Joining a network",
  forget: "Forgetting a network",
} as const;

function joinNames(names: string[]): string {
  const listed = names.map((n, i) => (i === 0 ? n : n.charAt(0).toLowerCase() + n.slice(1)));
  if (listed.length < 2) return listed.join("");
  return `${listed.slice(0, -1).join(", ")} and ${listed[listed.length - 1]}`;
}

export function capabilityNotices(wifi: WifiLink): string[] {
  const byReason = new Map<string, string[]>();
  for (const key of ["scan", "join", "forget"] as const) {
    const cap = wifi.capabilities[key];
    if (cap.available) continue;
    const reason = cap.reason?.trim() ?? "";
    byReason.set(reason, [...(byReason.get(reason) ?? []), CAPABILITY_NAME[key]]);
  }
  return [...byReason].map(([reason, names]) => {
    const subject = `${joinNames(names)} ${names.length > 1 ? "are" : "is"} unavailable`;
    return reason ? `${subject}: ${endSentence(reason)}` : `${subject}.`;
  });
}

export function unavailableReason(wifi: WifiLink): string {
  const given =
    wifi.reason?.trim() ||
    [wifi.capabilities.scan, wifi.capabilities.join, wifi.capabilities.forget]
      .map((c) => c.reason?.trim())
      .find(Boolean);
  return given
    ? endSentence(given)
    : "This pond has no Wi-Fi hardware, or its operating system does not allow controlling it. A cable still works.";
}

/** Joining another network takes the pond off this one, which may be how this app reaches it. */
export function joinLeavesNetwork(wifi: WifiLink, target: string): boolean {
  return wifi.state === "connected" && wifi.ssid !== null && wifi.ssid !== target;
}

/** The pond came up on a different network or address than it was on, so this reach may end. */
export function joinMoved(before: WifiLink, result: WifiJoinResult): boolean {
  return (
    before.state === "connected" &&
    (before.ssid !== result.ssid || (before.address !== null && before.address !== result.address))
  );
}

export function reachLine(result: WifiJoinResult): string {
  const host = result.hostname.includes(".") ? result.hostname : `${result.hostname}.local`;
  const port = result.https_port ? `, port ${result.https_port}` : "";
  return `Find it at ${host} or ${result.address}${port}.`;
}

/** A network name is up to 32 bytes, not 32 characters. */
export function ssidProblem(ssid: string): string | null {
  if (ssid.trim().length === 0) return "Enter the network's name.";
  if (new TextEncoder().encode(ssid).length > 32) return "A network name is at most 32 bytes. Check it.";
  return null;
}

/** `security` is null for a hidden network, where a passphrase may be absent (an open network). */
export function passphraseProblem(passphrase: string, security: WifiSecurity | null): string | null {
  if (security === "open") return null;
  if (security === null) {
    return passphrase.length > 64 ? "A passphrase is at most 64 characters." : null;
  }
  if (passphrase.length === 0) return "Enter the passphrase.";
  if (security === "wep") return null;
  const hexKey = /^[0-9a-fA-F]{64}$/.test(passphrase);
  if (!hexKey && (passphrase.length < 8 || passphrase.length > 63)) {
    return "A passphrase is 8 to 63 characters.";
  }
  return null;
}

export interface LinkLine {
  headline: string;
  detail: string;
  /** Something is wrong or missing; said in words and by an icon, never by a hue alone. */
  problem: boolean;
}

export function describeBluetooth(bt: BluetoothLink): LinkLine {
  switch (bt.state) {
    case "advertising":
      return {
        headline: "Visible to phones",
        detail: bt.advertising_as
          ? `Phones nearby can find this pond as “${bt.advertising_as}”.`
          : "Phones nearby can find this pond.",
        problem: false,
      };
    case "serving":
      return {
        headline: "Ready",
        detail:
          "Ready, but not showing itself. The pond advertises over Bluetooth only while it is unclaimed or offline.",
        problem: false,
      };
    case "off":
      return {
        headline: "Off",
        detail: bt.enabled
          ? "Bluetooth is off on this machine, so phones cannot find the pond this way."
          : "Setup over Bluetooth is switched off, so phones cannot find the pond this way.",
        problem: false,
      };
    case "unavailable":
      return {
        headline: "Unavailable",
        detail: bt.reason?.trim()
          ? `Bluetooth is not available here: ${endSentence(bt.reason)}`
          : "Bluetooth is not available on this machine.",
        problem: true,
      };
  }
  return { headline: "Unknown", detail: "The pond reported a Bluetooth state this app does not know.", problem: true };
}

export function claimedDate(iso: string | null): string | null {
  if (!iso) return null;
  const at = new Date(iso);
  return Number.isNaN(at.getTime())
    ? null
    : at.toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
}

export function describeSetup(setup: SetupStatus): LinkLine {
  if (!setup.claimed) {
    return {
      headline: "Waiting",
      detail: `Waiting to be claimed (card ${setup.id})`,
      problem: false,
    };
  }
  const date = claimedDate(setup.claimed_at);
  return { headline: "Claimed", detail: date ? `Claimed on ${date}` : "Claimed", problem: false };
}

export function speedLabel(mbps: number | null): string | null {
  if (mbps === null || !Number.isFinite(mbps) || mbps <= 0) return null;
  return mbps >= 1000 ? `${mbps / 1000} Gbps` : `${mbps} Mbps`;
}

export function describeEthernet(link: EthernetLink): {
  state: string;
  address: string | null;
  speed: string | null;
} {
  const up = link.link === "up";
  return {
    state: up ? "Connected" : "No link",
    address: link.address ?? (up ? "No address yet" : null),
    speed: up ? speedLabel(link.speed_mbps) : null,
  };
}
