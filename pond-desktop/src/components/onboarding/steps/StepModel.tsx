// ────────────────────────────────────────────────────────────
// Step 5 — AI Model (LLM + ASR + TTS)
// REQUIRED: an LLM model must be selected to proceed.
// ASR and TTS are optional (sensible defaults exist).
// ────────────────────────────────────────────────────────────

import { Cpu, Server, HardDrive, AudioLines, Volume2 } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { RadioCard } from "../primitives/RadioCard";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { useAvailableModels } from "../hooks/useAvailableModels";
import { PROVIDERS } from "../onboarding.constants";
import type { ModelEntry } from "../../../api/types";

const ICON_PROPS = { size: 18, strokeWidth: 1.8 } as const;

const PROVIDER_ICONS: Record<string, React.ReactNode> = {
  llamafile: <Cpu {...ICON_PROPS} />,
  ollama:    <Server {...ICON_PROPS} />,
  local:     <HardDrive {...ICON_PROPS} />,
};

/** Recommended LLM models when the API returns empty. */
const RECOMMENDED_LLM: Record<string, ModelEntry[]> = {
  llamafile: [
    { id: "gemma-2-2b-it", provider: "llamafile", name: "gemma-2-2b-it.Q4_K_M", display_name: "Gemma 2 2B Instruct", is_active: false, ram_estimate_mb: 1800, recommended_role: "Fast", downloaded: false, size_mb: 1600, category: "llamafile", description: "Default llamafile model. Lightweight and responsive." },
  ],
  ollama: [
    { id: "gemma4-e2b", provider: "ollama", name: "gemma4:e2b", display_name: "Gemma 4 E2B", is_active: false, ram_estimate_mb: 3200, recommended_role: "Recommended", downloaded: false, size_mb: 3100, category: "ollama", description: "Tool-capable. Best default for the assistant." },
    { id: "gemma4", provider: "ollama", name: "gemma4:latest", display_name: "Gemma 4", is_active: false, ram_estimate_mb: 5120, recommended_role: "Capable", downloaded: false, size_mb: 4800, category: "ollama", description: "Latest generation. Best quality responses." },
  ],
  local: [
    { id: "gemma-4-e2b", provider: "local", name: "gemma-4-E2B-it-Q4_K_M.gguf", display_name: "Gemma 4 E2B Q4_K_M", is_active: false, ram_estimate_mb: 3200, recommended_role: "Recommended", downloaded: false, size_mb: 3100, category: "gguf", description: "Recommended GGUF. 3.1 GB, excellent quality for local inference." },
  ],
};

/** Recommended ASR models when the API returns empty. */
const RECOMMENDED_ASR: ModelEntry[] = [
  { id: "whisper-base", provider: "whisper", name: "base", display_name: "Whisper Base", is_active: false, ram_estimate_mb: 290, recommended_role: "Recommended", downloaded: false, size_mb: 142, category: "whisper", description: "Best balance of accuracy and speed for voice commands." },
  { id: "whisper-tiny", provider: "whisper", name: "tiny", display_name: "Whisper Tiny", is_active: false, ram_estimate_mb: 150, recommended_role: "Fast", downloaded: false, size_mb: 39, category: "whisper", description: "Ultra-fast wake word detection. Lower accuracy." },
  { id: "whisper-small", provider: "whisper", name: "small", display_name: "Whisper Small", is_active: false, ram_estimate_mb: 970, recommended_role: "Accurate", downloaded: false, size_mb: 466, category: "whisper", description: "Higher accuracy for complex speech. Uses more RAM." },
];

/** Recommended TTS models when the API returns empty. */
const RECOMMENDED_TTS: ModelEntry[] = [
  { id: "piper-lessac", provider: "piper", name: "en_US-lessac-medium", display_name: "Lessac (Medium)", is_active: false, ram_estimate_mb: 64, recommended_role: "Recommended", downloaded: false, size_mb: 63, category: "tts", description: "Natural US English voice. Best quality for the size." },
  { id: "piper-amy", provider: "piper", name: "en_GB-amy-medium", display_name: "Amy (Medium)", is_active: false, ram_estimate_mb: 64, recommended_role: "British", downloaded: false, size_mb: 63, category: "tts", description: "British English female voice." },
];

// ── Shared model card renderer ───────────────────────────────

function ModelCard({
  model,
  selected,
  onSelect,
}: {
  model: ModelEntry;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <div
      onClick={onSelect}
      className={`ob-model-card ${selected ? "ob-model-card--selected" : ""}`}
    >
      <div className="ob-model-card__dot" />
      <div className="ob-model-card__info">
        <div className="ob-model-card__meta">
          <code className="ob-model-card__name">{model.display_name || model.name}</code>
          {model.size_mb != null && (
            <span className="ob-model-card__size">
              {model.size_mb >= 1024 ? `${(model.size_mb / 1024).toFixed(1)} GB` : `${model.size_mb} MB`}
            </span>
          )}
          {model.ram_estimate_mb != null && (
            <span className="ob-model-card__size">
              ~{model.ram_estimate_mb >= 1024 ? `${(model.ram_estimate_mb / 1024).toFixed(1)} GB` : `${model.ram_estimate_mb} MB`} RAM
            </span>
          )}
        </div>
        {model.description && (
          <p className="ob-model-card__desc">{model.description}</p>
        )}
      </div>
      <div className="ob-model-card__tags">
        {model.recommended_role && (
          <span className="ob-model-card__tag">{model.recommended_role}</span>
        )}
        <span
          className={`ob-model-card__status ${
            model.downloaded !== false
              ? "ob-model-card__status--ready"
              : "ob-model-card__status--download"
          }`}
        >
          {model.downloaded !== false ? "\u2713 Ready" : "Download needed"}
        </span>
      </div>
    </div>
  );
}

