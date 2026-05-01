// ────────────────────────────────────────────────────────────
// Step 3 — Personality & Identity (merged)
// Skip allowed — sensible defaults exist
// ────────────────────────────────────────────────────────────

import { useState } from "react";
import { Scale, Zap, Wrench, Sun, Play, Square } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { RadioCard } from "../primitives/RadioCard";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { TTS_VOICES } from "../onboarding.constants";

const ICON_PROPS = { size: 16, strokeWidth: 1.8 } as const;

const PROMPT_STYLES = [
  { value: "balanced",  label: "Balanced",  icon: <Scale {...ICON_PROPS} />,  desc: "Warm and practical. Just enough detail." },
  { value: "concise",   label: "Concise",   icon: <Zap {...ICON_PROPS} />,    desc: "Short, action-first. Skips the small talk." },
  { value: "technical", label: "Technical",  icon: <Wrench {...ICON_PROPS} />, desc: "Step-by-step. Detailed narration for tinkerers." },
  { value: "warm",      label: "Warm",       icon: <Sun {...ICON_PROPS} />,    desc: "Conversational, like a helpful neighbour." },
];

export function StepPersonality() {
  const { draft, patch } = useOnboarding();
  const [playing, setPlaying] = useState<string | null>(null);

  function preview(v: string) {
    setPlaying(v);
    setTimeout(() => setPlaying(null), 1400);
  }

  return (
    <div className="ob-step-content">
      <Lead>
        Choose how Goose talks to you, and give your assistant a name and voice.
      </Lead>

      {/* Conversation style */}
      <div className="ob-field">
        <FormLabel>Conversation style</FormLabel>
        <div className="ob-card-grid">
          {PROMPT_STYLES.map((s) => (
            <RadioCard
              key={s.value}
              selected={draft.promptStyle === s.value}
              onClick={() => patch({ promptStyle: s.value })}
            >
              <div className="ob-radio-card__content">
                <div className="ob-radio-card__title-row">
                  {s.icon}
                  <span className="ob-radio-card__label">{s.label}</span>
                </div>
                <span className="ob-radio-card__desc">{s.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      {/* Personality hint */}
      <div className="ob-field">
        <FormLabel optional>Personality hint</FormLabel>
        <p className="ob-field-hint">
          A few words to shape Goose's character, e.g. "curious and warm".
        </p>
        <textarea
          value={draft.personality}
          onChange={(e) => patch({ personality: e.target.value })}
          placeholder="friendly and helpful"
          rows={2}
          className="ob-textarea"
        />
      </div>

      {/* Assistant name */}
      <div className="ob-field">
        <FormLabel>Assistant name <span style={{ color: "var(--color-destructive)" }}>*</span></FormLabel>
        <input
          className="ob-input"
          placeholder="Goose"
          value={draft.assistantName}
          onChange={(e) => patch({ assistantName: e.target.value })}
          required
        />
      </div>

      {/* TTS Voice */}
      <div className="ob-field">
        <FormLabel>Voice</FormLabel>
        <div className="ob-toggle-stack">
          {TTS_VOICES.map((v) => {
            const sel = draft.ttsVoice === v.value;
            return (
              <div
                key={v.value}
                onClick={() => patch({ ttsVoice: v.value })}
                className={`ob-voice-card ${sel ? "ob-voice-card--selected" : ""}`}
              >
                <div className="ob-voice-card__dot" />
                <div className="ob-voice-card__info">
                  <div className="ob-voice-card__name">{v.label}</div>
                  <div className="ob-voice-card__accent">
                    {v.accent} &middot;{" "}
                    <code className="ob-voice-card__file">{v.file}</code>
                  </div>
                </div>
                <button
                  type="button"
                  onClick={(e) => { e.stopPropagation(); preview(v.value); }}
                  className={`ob-voice-card__preview ${playing === v.value ? "ob-voice-card__preview--playing" : ""}`}
                >
                  {playing === v.value ? <Square size={10} /> : <Play size={10} />}
                </button>
              </div>
            );
          })}
        </div>
      </div>

      {/* Speaking rate */}
      <div className="ob-field">
        <FormLabel
          sub={
            draft.ttsRate < 40 ? "\u00b7 Slower"
              : draft.ttsRate > 65 ? "\u00b7 Faster"
              : "\u00b7 Normal"
          }
        >
          Speaking rate
        </FormLabel>
        <input
          type="range"
          min={0}
          max={100}
          step={5}
          value={draft.ttsRate}
          onChange={(e) => patch({ ttsRate: Number(e.target.value) })}
          className="ob-slider"
          style={{
            background: `linear-gradient(to right, var(--color-accent) ${draft.ttsRate}%, var(--grey-200) ${draft.ttsRate}%)`,
          }}
        />
      </div>
    </div>
  );
}
