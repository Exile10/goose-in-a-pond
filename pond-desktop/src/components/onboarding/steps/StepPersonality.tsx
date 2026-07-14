// ────────────────────────────────────────────────────────────
// Step 3 — Personality & Identity (merged)
// Skip allowed — sensible defaults exist
// ────────────────────────────────────────────────────────────

import { useState, useRef, useEffect } from "react";
import { Scale, Zap, Wrench, Sun, Play, Square, VolumeX } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { RadioCard } from "../primitives/RadioCard";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { TTS_VOICES } from "../onboarding.constants";
import { api } from "../../../api/PondApiClient";

const ICON_PROPS = { size: 16, strokeWidth: 1.8 } as const;

/** Short line synthesized when previewing a voice. */
const PREVIEW_LINE = "Hi, I'm your Goose assistant. This is how I sound.";

const PROMPT_STYLES = [
  { value: "balanced",  label: "Balanced",  icon: <Scale {...ICON_PROPS} />,  desc: "Warm and practical. Just enough detail." },
  { value: "concise",   label: "Concise",   icon: <Zap {...ICON_PROPS} />,    desc: "Short, action-first. Skips the small talk." },
  { value: "technical", label: "Technical",  icon: <Wrench {...ICON_PROPS} />, desc: "Step-by-step. Detailed narration for tinkerers." },
  { value: "warm",      label: "Warm",       icon: <Sun {...ICON_PROPS} />,    desc: "Conversational, like a helpful neighbour." },
];

export function StepPersonality() {
  const { draft, patch } = useOnboarding();
  const [playing, setPlaying] = useState<string | null>(null);
  // null = untested, true = TTS worked, false = TTS unavailable on this host.
  const [ttsAvailable, setTtsAvailable] = useState<boolean | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const urlRef = useRef<string | null>(null);

  // Clean up any object URL / audio element on unmount.
  useEffect(() => {
    return () => {
      audioRef.current?.pause();
      if (urlRef.current) URL.revokeObjectURL(urlRef.current);
    };
  }, []);

  async function preview(v: string) {
    // Stop any in-flight playback before starting a new one.
    audioRef.current?.pause();
    if (urlRef.current) { URL.revokeObjectURL(urlRef.current); urlRef.current = null; }

    setPlaying(v);
    try {
      const audioBytes = await api.synthesizeSpeech(PREVIEW_LINE);
      const url = URL.createObjectURL(new Blob([audioBytes], { type: "audio/wav" }));
      urlRef.current = url;
      const audio = new Audio(url);
      audioRef.current = audio;
      audio.onended = () => setPlaying((cur) => (cur === v ? null : cur));
      audio.onerror = () => { setPlaying(null); setTtsAvailable(false); };
      await audio.play();
      setTtsAvailable(true);
    } catch {
      // 503 (no TTS backend) or a playback error — degrade gracefully.
      setPlaying(null);
      setTtsAvailable(false);
    }
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
        <FormLabel>Assistant name <span className="ob-required">*</span></FormLabel>
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
                  onClick={(e) => { e.stopPropagation(); void preview(v.value); }}
                  disabled={ttsAvailable === false || (playing !== null && playing !== v.value)}
                  aria-label={`Preview ${v.label} voice`}
                  className={`ob-voice-card__preview ${playing === v.value ? "ob-voice-card__preview--playing" : ""}`}
                >
                  {ttsAvailable === false
                    ? <VolumeX size={10} />
                    : playing === v.value ? <Square size={10} /> : <Play size={10} />}
                </button>
              </div>
            );
          })}
        </div>
        {ttsAvailable === false && (
          <p className="ob-field-hint" role="status">
            Voice preview isn't available on this host. Your selected voice is
            still saved and will be used once a TTS engine is running.
          </p>
        )}
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
