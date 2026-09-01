import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const BASE_CSS = join(dirname(fileURLToPath(import.meta.url)), "base.css");

/**
 * The `!important` focus-visible block, as source text.
 *
 * A structural guard rather than a rendered one: jsdom does not apply an external
 * stylesheet's cascade, so there is nothing to measure. The same shape as the
 * no-emoji lint beside it -- read the source, assert the property.
 */
function importantFocusSelectors(): string[] {
  // Comments first: this rule carries a long one, and a selector list read straight
  // out of the file would arrive with the last comment line glued to it.
  const css = readFileSync(BASE_CSS, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const match = css.match(
    /([^};]*)\{[^}]*outline:\s*2px solid var\(--focus-ring\) !important[^}]*\}/,
  );
  if (match === null) throw new Error("the !important focus-visible rule is gone from base.css");
  return match[1]
    .split(",")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

describe("the focus ring", () => {
  it("does not draw a second ring around a text field", () => {
    // The bug this exists for: the Register-device dialog's Setup code input showed
    // TWO concentric accent borders. Text fields already have a focus treatment --
    // an accent border plus a 3px halo -- and this rule added an outline 2px outside
    // it. `autoFocus` on that field is why it was the one that showed it worst.
    const selectors = importantFocusSelectors();

    for (const element of ["input", "textarea", "select"]) {
      expect(
        selectors.some((s) => s.startsWith(`${element}:`)),
        `${element} is back in the !important focus-visible rule, which draws a ` +
          `second ring outside the border and halo it already has`,
      ).toBe(false);
    }
  });

  it("still guarantees a ring for everything that has no other indicator", () => {
    // The safety net's whole point: several component stylesheets set
    // `outline: none` on custom controls with no replacement. Removing text fields
    // must not quietly remove the protection from anything else.
    const selectors = importantFocusSelectors();

    for (const element of ["button", "a", '[role="button"]', "[tabindex]"]) {
      expect(selectors).toContain(`${element}:focus-visible`);
    }
  });

  it("leaves text fields a focus indicator of their own", () => {
    // Not a second ring, but not zero rings either. `:focus`, not `:focus-visible`,
    // because a text field earns an indicator on any focus -- including the
    // programmatic `autoFocus` that opens the dialog.
    const css = readFileSync(BASE_CSS, "utf8");
    const rule = css.match(/input:focus,\s*textarea:focus,\s*select:focus\s*\{([^}]*)\}/);

    expect(rule, "text fields have no :focus rule at all").not.toBeNull();
    expect(rule?.[1]).toContain("border-color: var(--color-accent)");
    expect(rule?.[1]).toContain("box-shadow: 0 0 0 3px var(--color-accent-subtle)");
  });
});
