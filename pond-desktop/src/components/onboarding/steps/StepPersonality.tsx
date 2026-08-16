// ────────────────────────────────────────────────────────────
// Step 3 — Personality & Identity (merged)
// Skip allowed — sensible defaults exist
// ────────────────────────────────────────────────────────────

import { useState, useEffect, useMemo, useRef, useCallback } from "react";
import { Scale, Zap, Wrench, Sun } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { RadioCard } from "../primitives/RadioCard";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { api } from "../../../api/PondApiClient";
import { useVoicePreview } from "../../../voice/useVoicePreview";
import { clampPace, ONBOARDING_QUALITY } from "../../../voice/voiceCatalogue";
import { VoicePicker } from "../../../hub/views/settings/VoicePicker";
import "../../../hub/views/settings/voice-picker.css";

const ICON_PROPS = { size: 16, strokeWidth: 1.8 } as const;



const PROMPT_STYLES = [
  { value: "balanced",  label: "Balanced",  icon: <Scale {...ICON_PROPS} />,  desc: "Warm and practical. Just enough detail." },
  { value: "concise",   label: "Concise",   icon: <Zap {...ICON_PROPS} />,    desc: "Short, action-first. Skips the small talk." },
  { value: "technical", label: "Technical",  icon: <Wrench {...ICON_PROPS} />, desc: "Step-by-step. Detailed narration for tinkerers." },
  { value: "warm",      label: "Warm",       icon: <Sun {...ICON_PROPS} />,    desc: "Conversational, like a helpful neighbour." },
];

export function StepPersonality() {
  const { draft, patch } = useOnboarding();
  const preview = useVoicePreview();

  const [models, setModels] = useState<{ name: string; downloaded?: boolean }[]>([]);
  const [loadingVoices, setLoadingVoices] = useState(true);
  const [applying, setApplying] = useState(false);
  // The tier actually in force. Starts as the one setup asks for and is
  // replaced by whatever the server reports back, which is not always the same
  // string — see the apply call below.
  const [tier, setTier] = useState<string>(ONBOARDING_QUALITY);
  const [transfers, setTransfers] = useState<
    { filename: string; downloaded: number; total: number | null }[]
  >([]);
  const paceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Every Kokoro voice the catalogue knows. Onboarding runs before there is a
  // device token, which is why `/voice/tts/apply` is public until onboarding
  // completes — the alternative was a wizard that writes a voice it cannot
  // apply or preview.
  useEffect(() => {
    api
      .listModels()
      .then((m) =>
        setModels(
          m
            .filter((x) => (x.category ?? x.provider) === "tts_kokoro")
            .map((x) => ({ name: x.name, downloaded: x.downloaded })),
        ),
      )
      .catch(() => {
        /* offline: the picker falls back to whatever voice is already set */
      })
      .finally(() => setLoadingVoices(false));

    // Setup uses the smallest tier, settled before anything is previewed so
    // every sample the household hears comes from the engine they will
    // actually be running. Doing it later would let them judge a voice on one
    // tier and then live with another.
    //
    // Applied first and stored second, because the server has the last word:
    // a tier that cannot produce audio on this machine comes back substituted.
    // Storing the request instead of the answer would leave the saved tier and
    // the running one disagreeing — the exact thing this ordering prevents.
    api
      .applyTtsSettings({ quality: ONBOARDING_QUALITY })
      .then((r) => {
        const effective = r?.quality || ONBOARDING_QUALITY;
        setTier(effective);
        return api.updateSettings({ voice_tts_quality: effective });
      })
      .catch(() => {
        /* offline: the tier is saved on the next successful apply */
      });
    return () => {
      if (paceTimer.current) clearTimeout(paceTimer.current);
      if (pollTimer.current) clearTimeout(pollTimer.current);
    };
  }, []);

  const voices = useMemo(() => models.map((m) => m.name), [models]);
  const installed = useMemo(
    () => new Set(models.filter((m) => m.downloaded !== false).map((m) => m.name)),
    [models],
  );

  /** Follow a fetch while it runs, so a first-time voice shows progress. */
  const pollTransfers = useCallback(() => {
    if (pollTimer.current) clearTimeout(pollTimer.current);
    const tick = async () => {
      try {
        const { downloads } = await api.getDownloadProgress();
        const voiceOnes = downloads
          .filter((d) => d.category === "tts_kokoro" && d.status === "downloading")
          .map((d) => ({
            filename: d.filename,
            downloaded: d.downloaded_bytes ?? 0,
            total: d.total_bytes ?? null,
          }));
        setTransfers(voiceOnes);
        if (voiceOnes.length) pollTimer.current = setTimeout(tick, 700);
      } catch {
        setTransfers([]);
      }
    };
    void tick();
  }, []);

  async function chooseVoice(id: string) {
    const previous = draft.ttsVoice;
    patch({ ttsVoice: id });
    setApplying(true);
    try {
      await api.updateSettings({
        voice_tts_voice: id,
        voice_tts_speed: draft.ttsRate / 100,
        voice_tts_quality: tier,
      });
      pollTransfers();
      // Applies to the running engine, fetching the voice first when this is
      // the household's first time choosing it. The tier rides along so the
      // sample is never produced by a different one.
      await api.applyTtsSettings({ voice: id, quality: tier });
      void preview.play();
    } catch {
      patch({ ttsVoice: previous });
    } finally {
      setApplying(false);
      setTransfers([]);
    }
  }

  function choosePace(next: number) {
    const pace = clampPace(next);
    patch({ ttsRate: Math.round(pace * 100) });
    if (paceTimer.current) clearTimeout(paceTimer.current);
    paceTimer.current = setTimeout(() => {
      api
        .updateSettings({ voice_tts_speed: pace })
        .then(() => api.applyTtsSettings({ speed: pace, quality: tier }))
        .then(() => preview.play())
        .catch(() => {});
    }, 500);
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

      {/* ── Voice ──
          The same picker the Models page uses, so the voice chosen during
          setup is chosen the same way it will be changed later. It replaces
          four hardcoded cards whose ids named nothing that existed, and a
          rate slider whose value was collected and then discarded. */}
      <div className="ob-field">
        <FormLabel>Voice</FormLabel>
        <VoicePicker
          voices={voices}
          selected={draft.ttsVoice}
          onSelect={(id) => void chooseVoice(id)}
          pace={draft.ttsRate / 100}
          onPaceChange={choosePace}
          preview={preview}
          loading={loadingVoices}
          installed={installed}
          applying={applying}
          transfers={transfers}
          maxPerAccent={3}
        />
      </div>
    </div>
  );
}
