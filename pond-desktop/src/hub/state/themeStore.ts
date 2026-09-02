/**
 * themeStore.ts
 *
 * The household's three choices — theme, accent, density — held in a
 * useSyncExternalStore, persisted to localStorage, and applied straight to
 * `document.documentElement`.
 *
 * Since the Ink migration this store is also the **single writer of design
 * tokens for the whole application**. It used to set four custom properties
 * (`--pp`, `--pp-600`, `--pp-100`, `--pp-50`) and leave the other eighty to
 * `design-tokens.css`; now it writes the complete set that `@jarida/ink`
 * resolves, so every screen — classic, hub, canvas, voice — is rendering from
 * the library whether or not it imports a single component from it.
 *
 * Three things follow from that, and each is the reason it is done here rather
 * than in a React provider:
 *
 * - **No flash.** This module is imported by `themeBootstrap` before React
 *   renders, so the tokens are on `<html>` before the first paint. A provider
 *   writing them in an effect would repaint the app one frame in.
 * - **One writer.** `InkProvider` runs in `context` mode in `App.tsx`, supplying
 *   the same theme object to Ink components without touching the DOM. Two
 *   writers of the same custom properties is how they drift.
 * - **It is additive.** Inline properties on `<html>` beat `:root` rules, so the
 *   library's values win for the tokens it defines and `design-tokens.css` still
 *   governs everything else — the grey ramp, the orb states, the role colours.
 *   `src/ink-adoption.test.ts` asserts the two agree token for token.
 */
import { useSyncExternalStore } from "react";
import {
  ACCENTS,
  createTheme,
  cssVariables,
  type AccentName,
  type InkTheme,
} from "@jarida/ink";

// ─── Types ────────────────────────────────────────────────────────────────────

export type ThemeChoice = "Light" | "Dark" | "Auto";
export type DensityChoice = "Comfortable" | "Compact";

/** Re-exported from the library, which now owns the union. */
export type { AccentName };

export interface ThemeState {
  theme: ThemeChoice;
  accent: AccentName;
  density: DensityChoice;
  /** "light" | "dark" — Auto resolved by time of day */
  resolvedTheme: "light" | "dark";
  /**
   * The resolved theme object, for `InkProvider` and any component that needs a
   * token in JavaScript rather than CSS. Built here so the DOM and the React
   * context can never be describing two different themes.
   */
  ink: InkTheme;
}

// ─── Accent palettes ──────────────────────────────────────────────────────────

/**
 * One source, in the library.
 *
 * These five ramps used to be declared here and again — as a near-copy, with a
 * separate dark variant — in whatever else needed them. `@jarida/ink` owns them
 * now; this is a re-export so existing imports keep working.
 *
 * Each is `[base, pressed, tint, paper]`. Note that Purple's base is #7C3AED and
 * not the #8C4BFF of the mark: the running accent and the logo colour are
 * different values, and collapsing them either dulls the logo or fails the text.
 */
export const ACCENT_PALETTES = ACCENTS;

// ─── localStorage keys ────────────────────────────────────────────────────────

const KEY_THEME   = "goosehub_theme";
const KEY_ACCENT  = "goosehub_accent";
const KEY_DENSITY = "goosehub_density";

// ─── Helpers ──────────────────────────────────────────────────────────────────

function resolveTheme(theme: ThemeChoice): "light" | "dark" {
  if (theme === "Dark") return "dark";
  if (theme === "Light") return "light";
  // Auto: dark between 19:00 and 05:59
  const h = new Date().getHours();
  return h >= 19 || h < 6 ? "dark" : "light";
}

function isThemeChoice(v: unknown): v is ThemeChoice {
  return v === "Light" || v === "Dark" || v === "Auto";
}

function isAccentName(v: unknown): v is AccentName {
  return v === "Purple" || v === "Blue" || v === "Teal" || v === "Coral" || v === "Magenta";
}

function isDensityChoice(v: unknown): v is DensityChoice {
  return v === "Comfortable" || v === "Compact";
}

function readFromStorage(): { theme: ThemeChoice; accent: AccentName; density: DensityChoice } {
  try {
    const t = localStorage.getItem(KEY_THEME);
    const a = localStorage.getItem(KEY_ACCENT);
    const d = localStorage.getItem(KEY_DENSITY);
    return {
      theme:   isThemeChoice(t)   ? t : "Light",
      accent:  isAccentName(a)    ? a : "Purple",
      density: isDensityChoice(d) ? d : "Comfortable",
    };
  } catch {
    return { theme: "Light", accent: "Purple", density: "Comfortable" };
  }
}

function applyToDOM(theme: ThemeChoice, accent: AccentName, density: DensityChoice): InkTheme {
  const resolved = resolveTheme(theme);
  const ink = createTheme({
    scheme: resolved,
    accent,
    density: density === "Compact" ? "compact" : "comfortable",
  });

  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.dataset.density = density.toLowerCase();

  // The whole set, inline, before first paint. Includes --pp/--pp-600/--pp-100/
  // --pp-50, which is what keeps every `var(--pp, …)` already written across
  // ten stylesheets pointing at the same values it always did.
  for (const [name, value] of Object.entries(cssVariables(ink))) {
    root.style.setProperty(name, value);
  }

  return ink;
}

// ─── Store internals ──────────────────────────────────────────────────────────

const initial = readFromStorage();

let _state: ThemeState = {
  ...initial,
  resolvedTheme: resolveTheme(initial.theme),
  ink: createTheme({
    scheme: resolveTheme(initial.theme),
    accent: initial.accent,
    density: initial.density === "Compact" ? "compact" : "comfortable",
  }),
};

const _listeners = new Set<() => void>();

function _notify(): void {
  _listeners.forEach((fn) => fn());
}

function _snapshot(): ThemeState {
  return _state;
}

// ─── Public setters ───────────────────────────────────────────────────────────

function setTheme(theme: ThemeChoice): void {
  try { localStorage.setItem(KEY_THEME, theme); } catch { /* ignore */ }
  _state = { ..._state, theme, resolvedTheme: resolveTheme(theme) };
  _state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };
  _notify();
}

function setAccent(accent: AccentName): void {
  try { localStorage.setItem(KEY_ACCENT, accent); } catch { /* ignore */ }
  _state = { ..._state, accent };
  _state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };
  _notify();
}

function setDensity(density: DensityChoice): void {
  try { localStorage.setItem(KEY_DENSITY, density); } catch { /* ignore */ }
  _state = { ..._state, density };
  _state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };
  _notify();
}

// ─── Bootstrap: apply immediately on module load ──────────────────────────────

_state = { ..._state, ink: applyToDOM(_state.theme, _state.accent, _state.density) };

// ─── useSyncExternalStore subscription ───────────────────────────────────────

function _subscribe(listener: () => void): () => void {
  _listeners.add(listener);
  return () => _listeners.delete(listener);
}

// ─── Hook ─────────────────────────────────────────────────────────────────────

export function useTheme(): ThemeState & {
  setTheme: (t: ThemeChoice) => void;
  setAccent: (a: AccentName) => void;
  setDensity: (d: DensityChoice) => void;
} {
  const state = useSyncExternalStore(_subscribe, _snapshot, _snapshot);
  return { ...state, setTheme, setAccent, setDensity };
}
