import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  ACCENTS,
  ACCENTS_DARK,
  createTheme,
  cssVariables,
  parseColor,
  ratio,
} from "@jarida/ink";
import { ACCENT_PALETTES } from "./hub/state/themeStore";

/**
 * The product, held to its own design library.
 *
 * `@jarida/ink` lives outside this repository so it can be published and used
 * by other Jarida products. That makes this file the only place the two halves
 * can be compared: the library has no idea this application exists, and a copy
 * of `design-tokens.css` vendored into the library would be a snapshot that
 * rots from the day it is taken.
 *
 * So the obligation runs this way round. A product that adopts the tokens owes
 * itself a test that it has not drifted from them, and it is the side that can
 * see both files.
 *
 * It also guards the alias. `vite.config.ts` and `vitest.config.ts` both resolve
 * `@jarida/ink` from a sibling checkout, and an alias nothing imports is an
 * alias that quietly stops resolving.
 */

const HERE = dirname(fileURLToPath(import.meta.url));
const TOKENS_CSS = resolve(HERE, "styles/design-tokens.css");

/**
 * Parse one custom-property block of the shipped stylesheet.
 *
 * Later declarations win, the way the cascade resolves them — `--bg-brand-soft`
 * is declared twice in that file and the second one is the value on screen.
 */
function parseBlock(css: string, selector: string): Record<string, string> {
  const start = css.indexOf(selector);
  expect(start, `${selector} is missing from design-tokens.css`).toBeGreaterThan(-1);
  const end = css.indexOf("\n}", start);
  const body = css.slice(start, end);
  const out: Record<string, string> = {};
  for (const line of body.split("\n")) {
    const match = line.match(/^\s*(--[a-z0-9-]+)\s*:\s*(.+?);\s*(?:\/\*.*)?$/i);
    if (match) out[match[1]] = match[2].trim();
  }
  return out;
}

/**
 * Resolve the indirections the stylesheet is written through.
 *
 * `--color-accent` is `var(--pp, #8C4BFF)`, and `--pp` is not in this file at
 * all — the theme store writes it as an inline style on `:root` before React
 * renders. So the value on screen is the store's, not the fallback's, and any
 * comparison that reads the fallback is comparing against a colour the app has
 * never shown. That gap is the whole reason this test exists.
 */
function resolver(vars: Record<string, string>, ramp: readonly string[]) {
  const runtime: Record<string, string> = {
    "--pp": ramp[0],
    "--pp-600": ramp[1],
    "--pp-100": ramp[2],
    "--pp-50": ramp[3],
  };

  function resolve(value: string, depth = 0): string {
    if (depth > 8) return value;
    let out = value;

    // color-mix(in srgb, <colour> <pct>%, transparent) — the only form used.
    const mix = out.match(/^color-mix\(in srgb,\s*(.+?)\s+(\d+)%,\s*transparent\)$/);
    if (mix) {
      const base = resolve(mix[1], depth + 1);
      const hex = base.replace("#", "");
      const rgb = [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16));
      return `rgba(${rgb[0]}, ${rgb[1]}, ${rgb[2]}, ${Number(mix[2]) / 100})`;
    }

    out = out.replace(/var\((--[a-z0-9-]+)(?:,\s*([^)]+))?\)/gi, (_all, name, fallback) => {
      const found = runtime[name] ?? vars[name];
      return resolve(found ?? fallback ?? "", depth + 1);
    });
    return out.trim();
  }

  return resolve;
}

const css = readFileSync(TOKENS_CSS, "utf8");

const light = parseBlock(css, ":root {");
const darkOverrides = parseBlock(css, ':root[data-theme="dark"] {');
// Dark only overrides. Anything it does not name is still whatever :root said.
const dark = { ...light, ...darkOverrides };

const resolveLight = resolver(light, ACCENTS.Purple);
const resolveDark = resolver(dark, ACCENTS_DARK.Purple);

/**
 * Compare colours as colours.
 *
 * The dark block aligns its columns — `rgba(52,  199,  89, 0.18)` — and the
 * library writes one space. Comparing those as strings is comparing
 * formatting, so anything that parses as a colour is parsed and written back
 * out canonically first. Everything else is compared verbatim, because a
 * length or a duration has no second spelling.
 */
function canonical(value: string): string {
  try {
    const { r, g, b, a } = parseColor(value);
    const round = (n: number) => Math.round(n);
    return a >= 1
      ? `#${[r, g, b].map((n) => round(n).toString(16).padStart(2, "0")).join("")}`
      : `rgba(${round(r)}, ${round(g)}, ${round(b)}, ${a})`;
  } catch {
    return value.toLowerCase();
  }
}

