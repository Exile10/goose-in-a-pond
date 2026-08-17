// ─── Settings draft state ─────────────────────────────────────────────────
//
// The three rules for holding an edit-in-progress against a server that may
// answer at any moment. Extracted from `sections/Settings.tsx` when the
// catalogue became the settings view: they are the state model, not part of any
// one screen, and the screen they lived in is gone.

import type { Settings as SettingsType } from "../api/types";

/**
 * Deep value equality for settings values.
 *
 * Compare by VALUE, not identity: several fields are arrays or maps
 * (`voice_wake_word_transcriptions`, `retention_events_by_category`) that the
 * UI replaces wholesale, so a reference check would report every one of them
 * as edited on every Save. Object key order is not significant (the server
 * serialises a `HashMap`); array order is. Settings are plain JSON, so no
 * cycle handling is needed.
 */
export function settingsValueEquals(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  const aIsArray = Array.isArray(a);
  if (aIsArray !== Array.isArray(b)) return false;
  if (aIsArray) {
    const av = a as unknown[];
    const bv = b as unknown[];
    return av.length === bv.length && av.every((v, i) => settingsValueEquals(v, bv[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const aKeys = Object.keys(ao);
  if (aKeys.length !== Object.keys(bo).length) return false;
  return aKeys.every(
    (k) => Object.prototype.hasOwnProperty.call(bo, k) && settingsValueEquals(ao[k], bo[k]),
  );
}

/**
 * The keys of `current` whose value differs from `baseline` — the last state
 * the server told us about. Keys the panel never touched are omitted, so an
 * untouched field is neither re-sent nor marked as chosen.
 *
 * A key present in `baseline` but not in `current` is NOT reported: the
 * endpoint is a patch and has no way to express a deletion.
 */
export function diffSettings(
  baseline: Partial<SettingsType>,
  current: Partial<SettingsType>,
): Partial<SettingsType> {
  const out: Record<string, unknown> = {};
  const base = baseline as Record<string, unknown>;
  for (const [key, value] of Object.entries(current)) {
    if (!settingsValueEquals(value, base[key])) out[key] = value;
  }
  return out as Partial<SettingsType>;
}

/**
 * Fold a fresh server snapshot into local state without discarding edits the
 * user has made but not yet saved: a key they have not touched (local still
 * equals baseline) takes the server's value, a key they have touched keeps
 * theirs. Without this, a background refresh would silently revert in-progress
 * typing, and — worse — would leave the baseline disagreeing with the values on
 * screen, so the next Save would re-send fields nobody edited.
 */
export function foldServerState(
  prev: Partial<SettingsType>,
  baseline: Partial<SettingsType>,
  server: Partial<SettingsType>,
): Partial<SettingsType> {
  const next = { ...prev } as Record<string, unknown>;
  const prevRec = prev as Record<string, unknown>;
  const baseRec = baseline as Record<string, unknown>;
  for (const [key, value] of Object.entries(server)) {
    if (settingsValueEquals(prevRec[key], baseRec[key])) next[key] = value;
  }
  return next as Partial<SettingsType>;
}
