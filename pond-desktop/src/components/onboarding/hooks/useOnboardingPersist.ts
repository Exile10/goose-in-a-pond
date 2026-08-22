// ────────────────────────────────────────────────────────────
// useOnboardingPersist — Saves step data to backend settings
// ────────────────────────────────────────────────────────────

import { useState, useCallback } from "react";
import { api } from "../../../api/PondApiClient";
import type { OnboardingDraft } from "../onboarding.types";
import type { Settings } from "../../../api/types";
import { FE_STEP_TO_BE } from "../onboarding.constants";

/**
 * Maps onboarding draft fields to backend Settings keys per step,
 * then calls PUT /api/v1/settings to persist.
 *
 * Fields NOT in the Settings struct (preferred name, birthday, avatar,
 * accessibility prefs) belong to the household MEMBER, not the pond, and go to
 * `PATCH /api/v1/profiles/{id}`.
 *
 * They used to go to `localStorage` under "giap-user-profile" while the server
 * read them from SQLite `profiles.preferences`, so the assistant never learned
 * any of them: the reader, the writer and the route all existed and nothing
 * joined them. Two consequences worth keeping in mind here:
 *
 * 1. The key spelling is the contract. The server reads `preferred_name`,
 *    `birthday`, `language`, `accessibility_atypical_speech`. This app's draft
 *    uses camelCase, and a camelCase key sent to the API returns 200 and
 *    reaches the model as nothing — the same bug wearing a different hat.
 * 2. Preferences need a member to hang on, and a fresh pond has none. The
 *    wizard therefore ensures a primary profile exists before writing.
 */
export function useOnboardingPersist() {
  const [isPersisting, setIsPersisting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const persist = useCallback(async (stepId: string, draft: OnboardingDraft): Promise<boolean> => {
    setError(null);
    setIsPersisting(true);

    try {
      const settingsPatch = buildSettingsPatch(stepId, draft);
      const profilePatch = buildProfilePatch(stepId, draft);

      // Save settings to backend (if there are fields for this step)
      if (settingsPatch && Object.keys(settingsPatch).length > 0) {
        await api.updateSettings(settingsPatch);
      }

      // Member preferences go to the server, on the member's own profile.
      if (profilePatch && Object.keys(profilePatch).length > 0) {
        const profileId = await ensurePrimaryProfile(draft.userName);
        if (profileId) {
          await api.updateProfilePrefs(profileId, profilePatch);
        }
      }

      // Track backend progress so a mid-onboarding quit resumes from here.
      // Non-fatal: a step-tracking hiccup must not block advancing the wizard.
      const beStep = FE_STEP_TO_BE[stepId];
      if (beStep) {
        try { await api.recordOnboardingStep(beStep); }
        catch (err) { console.warn("onboard step tracking failed (non-fatal):", err); }
      }

      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to save settings");
      return false;
    } finally {
      setIsPersisting(false);
    }
  }, []);

  const clearError = useCallback(() => setError(null), []);

  return { persist, isPersisting, error, clearError };
}

function buildSettingsPatch(stepId: string, draft: OnboardingDraft): Partial<Settings> | null {
  switch (stepId) {
    case "about-you":
      return {
        user_name: draft.userName.trim(),
      };

    case "locale":
      return {
        timezone: draft.timezone,
        weather_location_name: draft.locationName,
        weather_enabled: draft.enableWeather,
        // Carried through when Auto-detect found them. The wizard used to send
        // only the NAME, so a household finished setup with coordinates of
        // 0,0 and weather that could not be fetched until the server happened
        // to geocode the name on a later save. Sent only when they are real:
        // 0/0 is this struct's unset value, and writing it would overwrite
        // coordinates the server had already worked out.
        ...(draft.latitude !== 0 || draft.longitude !== 0
          ? { weather_latitude: draft.latitude, weather_longitude: draft.longitude }
          : {}),
      };

    case "personality":
      return {
        prompt_style: draft.promptStyle,
        assistant_personality: draft.personality,
        assistant_name: draft.assistantName.trim(),
        voice_tts_voice: draft.ttsVoice,
        // Was collected by the slider and never written — the pace a household
        // chose during setup was discarded the moment onboarding finished.
        voice_tts_speed: draft.ttsRate / 100,
      };

    case "wake-word":
      return {
        voice_wake_word: draft.wakeWord === "custom"
          ? draft.wakeWordCustom.trim()
          : draft.wakeWord,
      };

    case "complete":
      return {
        agent_memory_inject: draft.enableMcpMemory,
      };

    default:
      return null;
  }
}

/**
 * The exact keys `routes.rs :: particulars_for` reads out of
 * `profiles.preferences`. Exported so a test can pin them: this app holds the
 * same fields in camelCase, and the two spellings diverging silently is the
 * defect this file was fixed for.
 */
export const PROFILE_PREF_KEYS = {
  preferredName: "preferred_name",
  birthday: "birthday",
  language: "language",
  atypicalSpeech: "accessibility_atypical_speech",
  slowSpeech: "accessibility_slow_speech",
  highContrast: "accessibility_high_contrast",
  reduceMotion: "accessibility_reduce_motion",
  avatar: "avatar",
} as const;

/**
 * Values are strings because `profiles.preferences` is a `HashMap<String,
 * String>` — the server compares the accessibility flags against the literal
 * `"true"`, so a JSON boolean would read as false forever.
 *
 * An empty value is OMITTED rather than written blank: the prompt builder skips
 * a missing key but would render "The user's birthday is ." for an empty one.
 */
export function buildProfilePatch(
  stepId: string,
  draft: OnboardingDraft,
): Record<string, string> | null {
  if (stepId !== "about-you") return null;

  const raw: Record<string, string> = {
    [PROFILE_PREF_KEYS.preferredName]: (draft.preferredName ?? "").trim(),
    [PROFILE_PREF_KEYS.birthday]: (draft.birthday ?? "").trim(),
    [PROFILE_PREF_KEYS.avatar]: draft.avatar ?? "",
    [PROFILE_PREF_KEYS.atypicalSpeech]: draft.atypicalSpeech ? "true" : "false",
    [PROFILE_PREF_KEYS.slowSpeech]: draft.slowSpeech ? "true" : "false",
    [PROFILE_PREF_KEYS.highContrast]: draft.highContrast ? "true" : "false",
    [PROFILE_PREF_KEYS.reduceMotion]: draft.reduceMotion ? "true" : "false",
  };
  return Object.fromEntries(Object.entries(raw).filter(([, v]) => v !== ""));
}

/**
 * Resolve the profile these preferences belong to, creating one if the pond has
 * none yet.
 *
 * A fresh pond has no profiles and no `primary_profile_id` — nothing in the app
 * ever created either — so without this the preferences have nowhere to go and
 * the wizard would appear to save them while writing nothing. Returns null
 * rather than throwing: failing to record a preferred name must not block
 * somebody getting through onboarding.
 */
async function ensurePrimaryProfile(userName: string): Promise<string | null> {
  try {
    const settings = await api.getSettings();
    const existing = settings.primary_profile_id;
    if (existing) return existing;

    const name = (userName ?? "").trim() || "Me";
    const created = await api.createProfile(name);
    await api.updateSettings({ primary_profile_id: created.id });
    return created.id;
  } catch (err) {
    console.warn("could not resolve a primary profile for preferences:", err);
    return null;
  }
}
