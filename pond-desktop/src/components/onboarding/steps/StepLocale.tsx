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
import { LANGUAGES } from "../onboarding.constants";
import { detectPlace } from "../../../lib/place";
import { ZonePicker } from "../../ZonePicker";

export function StepLocale() {
  const { draft, patch } = useOnboarding();
  const [detecting, setDetecting] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  /**
   * The same cascade Settings runs, on the server.
   *
   * What was here before did not detect anything: it split the time-zone
   * string on "/", waited 400ms so it looked like work, and produced NO
   * COORDINATES — then switched weather on regardless, so setup finished with
   * a forecast that could never be fetched.
   */
  async function detect() {
    setDetecting(true);
    setNote(null);
    try {
      const at = await detectPlace(draft.locationName);
      patch({
        locationName: at.name || draft.locationName,
        timezone: at.timezone,
        latitude: at.latitude,
        longitude: at.longitude,
        // Only offer weather when there is something to ask about. Turning it
        // on with no coordinates and no name is how the old button left every
        // onboarded pond with weather enabled and permanently unconfigured.
        enableWeather: at.has_coordinates || Boolean(at.name),
      });
      setNote(
        at.note
          ? at.note
          : at.certain
            ? `Found ${at.name}.`
            : `Guessed ${at.name} from your time zone.`,
      );
    } catch {
      setNote("Could not work that out. Type the nearest town instead.");
    } finally {
      setDetecting(false);
    }
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
          <ZonePicker
            value={draft.timezone}
            onChange={(timezone) => patch({ timezone })}
            className="ob-select"
            aria-label="Timezone"
          />
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
        {note && <p className="ob-field-hint">{note}</p>}
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