function drift(
  emitted: Record<string, string>,
  shipped: Record<string, string>,
  resolveValue: (value: string) => string,
  exempt: Record<string, string>,
): string[] {
  const mismatches: string[] = [];
  for (const [name, mine] of Object.entries(emitted)) {
    if (name in exempt) continue;
    const theirs = shipped[name];
    if (theirs === undefined) continue;
    const resolved = resolveValue(theirs);
    if (canonical(resolved) !== canonical(mine)) {
      mismatches.push(
        `${name}\n    shipped:  ${theirs}\n    resolved: ${resolved}\n    library:  ${mine}`,
      );
    }
  }
  return mismatches;
}

describe("light", () => {
  const emitted = cssVariables(createTheme());

  /**
   * No exemptions. The light theme is what runs in front of a household, and
   * DESIGN.md's precedence rule says the shipped system wins — so a library
   * that claims to be its source of truth has to reproduce it exactly, with no
   * list of things it decided to improve on the way.
   */
  it("reproduces the shipped stylesheet exactly", () => {
    const mismatches = drift(emitted, light, resolveLight, {});
    expect(
      mismatches,
      `The library and design-tokens.css have drifted:\n\n${mismatches.join("\n\n")}\n`,
    ).toEqual([]);
  });

  it("covers enough of it to be worth calling a source of truth", () => {
    const shared = Object.keys(emitted).filter((name) => name in light);
    // Sixty-odd tokens is every value a component here actually reads. The rest
    // of design-tokens.css is orb states, memory-segment hues and role colours,
    // which belong to the product rather than to a design library.
    expect(shared.length).toBeGreaterThan(60);
  });
});

describe("dark", () => {
  const emitted = cssVariables(createTheme({ scheme: "dark" }));

  /**
   * One exemption, and it is a fix rather than a preference — asserted below.
   */
  const DIVERGENCES: Record<string, string> = {
    "--border-ink": "the plate has to survive the theme; see the test below",
    "--shadow-ink": "composed from --border-ink",
    "--color-accent-line": "derived; the shipped file has no such token in dark",
    "--focus-ring": "derived from the accent line",
    "--bg-brand-soft": "a solid paper, not a tint of the accent",
    "--pp-50": "same",
  };

  it("reproduces the shipped dark overrides", () => {
    const mismatches = drift(emitted, dark, resolveDark, DIVERGENCES);
    expect(
      mismatches,
      `The library and the dark block have drifted:\n\n${mismatches.join("\n\n")}\n`,
    ).toEqual([]);
  });

  it("diverges on the plate because the shipped one is invisible", () => {
    // The stylesheet never overrides --border-ink for dark mode, so the plate
    // stays #5D23C2 on a #131119 page — 2.21:1, and 2.00:1 on the panel. That
    // is the one treatment the whole system is built around, rendered at a
    // contrast where it barely exists.
    const shippedPlate = resolveDark(dark["--border-ink"]);
    expect(shippedPlate.toLowerCase()).toBe("#5d23c2");
    expect(ratio(shippedPlate, "#131119")).toBeLessThan(3);
    expect(ratio(shippedPlate, "#1E1B26")).toBeLessThan(3);
    expect(ratio(createTheme({ scheme: "dark" }).color.ink, "#131119")).toBeGreaterThanOrEqual(3);
  });

  it("keeps no exemption that has stopped being one", () => {
    // A divergence entry that no longer diverges is a stale exemption, and a
    // stale exemption is how a real drift gets hidden later.
    for (const name of Object.keys(DIVERGENCES)) {
      const theirs = dark[name];
      if (theirs === undefined) continue;
      expect(
        canonical(resolveDark(theirs)),
        `${name} now matches — remove it from DIVERGENCES`,
      ).not.toBe(canonical(emitted[name]));
    }
  });
});

describe("the accent chain", () => {
  it("uses the palette the theme store actually writes at boot", () => {
    // The stylesheet's fallback is #8C4BFF, the mark colour. The store writes
    // #7C3AED. Reading the fallback would measure a colour the app never shows.
    for (const [name, ramp] of Object.entries(ACCENTS)) {
      expect(ACCENT_PALETTES[name as keyof typeof ACCENT_PALETTES], `${name} has drifted`).toEqual([
        ...ramp,
      ]);
    }
  });

  it("resolves the library through the alias", () => {
    expect(typeof createTheme).toBe("function");
  });
});
