// ────────────────────────────────────────────────────────────
// Step 4 — Wake Word
// Skip allowed — defaults to "goose"
// ────────────────────────────────────────────────────────────

import { useState, useRef, useEffect } from "react";
import { Mic } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { RadioCard } from "../primitives/RadioCard";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { WAKE_PRESETS } from "../onboarding.constants";

export function StepWakeWord() {
  const { draft, patch } = useOnboarding();
  const [calibrating, setCalibrating] = useState(false);
  const [level, setLevel] = useState(0);
  const [samples, setSamples] = useState(0);
  const tick = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    return () => { if (tick.current) clearInterval(tick.current); };
  }, []);

  function startCalib() {
    setCalibrating(true);
    setLevel(0);
    setSamples(0);
    tick.current = setInterval(() => {
      setLevel(0.15 + Math.random() * 0.85);
      setSamples((s) => {
        const n = s + 1;
        if (n >= 12) {
          if (tick.current) clearInterval(tick.current);
          setCalibrating(false);
          setLevel(0);
        }
        return n;
      });
    }, 220);
  }

  const isCustom = draft.wakeWord === "custom";

  return (
    <div className="ob-step-content">
      <Lead>
        Say this phrase to activate Goose when it's listening in voice mode.
      </Lead>

      {/* Wake phrase selection */}
      <div className="ob-field">
        <FormLabel>Wake phrase</FormLabel>
        <div className="ob-card-grid">
          {WAKE_PRESETS.map((p) => (
            <RadioCard
              key={p.value}
              selected={draft.wakeWord === p.value}
              onClick={() => patch({ wakeWord: p.value })}
            >
              <div className="ob-radio-card__content">
                <strong className="ob-radio-card__label">{p.label}</strong>
                <span className="ob-radio-card__desc">{p.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      {/* Custom phrase input */}
      {isCustom && (
        <div className="ob-field">
          <FormLabel>Your custom phrase <span className="ob-required">*</span></FormLabel>
          <input
            className="ob-input"
            placeholder="e.g. hey duck, morning pond"
            value={draft.wakeWordCustom}
            onChange={(e) => patch({ wakeWordCustom: e.target.value })}
            required
          />
        </div>
      )}

      {/* Calibration */}
      <div className="ob-calibration">
        <div className="ob-calibration__header">
          <div>
            <div className="ob-calibration__title">
              <Mic size={16} strokeWidth={1.8} className="ob-calibration__title-icon" />
              Calibrate microphone
            </div>
            <p className="ob-calibration__desc">
              Say your wake word a few times so Goose learns your voice pattern.
            </p>
          </div>
          <button
            type="button"
            onClick={startCalib}
            disabled={calibrating}
            className="ob-calibration__btn"
          >
            {calibrating ? "Listening\u2026" : samples >= 12 ? "Re-calibrate" : "Start"}
          </button>
        </div>

        {/* Level meter */}
        <div className="ob-calibration__meter">
          {Array.from({ length: 20 }).map((_, i) => {
            const t = i / 19;
            const on = calibrating && level > t;
            return (
              <div
                key={i}
                className="ob-calibration__meter-bar"
                style={{
                  height: on ? `${40 + level * 60}%` : "25%",
                  background: on ? `hsl(${270 - t * 40}, 80%, 60%)` : undefined,
                }}
              />
            );
          })}
        </div>

        {/* Progress */}
        <div className="ob-calibration__progress">
          <span className="ob-calibration__progress-label">Samples</span>
          <div className="ob-calibration__progress-track">
            <div className="ob-calibration__progress-bar" style={{ width: `${(samples / 12) * 100}%` }} />
          </div>
          <span className="ob-calibration__progress-count">{samples}/12</span>
        </div>
      </div>
    </div>
  );
}
