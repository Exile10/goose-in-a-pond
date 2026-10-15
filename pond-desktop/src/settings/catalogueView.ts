import type { RetitleResult } from "../api/types";

// The settings page's conversions between stored values and what its text boxes and notes say,
// kept out of SettingsCatalogue.tsx so they can be tested.

/** A stored value as text; the one map (`retention_events_by_category`) shows as `network 14, sensor 7`. */
export function textValue(v: unknown): string {
  if (v == null) return "";
  if (Array.isArray(v)) return v.join(", ");
  if (typeof v === "object") {
    return Object.entries(v as Record<string, number>)
      .map(([k, n]) => `${k} ${n}`)
      .join(", ");
  }
  return String(v);
}

const NULLABLE_TEXT = new Set([
  "custom_system_prompt",
  "searxng_url",
]);

/** List-valued text boxes, typed comma-separated; one left out is sent raw and 422s the whole Save. */
const LIST_TEXT = new Set(["voice_wake_word_transcriptions", "suggestions_muted"]);

/** Inverse of `textValue`. Returns the shape the server expects for this key. */
export function parseText(key: string, raw: string): unknown {
  if (LIST_TEXT.has(key)) {
    return raw
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  }
  if (key === "retention_events_by_category") {
    const out: Record<string, number> = {};
    for (const part of raw.split(",")) {
      const [name, days] = part.trim().split(/\s+/);
      const n = Number(days);
      if (name && Number.isFinite(n)) out[name] = n;
    }
    return out;
  }
  // Emptied means unset: both are `Option<String>` server-side.
  if (raw === "" && NULLABLE_TEXT.has(key)) return null;
  return raw;
}

/** Terse retitle summary that tells "nothing needed doing" from "nothing was allowed" (all hand-named). */
export function summariseRetitle(r: RetitleResult): string {
  if (r.started) return "Renaming — names appear as it goes";
  // No titling loop is not a failure, so don't say "it did not work".
  return r.reason ?? "Nothing here to run";
}
