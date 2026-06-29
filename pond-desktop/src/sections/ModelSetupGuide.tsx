import { useState } from "react";
import { Button } from "@heroui/react";
import { Download, CheckCircle, Zap, Brain, Sparkles, Mic, Database, ChevronRight } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type { ModelEntry, ModelActiveRoles } from "../api/types";

// ── Curated model catalogue ───────────────────────────────────────────────────

interface CuratedModel {
  id: string;          // matches ModelEntry.name (or close enough for partial match)
  displayName: string;
  tagline: string;
  description: string;
  sizeMb: number;
  tier: "fast" | "balanced" | "smart";
  provider: "gguf" | "whisper" | "embedding";
  role: "chat" | "asr" | "embedding";
}

const CURATED_LLM: CuratedModel[] = [
  {
    id: "phi-3.5-mini-instruct",
    displayName: "Phi 3.5 Mini",
    tagline: "Fast · ~2 GB",
    description: "Best for quick questions and low-RAM machines. Punches above its size for everyday tasks.",
    sizeMb: 2200,
    tier: "fast",
    provider: "gguf",
    role: "chat",
  },
  {
    id: "mistral-7b-instruct",
    displayName: "Mistral 7B",
    tagline: "Balanced · ~4 GB",
    description: "The go-to all-rounder. Great reasoning, fast enough for real-time conversation.",
    sizeMb: 4100,
    tier: "balanced",
    provider: "gguf",
    role: "chat",
  },
  {
    id: "llama-3.1-8b-instruct",
    displayName: "Llama 3.1 8B",
    tagline: "Smart · ~5 GB",
    description: "More capable reasoning and longer context. Needs 8 GB RAM minimum.",
    sizeMb: 5000,
    tier: "smart",
    provider: "gguf",
    role: "chat",
  },
];

const CURATED_ASR: CuratedModel = {
  id: "ggml-base.en",
  displayName: "Whisper Base (English)",
  tagline: "~140 MB",
  description: "Lets Goose hear you speak. Small, fast, works offline.",
  sizeMb: 140,
  tier: "fast",
  provider: "whisper",
  role: "asr",
};

const CURATED_EMBEDDING: CuratedModel = {
  id: "nomic-embed-text",
  displayName: "Nomic Embed Text",
  tagline: "~270 MB",
  description: "Gives Goose long-term memory — it can recall things you told it weeks ago.",
  sizeMb: 270,
  tier: "fast",
  provider: "embedding",
  role: "embedding",
};

const TIER_ICON: Record<string, React.ReactNode> = {
  fast:     <Zap size={14} />,
  balanced: <Sparkles size={14} />,
  smart:    <Brain size={14} />,
};

const TIER_COLOR: Record<string, string> = {
  fast:     "#16A34A",
  balanced: "var(--pp)",
  smart:    "#0EA5E9",
};

// ── Helpers ───────────────────────────────────────────────────────────────────

function modelMatches(entries: ModelEntry[], id: string): ModelEntry | undefined {
  return entries.find((m) => m.name.toLowerCase().includes(id.toLowerCase().split("-")[0]) &&
    m.name.toLowerCase().includes(id.toLowerCase().split("-")[1] ?? ""));
}

function isDownloaded(entries: ModelEntry[], id: string): boolean {
  const m = modelMatches(entries, id);
  return !!m && m.downloaded !== false;
}

function isActive(roles: ModelActiveRoles | null, role: string, id: string): boolean {
  const r = roles?.[role as keyof ModelActiveRoles];
  return !!r && typeof r === "object" && "model" in r &&
    (r as { model: string }).model.toLowerCase().includes(id.toLowerCase().split("-")[0]);
}

// ── Sub-components ────────────────────────────────────────────────────────────

interface LlmCardProps {
  model: CuratedModel;
  models: ModelEntry[];
  activeRoles: ModelActiveRoles | null;
  onActivate: (provider: string, name: string, role: string) => void;
  onDownloadStarted: () => void;
}

