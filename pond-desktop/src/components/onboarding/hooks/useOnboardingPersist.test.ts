// ────────────────────────────────────────────────────────────
// The onboarding profile patch is a CONTRACT with the prompt builder.
//
// `routes.rs :: particulars_for` reads `preferred_name`, `birthday`,
// `language` and `accessibility_atypical_speech` out of
// `profiles.preferences`. This app holds the same fields in camelCase.
//
// Until 2026-08-12 the wizard collected a preferred name and a birthday and
// wrote them to browser `localStorage`, while the server read them from
// SQLite — so the assistant never learned any of them. The reader existed, the
// route existed (`PATCH /api/v1/profiles/{id}`), and nothing joined them.
//
// The replacement can fail the same way for a smaller reason: send
// `preferredName` instead of `preferred_name` and the API returns 200, the row
// looks populated, and the model still hears nothing. These tests exist to make
// that spelling break a build rather than a household.
// ────────────────────────────────────────────────────────────

import { describe, it, expect } from "vitest";
import { buildProfilePatch, PROFILE_PREF_KEYS } from "./useOnboardingPersist";
import type { OnboardingDraft } from "../onboarding.types";

function draft(over: Partial<OnboardingDraft> = {}): OnboardingDraft {
  return {
    userName: "Jerry",
    preferredName: "Cap",
    birthday: "1990-04-02",
    avatar: "duck",
    atypicalSpeech: false,
    slowSpeech: false,
    highContrast: false,
    reduceMotion: false,
    ...over,
  } as OnboardingDraft;
}

describe("buildProfilePatch", () => {
  it("emits the snake_case keys the server actually reads", () => {
    const patch = buildProfilePatch("about-you", draft())!;

    // Spelled out rather than derived from PROFILE_PREF_KEYS: a test that maps
    // the same constant it is checking passes no matter what the constant says.
    expect(patch).toHaveProperty("preferred_name", "Cap");
    expect(patch).toHaveProperty("birthday", "1990-04-02");

    for (const camel of ["preferredName", "atypicalSpeech", "slowSpeech", "highContrast"]) {
      expect(patch, `camelCase key ${camel} would be stored and never read`).not.toHaveProperty(
        camel,
      );
    }
  });

  it("keeps the mapping honest about which spelling is the server's", () => {
    expect(PROFILE_PREF_KEYS.preferredName).toBe("preferred_name");
    expect(PROFILE_PREF_KEYS.atypicalSpeech).toBe("accessibility_atypical_speech");
    expect(PROFILE_PREF_KEYS.language).toBe("language");
  });

  it("writes booleans as the literal strings the server compares against", () => {
    // `profiles.preferences` is HashMap<String, String> and the server tests
    // `== "true"`. A JSON boolean would deserialize-fail or read as false.
    const on = buildProfilePatch("about-you", draft({ atypicalSpeech: true }))!;
    expect(on["accessibility_atypical_speech"]).toBe("true");
    expect(typeof on["accessibility_atypical_speech"]).toBe("string");

    const off = buildProfilePatch("about-you", draft({ atypicalSpeech: false }))!;
    expect(off["accessibility_atypical_speech"]).toBe("false");
  });

  it("omits an empty value rather than writing a blank one", () => {
    // The prompt builder skips a missing key but renders a present-and-empty
    // one, so a blank birthday becomes "The user's birthday is ." in the model's
    // context.
    const patch = buildProfilePatch("about-you", draft({ birthday: "", preferredName: "  " }))!;
    expect(patch).not.toHaveProperty("birthday");
    expect(patch).not.toHaveProperty("preferred_name");
    // ...while a real value on the same step still comes through, so the filter
    // is not simply dropping everything.
    expect(patch["accessibility_atypical_speech"]).toBe("false");
  });

  it("returns nothing for a step that carries no member preferences", () => {
    expect(buildProfilePatch("assistant", draft())).toBeNull();
  });
});