// ── Main step component ──────────────────────────────────────

export function StepModel() {
  const { draft, patch } = useOnboarding();
  const { grouped, asrModels, ttsModels, loading, error } = useAvailableModels();

  // ── LLM providers + models ──
  const providers = PROVIDERS.map((p) => {
    const live = grouped.find((g) => g.provider === p.key);
    const models = live?.models?.length ? live.models : (RECOMMENDED_LLM[p.key] || []);
    return { ...p, models };
  });

  const currentLlmModels =
    providers.find((p) => p.key === draft.llmProvider)?.models || [];

  // ── ASR + TTS with fallbacks ──
  const asrList = asrModels.length > 0 ? asrModels : RECOMMENDED_ASR;
  const ttsList = ttsModels.length > 0 ? ttsModels : RECOMMENDED_TTS;

  return (
    <div className="ob-step-content">
      <Lead>
        Select the models that power Goose. The LLM is required — ASR and TTS
        are optional but needed for voice mode.
      </Lead>

      {/* ── LLM Provider ─────────────────────────────────── */}
      <div className="ob-field">
        <FormLabel>LLM provider</FormLabel>
        <div className="ob-toggle-stack">
          {providers.map((p) => (
            <RadioCard
              key={p.key}
              selected={draft.llmProvider === p.key}
              onClick={() => {
                const first = p.models[0]?.name || "";
                patch({ llmProvider: p.key, llmModel: first });
              }}
            >
              <div className="ob-radio-card__content">
                <div className="ob-radio-card__title-row">
                  {PROVIDER_ICONS[p.key] || <Cpu {...ICON_PROPS} />}
                  <span className="ob-radio-card__label">{p.label}</span>
                  {p.recommended && (
                    <span className="ob-toggle-row__badge">Recommended</span>
                  )}
                  {p.models.length > 0 && (
                    <span className="ob-model-count">
                      {p.models.length} model{p.models.length !== 1 ? "s" : ""}
                    </span>
                  )}
                </div>
                <span className="ob-radio-card__desc">{p.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      {/* ── LLM Model ────────────────────────────────────── */}
      <div className="ob-field">
        <FormLabel>Chat model <span className="ob-required">*</span></FormLabel>
        {loading && <p className="ob-field-hint">Loading available models...</p>}
        {error && <p className="ob-field-error">{error}</p>}
        {!loading && currentLlmModels.length === 0 && !error && (
          <p className="ob-field-hint">No models available for this provider. Make sure the provider is running.</p>
        )}
        <div className="ob-toggle-stack">
          {currentLlmModels.map((m) => (
            <ModelCard
              key={m.name}
              model={m}
              selected={draft.llmModel === m.name}
              onSelect={() => patch({ llmModel: m.name })}
            />
          ))}
        </div>
      </div>

      {/* ── ASR (Whisper) ─────────────────────────────────── */}
      <div className="ob-model-section">
        <div className="ob-model-section__header">
          <AudioLines {...ICON_PROPS} />
          <div>
            <div className="ob-model-section__title">Speech recognition (ASR)</div>
            <div className="ob-model-section__subtitle">Whisper model for voice commands and transcription</div>
          </div>
        </div>
        <div className="ob-toggle-stack">
          {asrList.map((m) => (
            <ModelCard
              key={m.name}
              model={m}
              selected={draft.asrModel === m.name}
              onSelect={() => patch({ asrModel: m.name })}
            />
          ))}
        </div>
        {!draft.asrModel && (
          <p className="ob-field-hint">Optional — needed for voice mode. Can be configured later in Settings.</p>
        )}
      </div>

      {/* ── TTS (Piper) ───────────────────────────────────── */}
      <div className="ob-model-section">
        <div className="ob-model-section__header">
          <Volume2 {...ICON_PROPS} />
          <div>
            <div className="ob-model-section__title">Text-to-speech (TTS)</div>
            <div className="ob-model-section__subtitle">Piper voice model for spoken responses</div>
          </div>
        </div>
        <div className="ob-toggle-stack">
          {ttsList.map((m) => (
            <ModelCard
              key={m.name}
              model={m}
              selected={draft.ttsModel === m.name}
              onSelect={() => patch({ ttsModel: m.name })}
            />
          ))}
        </div>
        {!draft.ttsModel && (
          <p className="ob-field-hint">Optional — needed for voice mode. Can be configured later in Settings.</p>
        )}
      </div>
    </div>
  );
}