function LlmCard({ model, models, activeRoles, onActivate, onDownloadStarted }: LlmCardProps) {
  const state = useAppState();
  const [busy, setBusy] = useState<"downloading" | "activating" | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  const downloaded = isDownloaded(models, model.id);
  const active = isActive(activeRoles, "chat", model.id);
  const entry = modelMatches(models, model.id);

  async function handleDownload() {
    setBusy("downloading"); setMsg(null);
    try {
      await api.downloadModel("gguf", entry?.name ?? model.id);
      onDownloadStarted();
      setMsg("Download started — this may take a few minutes.");
    } catch (e) { setMsg(`Error: ${String(e)}`); }
    finally { setBusy(null); }
  }

  async function handleActivate() {
    if (!entry) return;
    setBusy("activating"); setMsg(null);
    try {
      onActivate("gguf", entry.name, "chat");
    } finally { setBusy(null); }
  }

  return (
    <div className={`setup-model-card${active ? " is-active" : ""}`}>
      <div className="setup-model-card__tier" style={{ color: TIER_COLOR[model.tier] }}>
        {TIER_ICON[model.tier]}
        <span>{model.tier.charAt(0).toUpperCase() + model.tier.slice(1)}</span>
      </div>
      <div className="setup-model-card__body">
        <div className="setup-model-card__name">{model.displayName}</div>
        <div className="setup-model-card__tag">{model.tagline}</div>
        <div className="setup-model-card__desc">{model.description}</div>
        {msg && <div className="setup-model-card__msg">{msg}</div>}
      </div>
      <div className="setup-model-card__actions">
        {active ? (
          <span className="setup-active-badge"><CheckCircle size={12} /> Active</span>
        ) : downloaded ? (
          <Button size="sm" variant="primary" onPress={handleActivate} isDisabled={busy === "activating"}>
            {busy === "activating" ? "Setting up…" : "Use this model"}
          </Button>
        ) : (
          <Button
            size="sm"
            variant="secondary"
            onPress={handleDownload}
            isDisabled={busy === "downloading" || !state.serverOnline}
          >
            <Download size={12} />
            {busy === "downloading" ? "Starting…" : "Download"}
          </Button>
        )}
      </div>
    </div>
  );
}

interface OptionalRoleCardProps {
  icon: React.ReactNode;
  title: string;
  subtitle: string;
  model: CuratedModel;
  models: ModelEntry[];
  activeRoles: ModelActiveRoles | null;
  apiCategory: string;
  onActivate: (provider: string, name: string, role: string) => void;
  onDownloadStarted: () => void;
}

