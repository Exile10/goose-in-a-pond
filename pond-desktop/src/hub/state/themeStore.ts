/**
 * themeStore.ts
 * useSyncExternalStore-based theme/accent/density store.
 * Persists to localStorage and applies directly to document.documentElement.
 */
import { useSyncExternalStore } from "react";

// ─── Types ────────────────────────────────────────────────────────────────────

export type ThemeChoice = "Light" | "Dark" | "Auto";
export type AccentName = "Purple" | "Blue" | "Teal" | "Coral" | "Magenta";
export type DensityChoice = "Comfortable" | "Compact";

export interface ThemeState {
  theme: ThemeChoice;
  accent: AccentName;
  density: DensityChoice;
  /** "light" | "dark" — Auto resolved by time of day */
  resolvedTheme: "light" | "dark";
}

// ─── Accent palettes ── exactly from Goose Hub.html line 27 ───────────────────

export const ACCENT_PALETTES: Record<AccentName, [string, string, string, string]> = {
  Purple:  ["#7C3AED", "#6D28D9", "#EDE9FE", "#F5F3FF"],
  Blue:    ["#2563EB", "#1D4ED8", "#DBEAFE", "#EFF6FF"],
  Teal:    ["#0D9488", "#0F766E", "#CCFBF1", "#F0FDFA"],
  Coral:   ["#F97316", "#EA580C", "#FFEDD5", "#FFF7ED"],
  Magenta: ["#DB2777", "#BE185D", "#FCE7F3", "#FDF2F8"],
};

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

function applyToDOM(theme: ThemeChoice, accent: AccentName, density: DensityChoice): void {
  const root = document.documentElement;
  const resolved = resolveTheme(theme);

  // theme dataset
  root.dataset.theme = resolved;

  // density dataset
  root.dataset.density = density.toLowerCase();

  // accent custom properties
  const [pp, pp600, pp100, pp50] = ACCENT_PALETTES[accent];
  root.style.setProperty("--pp",     pp);
  root.style.setProperty("--pp-600", pp600);
  root.style.setProperty("--pp-100", pp100);
  root.style.setProperty("--pp-50",  pp50);
}

// ─── Store internals ──────────────────────────────────────────────────────────

const initial = readFromStorage();

let _state: ThemeState = {
  ...initial,
  resolvedTheme: resolveTheme(initial.theme),
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
  applyToDOM(_state.theme, _state.accent, _state.density);
  _notify();
}

function setAccent(accent: AccentName): void {
  try { localStorage.setItem(KEY_ACCENT, accent); } catch { /* ignore */ }
  _state = { ..._state, accent };
  applyToDOM(_state.theme, _state.accent, _state.density);
  _notify();
}

function setDensity(density: DensityChoice): void {
  try { localStorage.setItem(KEY_DENSITY, density); } catch { /* ignore */ }
  _state = { ..._state, density };
  applyToDOM(_state.theme, _state.accent, _state.density);
  _notify();
}

// ─── Bootstrap: apply immediately on module load ──────────────────────────────

applyToDOM(_state.theme, _state.accent, _state.density);

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
