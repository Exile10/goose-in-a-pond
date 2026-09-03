// The sky is legible, and stays legible.
//
// The weather card is the one place in this interface where colour carries
// meaning rather than marking state, and it carries white text over four
// gradients and five conditions. That is exactly the arrangement where a
// contrast floor gets asserted in a comment and then quietly stops being true.
//
// So it is asserted here instead. The scrim alpha in `dashboard-grid.css` is
// DERIVED — 0.45 is the smallest flat value at which every stop of every sky
// clears 4.5:1 against white — and this file recomputes that rather than
// trusting it. Change a sky and this test tells you what it cost.

import { describe, expect, it } from "vitest";
import { skyConditionFor } from "./WeatherWidget";

/** Relative luminance, WCAG 2.1. */
function luminance([r, g, b]: number[]): number {
  const [R, G, B] = [r, g, b].map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * R + 0.7152 * G + 0.0722 * B;
}

function contrast(a: number[], b: number[]): number {
  const [l1, l2] = [luminance(a), luminance(b)];
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

/** Composite `fg` at `alpha` over `bg`. */
function over(fg: number[], alpha: number, bg: number[]): number[] {
  return bg.map((c, i) => fg[i] * alpha + c * (1 - alpha));
}

const WHITE = [255, 255, 255];
/** `rgba(9, 12, 24, 0.45)` from `.wx::before`. */
const SCRIM = [9, 12, 24];
const SCRIM_ALPHA = 0.45;

/** Every gradient stop of the four skies, from `hub.css`. */
const SKIES: Record<string, number[][]> = {
  day: [
    [96, 165, 250],
    [59, 130, 246],
    [129, 140, 248],
  ],
  dawn: [
    [253, 186, 116],
    [251, 146, 60],
    [129, 140, 248],
  ],
  dusk: [
    [251, 113, 133],
    [192, 38, 211],
    [76, 29, 149],
  ],
  night: [
    [30, 41, 59],
    [15, 23, 42],
    [49, 46, 129],
  ],
};

/** `wx--sky-snow` brightens by 1.04 — the one condition that lightens a sky. */
const SNOW_BRIGHTNESS = 1.04;

describe("white text on every sky", () => {
  it.each(Object.entries(SKIES))("clears 4.5:1 across %s", (_name, stops) => {
    for (const stop of stops) {
      expect(contrast(WHITE, over(SCRIM, SCRIM_ALPHA, stop))).toBeGreaterThanOrEqual(4.5);
    }
  });

  /**
   * Snow is the case the first scrim missed. It brightens rather than darkens,
   * so it pushes a sky toward the colour of the text sitting on it.
   */
  it("clears 4.5:1 on a snow-brightened sky too", () => {
    for (const stops of Object.values(SKIES)) {
      for (const stop of stops) {
        const lit = stop.map((c) => Math.min(255, c * SNOW_BRIGHTNESS));
        expect(contrast(WHITE, over(SCRIM, SCRIM_ALPHA, lit))).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  /**
   * The alpha is the smallest that works, not a comfortable one. If a lighter
   * scrim would do, the cards are darker than they need to be — and if this
   * starts failing, a sky has been added that the derivation never saw.
   */
  it("is no more opaque than it has to be", () => {
    const lighter = SCRIM_ALPHA - 0.05;
    const anyFails = Object.values(SKIES)
      .flat()
      .some((stop) => contrast(WHITE, over(SCRIM, lighter, stop)) < 4.5);
    expect(anyFails).toBe(true);
  });
});

describe("reading the condition", () => {
  /**
   * The bug this pins: the icon vocabulary contains `cloudSun`, and testing for
   * sun before cloud made "Partly cloudy" — the commonest sky there is — render
   * as a cloudless noon.
   */
  it("calls partly cloudy a cloud, not a clear sky", () => {
    expect(skyConditionFor("cloudSun", "Partly cloudy")).toBe("cloud");
  });

  it("still recognises an actually clear sky", () => {
    expect(skyConditionFor("sun", "Clear")).toBe("clear");
    expect(skyConditionFor("sun", "Sunny")).toBe("clear");
  });

  it("puts the wet and violent skies first", () => {
    expect(skyConditionFor("rain", "Light rain")).toBe("rain");
    expect(skyConditionFor("snow", "Snow showers")).toBe("snow");
    // A thunderstorm is also rain; it should read as the more specific one.
    expect(skyConditionFor("storm", "Thunderstorm with rain")).toBe("storm");
  });

  /** An unknown sky claims the least, rather than guessing at sunshine. */
  it("falls back to cloud", () => {
    expect(skyConditionFor("", "")).toBe("cloud");
    expect(skyConditionFor("wat", "Something new")).toBe("cloud");
  });
});
