// ────────────────────────────────────────────────────────────
// One plain sentence about this house, right now.
//
// It replaces "Nothing needs you right now." — true, but true of any house on
// any day, which makes it wallpaper. A household glancing at a panel across a
// room wants to know something they did not already know, and the pond knows
// several such things: which lights are on, whether the doors are locked, what
// the sky is doing, what time it is where they are.
//
// Deliberately ONE sentence and deliberately plain. This sits where a
// suggestion would, on a screen whose whole job is to be glanced at, so it
// reports and never asks. No counts dressed up as insight, no "you have 3
// items", no exclamation.
//
// Everything here is derived from data the pond actually holds. Nothing is
// inferred about mood, habit or intent — DESIGN.md §3, "never invent meaning
// the data lacks". If the house has nothing to say, the line says something
// true about the hour instead of manufacturing significance.
// ────────────────────────────────────────────────────────────

import type { DeviceData, WeatherData } from "../data/mockHome";

export interface HomeLineInput {
  user: string;
  devices: DeviceData[];
  weather: WeatherData;
  now: Date;
}

/** Lights that count as on. `on` is undefined for devices that do not report it. */
function litLights(devices: DeviceData[]): DeviceData[] {
  return devices.filter((d) => d.kind === "light" && d.on === true);
}

function locks(devices: DeviceData[]): { total: number; locked: number } {
  const all = devices.filter((d) => d.kind === "lock");
  return { total: all.length, locked: all.filter((d) => d.locked === true).length };
}

/**
 * Join names the way a person would say them.
 *
 * Two get "and"; three or more get the first two and a count, because reading
 * six device names aloud off a panel is a list, not a sentence.
 */
function names(devices: DeviceData[]): string {
  const n = devices.map((d) => d.name);
  if (n.length === 1) return n[0];
  if (n.length === 2) return `${n[0]} and ${n[1]}`;
  return `${n[0]}, ${n[1]} and ${n.length - 2} more`;
}

/**
 * The sentence.
 *
 * Ordered by what a household would want to be told first. A door that is
 * unlocked at night outranks a light that is on, and a light that is on
 * outranks the weather — the weather is already in the header, so it only
 * speaks when nothing else has anything to say.
 */
export function homeLine({ user, devices, weather, now }: HomeLineInput): string {
  const hour = now.getHours();
  const evening = hour >= 19 || hour < 6;
  const { total: lockTotal, locked } = locks(devices);
  const lit = litLights(devices);

  // 1. An unlocked door after dark. The one thing worth interrupting for, and
  //    still phrased as a report — the household can see the locks on screen.
  if (evening && lockTotal > 0 && locked < lockTotal) {
    const open = lockTotal - locked;
    return open === lockTotal
      ? "Nothing is locked yet tonight."
      : `${open} of ${lockTotal} doors are still unlocked.`;
  }

  // 2. Everything shut, after dark. The good outcome, said once.
  if (evening && lockTotal > 0 && locked === lockTotal && lit.length === 0) {
    return "All locked, and everything is off.";
  }

  // 3. What is on. The most common useful thing, and the one a person is most
  //    likely to act on from across a room.
  if (lit.length > 0) {
    return lit.length === 1
      ? `${names(lit)} is on.`
      : `${lit.length} lights are on — ${names(lit)}.`;
  }

  // 4. Nothing is on, in the daytime.
  if (devices.length > 0) {
    return lockTotal > 0 && locked === lockTotal
      ? "Everything is off, and the doors are locked."
      : "Everything is off.";
  }

  // 5. No devices at all. The pond still knows the sky and the hour, and a
  //    household that has not added anything yet is exactly who should not be
  //    told their house is empty.
  return skyLine(weather, hour, user);
}

/**
 * The fallback, for a pond with no devices in it.
 *
 * Uses the weather and the hour because those are true without a single device
 * paired. It is the first thing a new household sees on this screen, so it
 * reports something real rather than apologising for being empty.
 */
function skyLine(weather: WeatherData, hour: number, user: string): string {
  const cond = (weather.cond || "").toLowerCase();
  const wet = /rain|drizzle|shower|storm/.test(cond);
  const clear = /clear|sun/.test(cond);

  if (hour < 6) return `It is ${weather.temp}° out, ${user}. The house is quiet.`;
  if (wet) return `${weather.cond} out, and ${weather.temp}°.`;
  if (clear && hour < 12) return `Clear and ${weather.temp}° this morning.`;
  if (clear) return `Clear and ${weather.temp}° out.`;
  return `${weather.cond}, ${weather.temp}° out.`;
}