function OptionalRoleCard({
  icon, title, subtitle, model, models, activeRoles,
  apiCategory, onActivate, onDownloadStarted,
}: OptionalRoleCardProps) {
  const state = useAppState();
  const [busy, setBusy] = useState<"downloading" | "activating" | null>(null);
  const [msg, setMsg] = useState<string | null>(null);

  const downloaded = isDownloaded(models, model.id);
  const active = isActive(activeRoles, model.role, model.id);
  const entry = modelMatches(models, model.id);

  async function handleEnable() {
    setBusy("downloading"); setMsg(null);
    try {
      if (!downloaded) {
        await api.downloadModel(apiCategory, entry?.name ?? model.id);
        onDownloadStarted();
        setMsg("Downloading — Goose will activate this automatically when done.");
      } else if (entry) {
        onActivate(model.provider, entry.name, model.role);
      }
    } catch (e) { setMsg(`Error: ${String(e)}`); }
    finally { setBusy(null); }
  }

  return (
    <div className={`setup-optional-card${active ? " is-active" : ""}`}>
      <div className="setup-optional-card__left">
        <span className="setup-optional-card__icon">{icon}</span>
        <div>
          <div className="setup-optional-card__title">{title}</div>
          <div className="setup-optional-card__sub">{subtitle}</div>
          <div className="setup-optional-card__desc">{model.description}</div>
          <div className="setup-optional-card__meta">{model.displayName} · {model.tagline}</div>
          {msg && <div className="setup-model-card__msg">{msg}</div>}
        </div>
      </div>
      <div className="setup-optional-card__right">
        {active ? (
          <span className="setup-active-badge"><CheckCircle size={12} /> Active</span>
        ) : (
          <Button
            size="sm"
            variant="secondary"
            onPress={handleEnable}
            isDisabled={busy !== null || !state.serverOnline}
          >
            {downloaded ? "Enable" : <><Download size={12} /> {busy ? "Starting…" : "Download & enable"}</>}
          </Button>
        )}
      </div>
    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────────

interface ModelSetupGuideProps {
  models: ModelEntry[];
  activeRoles: ModelActiveRoles | null;
  modelsLoading: boolean;
  onActivate: (provider: string, name: string, role: string) => void;
  onDownloadStarted: () => void;
  onGoToAdvanced: () => void;
}

export function ModelSetupGuide({
  models, activeRoles, modelsLoading,
  onActivate, onDownloadStarted, onGoToAdvanced,
}: ModelSetupGuideProps) {
  const hasMainModel = !!(activeRoles?.chat?.provider && activeRoles?.chat?.model);

  const allLlmModels = models.filter(
    (m) => !["whisper", "tts", "tts_piper", "tts_http", "embedding"].includes(m.provider) &&
      m.category !== "embedding",
  );
  const asrModels = models.filter((m) => m.provider === "whisper");
  const embeddingModels = models.filter(
    (m) => m.provider === "embedding" || m.category === "embedding",
  );

  return (
    <div className="setup-guide">
      {/* Step 1 — Assistant */}
      <div className="setup-section">
        <div className="setup-section__hd">
          <div className="setup-section__num">1</div>
          <div>
            <div className="setup-section__title">Choose your assistant</div>
            <div className="setup-section__sub">
              This is the model that powers Goose. Download one and tap "Use this model".
            </div>
          </div>
        </div>

        {modelsLoading ? (
          <p className="hint">Loading models…</p>
        ) : (
          <div className="setup-model-grid">
            {CURATED_LLM.map((m) => (
              <LlmCard
                key={m.id}
                model={m}
                models={allLlmModels}
                activeRoles={activeRoles}
                onActivate={onActivate}
                onDownloadStarted={onDownloadStarted}
              />
            ))}
          </div>
        )}

        {hasMainModel && (
          <div className="setup-section__current">
            <CheckCircle size={13} color="#16A34A" />
            Currently using: <strong>{activeRoles!.chat!.model}</strong>
          </div>
        )}
      </div>

      {/* Step 2 — Voice (optional) */}
      <div className="setup-section">
        <div className="setup-section__hd">
          <div className="setup-section__num setup-section__num--opt">2</div>
          <div>
            <div className="setup-section__title">
              Add voice <span className="setup-optional-tag">Optional</span>
            </div>
            <div className="setup-section__sub">
              Lets Goose hear you speak and talk back. You can skip this and add it later.
            </div>
          </div>
        </div>
        <OptionalRoleCard
          icon={<Mic size={18} />}
          title="Speech recognition"
          subtitle="Goose can understand spoken words"
          model={CURATED_ASR}
          models={asrModels}
          activeRoles={activeRoles}
          apiCategory="whisper"
          onActivate={onActivate}
          onDownloadStarted={onDownloadStarted}
        />
        {activeRoles?.asr?.model && (
          <div className="setup-section__current">
            <CheckCircle size={13} color="#16A34A" />
            Currently using: <strong>{activeRoles.asr.model}</strong>
          </div>
        )}
      </div>

      {/* Step 3 — Memory (optional) */}
      <div className="setup-section">
        <div className="setup-section__hd">
          <div className="setup-section__num setup-section__num--opt">3</div>
          <div>
            <div className="setup-section__title">
              Add memory <span className="setup-optional-tag">Optional</span>
            </div>
            <div className="setup-section__sub">
              Goose remembers context across conversations. Skip and add later.
            </div>
          </div>
        </div>
        <OptionalRoleCard
          icon={<Database size={18} />}
          title="Embedding model"
          subtitle="Goose remembers things across sessions"
          model={CURATED_EMBEDDING}
          models={embeddingModels}
          activeRoles={activeRoles}
          apiCategory="embedding"
          onActivate={onActivate}
          onDownloadStarted={onDownloadStarted}
        />
        {activeRoles?.embedding?.model && (
          <div className="setup-section__current">
            <CheckCircle size={13} color="#16A34A" />
            Currently using: <strong>{activeRoles.embedding.model}</strong>
          </div>
        )}
      </div>

      {/* Footer — link to advanced */}
      <div className="setup-footer">
        <span className="setup-footer__text">
          Need Ollama, Llamafile, TTS, or a specific HuggingFace model?
        </span>
        <button className="setup-footer__link" onClick={onGoToAdvanced}>
          Open advanced settings <ChevronRight size={13} />
        </button>
      </div>
    </div>
  );
}
