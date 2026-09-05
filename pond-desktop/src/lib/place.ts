// ─── Where and when this pond is — one module, one answer ────────────────────
//
// There were three time-zone lists in this app and no two of them agreed:
// 16 zones in `settings/catalogue.ts`, 18 in `sections/Schedules.tsx`, 13 in
// `components/onboarding/onboarding.constants.ts`. A household in Kampala
// could not pick its own zone from any of them, and a schedule offered a zone
// that Settings would not show.
//
// And there were two detections, neither of which worked. Onboarding's split
// the zone string on "/", waited 400ms so it looked busy, and produced NO
// COORDINATES — then switched weather on, so setup finished with a forecast
// that could never be fetched. Settings' asked `navigator.geolocation`, which
// a Tauri webview does not reliably answer.
//
// So: the catalogue comes from the server, which reads the IANA database (597
// zones, not 16), and detection is one POST that runs the same cascade for
// both screens.

import { api } from "../api/PondApiClient";
import type { DetectedPlace, PlaceSource, ZoneChoice } from "../api/types";

// Re-exported so a caller needs one import for the whole subject.
export type { DetectedPlace, PlaceSource, ZoneChoice };

/**
 * The last resort, and deliberately tiny.
 *
 * Not a fourth curated list: it is what a picker shows when the server is
 * unreachable AND this webview is too old for `Intl.supportedValuesOf`. Both
 * of those are already broken situations, and the honest response is a handful
 * of zones plus whatever the device itself reports — not a pretend catalogue.
 */
const LAST_RESORT = ["UTC", "Africa/Nairobi", "Europe/London", "America/New_York"];

let cache: ZoneChoice[] | null = null;

/** Offset for a zone today, computed locally. Used to fill in local fallbacks. */
function localOffset(zone: string): string {
  try {
    const parts = new Intl.DateTimeFormat("en", {
      timeZone: zone,
      timeZoneName: "longOffset",
    }).formatToParts(new Date());
    const name = parts.find((p) => p.type === "timeZoneName")?.value ?? "";
    // "GMT+03:00" → "+03:00"; plain "GMT" means UTC.
    const m = name.match(/([+-]\d{2}:\d{2})$/);
    return m ? m[1] : "+00:00";
  } catch {
    return "+00:00";
  }
}

/** The place a zone name implies: `Africa/Nairobi` → `Nairobi`. */
export function placeFromZone(zone: string): string {
  if (!zone.includes("/")) return "";
  return zone.split("/").pop()?.replace(/_/g, " ") ?? "";
}

/** This device's own zone. Local, instant, no network. */
export function deviceZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

/** Build choices from bare zone names, resolving each offset on this device. */
function decorate(zones: string[]): ZoneChoice[] {
  return zones.map((zone) => ({
    zone,
    offset: localOffset(zone),
    place: placeFromZone(zone),
  }));
}

/**
 * Every zone, best source available.
 *
 * Server first (the IANA database, and the same list the server validates
 * against — so a picker can never offer a zone that saving would reject), then
 * this webview's own `Intl` catalogue, then [`LAST_RESORT`]. Cached: it is a
 * few hundred strings that change when the tzdata does, not per render.
 */
export async function allZones(): Promise<ZoneChoice[]> {
  if (cache) return cache;
  try {
    const res = await api.listTimeZones();
    if (res?.zones?.length) {
      cache = res.zones;
      return cache;
    }
  } catch {
    // Fall through — an offline pond still deserves a working picker.
  }
  try {
    const supported = (Intl as unknown as {
      supportedValuesOf?: (k: string) => string[];
    }).supportedValuesOf?.("timeZone");
    if (supported?.length) {
      cache = decorate(supported);
      return cache;
    }
  } catch {
    /* older webview */
  }
  // Always include the device's own zone, or the one household most likely to
  // be affected by this fallback cannot select where it actually is.
  const zone = deviceZone();
  const zones = LAST_RESORT.includes(zone) ? LAST_RESORT : [zone, ...LAST_RESORT];
  cache = decorate(zones);
  return cache;
}

/**
 * Ask this device where it is, without prompting anybody.
 *
 * Only called when a browser really implements geolocation AND the household
 * pressed the button — it is offered as an upgrade to the answer, never as the
 * thing that gates it, because in a Tauri webview it usually never resolves.
 * A refusal is not an error here: the cascade has other sources.
 */
function deviceCoords(timeoutMs = 6000): Promise<{ latitude: number; longitude: number } | null> {
  return new Promise((resolve) => {
    if (typeof navigator === "undefined" || !navigator.geolocation) {
      resolve(null);
      return;
    }
    let settled = false;
    const done = (v: { latitude: number; longitude: number } | null) => {
      if (!settled) {
        settled = true;
        resolve(v);
      }
    };
    // Its own timer as well as the option: a webview that silently never calls
    // either callback would otherwise hang the button forever.
    const timer = setTimeout(() => done(null), timeoutMs);
    navigator.geolocation.getCurrentPosition(
      (p) => {
        clearTimeout(timer);
        done({
          // Four decimals is ~11m — finer than weather needs, and it keeps the
          // stored value from reading like a tracking coordinate.
          latitude: Number(p.coords.latitude.toFixed(4)),
          longitude: Number(p.coords.longitude.toFixed(4)),
        });
      },
      () => {
        clearTimeout(timer);
        done(null);
      },
      { timeout: timeoutMs, maximumAge: 600_000, enableHighAccuracy: false },
    );
  });
}

/**
 * Work out where this pond is. The one detection, for every screen.
 *
 * `typedName` is whatever the household has already put in the box; it beats
 * anything derived. The device's own zone always goes along, because it is the
 * one source that is free, private and always available.
 */
export async function detectPlace(typedName?: string): Promise<DetectedPlace> {
  const coords = await deviceCoords();
  return api.detectLocation({
    system_zone: deviceZone(),
    typed_name: typedName?.trim() || undefined,
    latitude: coords?.latitude,
    longitude: coords?.longitude,
  });
}

/** Reset the cached catalogue. Tests only. */
export function __resetZoneCache() {
  cache = null;
}
