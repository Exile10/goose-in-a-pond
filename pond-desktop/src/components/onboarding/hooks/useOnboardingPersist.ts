// ────────────────────────────────────────────────────────────
// useOnboardingPersist — Saves step data to backend settings
// ────────────────────────────────────────────────────────────

import { useState, useCallback } from "react";
import { api } from "../../../api/PondApiClient";
import type { OnboardingDraft } from "../onboarding.types";
import type { Settings } from "../../../api/types";

/**
 * Maps onboarding draft fields to backend Settings keys per step,
 * then calls PUT /api/v1/settings to persist.
 *
 * Fields NOT in the Settings struct (preferredName, birthday, avatar,
 * accessibility prefs) are saved to localStorage under "giap-user-profile".
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

      // Save local-only fields to localStorage
      if (profilePatch) {
        const stored = localStorage.getItem("giap-user-profile");
        const existing = stored ? JSON.parse(stored) : {};
        localStorage.setItem("giap-user-profile", JSON.stringify({ ...existing, ...profilePatch }));
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
      };

    case "personality":
      return {
        prompt_style: draft.promptStyle,
        assistant_personality: draft.personality,
        assistant_name: draft.assistantName.trim(),
        voice_tts_voice: draft.ttsVoice,
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

function buildProfilePatch(
  stepId: string,
  draft: OnboardingDraft,
): Record<string, unknown> | null {
  switch (stepId) {
    case "about-you":
      return {
        preferredName: draft.preferredName,
        birthday: draft.birthday,
        avatar: draft.avatar,
        atypicalSpeech: draft.atypicalSpeech,
        slowSpeech: draft.slowSpeech,
        highContrast: draft.highContrast,
        reduceMotion: draft.reduceMotion,
      };
    default:
      return null;
  }
}
