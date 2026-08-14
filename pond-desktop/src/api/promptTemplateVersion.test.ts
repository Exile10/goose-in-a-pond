import { describe, it, expect } from "vitest";
import {
  promptTemplateIsOutdated,
  PROMPT_FACTORY_VERSION,
  type PromptTemplate,
} from "./types";

/**
 * The notice offers an update; it never takes one.
 *
 * The tempting alternative was to clear `is_customized` where the stored content
 * still matched an old factory string, adopting the row silently. That is the
 * settings adoption (migration 0035) run backwards: it moves a value only where
 * the value equals the old default AND `is_user_set = 0`, because "a row on its
 * own proves nothing". `is_customized` is this table's `is_user_set` — written
 * by exactly one thing, an explicit Save — so clearing it discards the only
 * honest record that somebody chose this text.
 */
const base: PromptTemplate = {
  name: "balanced",
  content: "…",
  is_system: true,
  is_customized: true,
  factory_version: PROMPT_FACTORY_VERSION - 1,
};

describe("promptTemplateIsOutdated", () => {
  it("flags an edit made against an older built-in", () => {
    expect(promptTemplateIsOutdated(base)).toBe(true);
  });

  it("says nothing about a row the reseed still owns", () => {
    // Untouched installs get every new generation automatically. Showing them a
    // notice would make it meaningless everywhere else.
    expect(
      promptTemplateIsOutdated({ ...base, is_customized: false }),
    ).toBe(false);
  });

  it("says nothing once the edit is current", () => {
    expect(
      promptTemplateIsOutdated({
        ...base,
        factory_version: PROMPT_FACTORY_VERSION,
      }),
    ).toBe(false);
  });

  it("treats a pre-column row as outdated rather than current", () => {
    // Migration 0048 defaults existing rows to 0 and deliberately does NOT
    // backfill: those rows genuinely predate the rewrite, and claiming
    // otherwise would suppress the one notice they need.
    const { factory_version: _omitted, ...withoutVersion } = base;
    expect(promptTemplateIsOutdated(withoutVersion)).toBe(true);
  });

  it("leaves user-created templates alone", () => {
    // A template the user wrote from scratch has no factory to be behind.
    expect(promptTemplateIsOutdated({ ...base, is_system: false })).toBe(false);
  });

  it("keeps the mirrored version in step with the backend", () => {
    // PROMPT_FACTORY_VERSION mirrors FACTORY_VERSION in
    // crates/pond-core/src/user_data/domain/prompt_template.rs. A stale copy
    // here means the notice silently never appears, which looks exactly like
    // "nobody is out of date".
    expect(PROMPT_FACTORY_VERSION).toBeGreaterThan(0);
    expect(Number.isInteger(PROMPT_FACTORY_VERSION)).toBe(true);
  });
});
