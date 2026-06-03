// ────────────────────────────────────────────────────────────
// Step 2 — Language & Location
// REQUIRED: timezone is pre-populated from Intl, user must confirm
// ────────────────────────────────────────────────────────────

import { useState } from "react";
import { MapPin, CloudSun } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { FormLabel } from "../primitives/FormLabel";
import { ToggleRow } from "../primitives/ToggleRow";
import { Lead } from "../primitives/StepShell";
import { LANGUAGES, TIMEZONES } from "../onboarding.constants";

export function StepLocale() {
  const { draft, patch } = useOnboarding();
  const [detecting, setDetecting] = useState(false);

  function detect() {
    setDetecting(true);
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    const city = tz.split("/").pop()?.replace(/_/g, " ") || "";
    setTimeout(() => {
      patch({ locationName: city, timezone: tz, enableWeather: true });
      setDetecting(false);
    }, 400);
  }

  return (
    <div className="ob-step-content">
      <Lead>Used for voice responses, weather, and time-aware answers.</Lead>

      <div className="ob-field-grid">
        <div>
          <FormLabel>Language</FormLabel>
          <select
            value={draft.language}
            onChange={(e) => patch({ language: e.target.value })}
            className="ob-select"
          >
            {LANGUAGES.map((l) => (
              <option key={l.key} value={l.key}>
                {l.label}
              </option>
            ))}
          </select>
        </div>
        <div>
          <FormLabel>Timezone</FormLabel>
          <select
            value={draft.timezone}
            onChange={(e) => patch({ timezone: e.target.value })}
            className="ob-select"
          >
            {TIMEZONES.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </div>
      </div>

      <div className="ob-field">
        <FormLabel optional>City / location name</FormLabel>
        <div className="ob-input-row">
          <input
            className="ob-input"
            placeholder="e.g. Nairobi, London, New York"
            value={draft.locationName}
            onChange={(e) => patch({ locationName: e.target.value })}
          />
          <button
            type="button"
            onClick={detect}
            disabled={detecting}
            className="ob-btn-secondary"
          >
            <MapPin size={14} strokeWidth={1.8} />
            {detecting ? "Detecting\u2026" : "Auto-detect"}
          </button>
        </div>
      </div>

      <ToggleRow
        icon={<CloudSun size={18} strokeWidth={1.8} />}
        title="Enable live weather"
        desc="Pulls forecast from Open-Meteo. Approximate network location only — no GPS needed."
        checked={draft.enableWeather}
        onChange={(v) => patch({ enableWeather: v })}
      />
    </div>
  );
}
