import { useState, useEffect, lazy, Suspense } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Tabs,
  Switch,
  Button,
  Card,
  CardContent,
  Chip,
  RadioGroup,
  Radio,
} from "@heroui/react";
import { Trash2, Plus } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppDispatch, useAppState } from "../state/AppContext";
import type { Settings as SettingsType, Extension } from "../api/types";
import { ModelPickerModal, type ModelRole } from "../components/ModelPickerModal";
import { PageHeader, Section, Row } from "../components/shared";
const WakeWordCalibration = lazy(() => import("../components/WakeWordCalibration").then(m => ({ default: m.WakeWordCalibration })));

// ── Tab definitions ───────────────────────────────────────────

type SettingsTab = "identity" | "voice" | "models" | "prompts" | "location" | "agent" | "data" | "tools";

const TABS: Array<{ id: SettingsTab; label: string }> = [
  { id: "identity", label: "Identity" },
  { id: "voice",    label: "Voice" },
  { id: "models",   label: "Models" },
  { id: "prompts",  label: "Prompts" },
  { id: "location", label: "Location" },
  { id: "agent",    label: "Agent" },
  { id: "data",     label: "Data" },
  { id: "tools",    label: "Tools" },
];

// Hardcoded common IANA timezone list (offline-first, no API needed)
const TIMEZONES = [
  "UTC",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
  "America/Anchorage",
  "Pacific/Honolulu",
  "America/Toronto",
  "America/Vancouver",
  "Europe/London",
  "Europe/Paris",
  "Europe/Berlin",
  "Europe/Rome",
  "Europe/Moscow",
  "Asia/Tokyo",
  "Asia/Shanghai",
  "Asia/Kolkata",
  "Asia/Dubai",
  "Australia/Sydney",
];

// ── Tab Panels ────────────────────────────────────────────────

function IdentityTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  return (
    <>
      <Section title="Personal">
        <Row label="Your name" hint="How Goose addresses you">
          <input
            style={nativeInput}
            value={s.user_name ?? ""}
            onChange={(e) => patch("user_name", e.target.value)}
            placeholder="Friend"
          />
        </Row>
        <Row label="Assistant name" hint="What you call your assistant">
          <input
            style={nativeInput}
            value={s.assistant_name ?? ""}
            onChange={(e) => patch("assistant_name", e.target.value)}
            placeholder="Goose"
          />
        </Row>
        <Row label="Timezone">
          <select
            style={selectFallback}
            value={s.timezone ?? "UTC"}
            onChange={(e) => patch("timezone", e.target.value)}
          >
            {TIMEZONES.map((tz) => (
              <option key={tz} value={tz}>{tz}</option>
            ))}
          </select>
        </Row>
      </Section>

      <Section title="Personality">
        <Row label="Personality" hint="Describe how your assistant should behave">
          <textarea
            style={{ ...nativeInput, height: "80px", resize: "vertical" as const }}
            value={s.assistant_personality ?? ""}
            onChange={(e) => patch("assistant_personality", e.target.value)}
            placeholder="Friendly, concise, and helpful"
          />
        </Row>
      </Section>
    </>
  );
}

function VoiceTab({
  s,
  patch,
  hotkey,
  setHotkey,
  applyHotkey,
  refreshSettings,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  hotkey: string;
  setHotkey: (v: string) => void;
  applyHotkey: () => void;
  refreshSettings: () => Promise<void>;
}) {
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [calibrating, setCalibrating] = useState(false);
  const recDur = s.voice_recording_duration_secs ?? 30;

  const wakePhrase = (s.voice_wake_word ?? "").trim();
  const transcriptions = s.voice_wake_word_transcriptions ?? [];
  const isCalibrated = transcriptions.length > 0;

  async function startCalibration() {
    if (wakePhrase) {
      try {
        await api.updateSettings({ voice_wake_word: wakePhrase });
        await api.resetWakeWordCalibration();
      } catch { /* ignore -- calibration component handles errors */ }
    }
    setCalibrating(true);
  }

  async function handleCalibrationComplete() {
    setCalibrating(false);
    await refreshSettings();
  }

  async function clearCalibration() {
    try {
      await api.resetWakeWordCalibration();
      patch("voice_wake_word_transcriptions", []);
    } catch { /* ignore */ }
  }

  return (
    <>
      {/* Activation */}
      <Section title="Activation">
        <Row label="Keyboard shortcut" hint="Press this to activate voice from anywhere">
          <div className="shortcut-row">
            <input
              style={{ ...nativeInput, flex: 1 }}
              value={hotkey}
              onChange={(e) => setHotkey(e.target.value)}
              placeholder="CmdOrCtrl+Shift+V"
            />
            <Button variant="outline" onPress={applyHotkey}>Apply</Button>
          </div>
        </Row>
        <Row label="Wake phrase" hint="Say this phrase to activate voice mode">
          <input
            style={nativeInput}
            value={s.voice_wake_word ?? ""}
            onChange={(e) => patch("voice_wake_word", e.target.value)}
            placeholder="goose"
            disabled={calibrating}
          />
        </Row>

        {/* Calibration status */}
        {!calibrating && wakePhrase && (
          <div style={calibrationRow}>
            <span
              style={{
                ...calibrationDot,
                background: isCalibrated ? "var(--color-success)" : "var(--color-warning)",
              }}
            />
            <span style={calibrationLabel}>
              {isCalibrated
                ? `Calibrated (${transcriptions.length} variant${transcriptions.length !== 1 ? "s" : ""})`
                : "Not calibrated"}
            </span>
            <div style={{ marginLeft: "auto", display: "flex", gap: "var(--space-2)" }}>
              {isCalibrated && (
                <Button variant="outline" size="sm" onPress={clearCalibration}>Clear</Button>
              )}
              <Button variant="outline" size="sm" onPress={startCalibration}>
                {isCalibrated ? "Re-calibrate" : "Calibrate"}
              </Button>
            </div>
          </div>
        )}

        {/* Inline calibration component */}
        {calibrating && wakePhrase && (
          <Suspense fallback={<p className="muted-12">Loading calibration...</p>}>
            <WakeWordCalibration
              phrase={wakePhrase}
              onComplete={handleCalibrationComplete}
              onCancel={() => setCalibrating(false)}
            />
          </Suspense>
        )}

        {/* Wake word info */}
        {!calibrating && (
          <div style={wakeWordNote}>
            <span style={wakeWordNoteIcon}>i</span>
            <span>
              {wakePhrase
                ? "Calibrating improves detection accuracy by learning how Whisper transcribes your voice. Record 3-5 samples for best results."
                : "When set, the app listens passively while Voice mode is open and activates automatically when the phrase is heard. Leave blank to use the keyboard shortcut only."}
            </span>
          </div>
        )}
      </Section>

      {/* Recording */}
      <Section title="Recording">
        <Row label={`Max listen time: ${recDur}s`} hint="Auto-stops recording after this duration">
          <input
            type="range"
            min={5}
            max={120}
            step={5}
            value={recDur}
            onChange={(e) => patch("voice_recording_duration_secs", Number(e.target.value))}
            style={{ width: "100%" }}
          />
        </Row>
      </Section>

      {/* Advanced toggle */}
      <button
        style={advancedToggleStyle}
        onClick={() => setShowAdvanced((v) => !v)}
        aria-expanded={showAdvanced}
      >
        {showAdvanced ? "\u25BE" : "\u25B8"} Advanced voice settings
      </button>

      {showAdvanced && (
        <>
          <Section title="Transcription">
            <Row label="Server address" hint="Where the speech-to-text server is running">
              <input
                style={nativeInput}
                value={s.voice_whisper_url ?? ""}
                onChange={(e) => patch("voice_whisper_url", e.target.value)}
                placeholder="http://127.0.0.1:9000"
              />
            </Row>
            <Row label="Model file" hint="Speech recognition model (e.g. ggml-base.bin)">
              <input
                style={nativeInput}
                value={s.active_whisper_model ?? ""}
                onChange={(e) => patch("active_whisper_model", e.target.value)}
                placeholder="ggml-base.bin"
              />
            </Row>
          </Section>

          <Section title="Wake-Word Detection">
            <Row label="KWS Whisper URL" hint="Separate whisper server for fast wake-word detection (uses tiny model). Leave blank to share the main server">
              <input
                style={nativeInput}
                value={s.voice_kws_whisper_url ?? ""}
                onChange={(e) => patch("voice_kws_whisper_url", e.target.value || null)}
                placeholder="Same as transcription server"
              />
            </Row>
            <Row label={`Energy threshold: ${(s.voice_kws_energy_threshold ?? 0.003).toFixed(3)}`} hint="Minimum audio energy to trigger whisper (0 = disabled, 0.003 = default)">
              <input
                type="range"
                min={0}
                max={0.05}
                step={0.001}
                value={s.voice_kws_energy_threshold ?? 0.003}
                onChange={(e) => patch("voice_kws_energy_threshold", Number(e.target.value))}
                style={{ width: "100%" }}
              />
            </Row>
            <Row label={`Silence cutoff: ${s.voice_kws_post_trigger_silence_ms ?? 400}ms`} hint="Consecutive silence that ends audio capture after wake word (0 = wait full duration)">
              <input
                type="range"
                min={0}
                max={2000}
                step={50}
                value={s.voice_kws_post_trigger_silence_ms ?? 400}
                onChange={(e) => patch("voice_kws_post_trigger_silence_ms", Number(e.target.value))}
                style={{ width: "100%" }}
              />
            </Row>
            <Row label={`Cooldown: ${s.voice_kws_cooldown_ms ?? 2000}ms`} hint="Delay before re-arming detection after activation (prevents TTS echo re-trigger)">
              <input
                type="range"
                min={500}
                max={5000}
                step={100}
                value={s.voice_kws_cooldown_ms ?? 2000}
                onChange={(e) => patch("voice_kws_cooldown_ms", Number(e.target.value))}
                style={{ width: "100%" }}
              />
            </Row>
          </Section>

          <Section title="Speech Synthesis">
            <Row label="Voice model" hint="Piper voice model file (.onnx)">
              <input
                style={nativeInput}
                value={s.active_tts_model ?? ""}
                onChange={(e) => patch("active_tts_model", e.target.value)}
                placeholder="en_US-lessac-medium.onnx"
              />
            </Row>
            <Row label="Voice name">
              <div style={{ display: "flex", gap: "var(--space-2)", alignItems: "center" }}>
                <input
                  style={{ ...nativeInput, flex: 1 }}
                  value={s.voice_tts_voice ?? ""}
                  onChange={(e) => patch("voice_tts_voice", e.target.value)}
                  placeholder="en_US-lessac-medium.onnx"
                />
                <Button variant="outline" isDisabled title="Coming soon">Preview</Button>
              </div>
            </Row>
          </Section>
        </>
      )}
    </>
  );
}

// Helper that shows the current model assignment and a "Change{"\u2026"}" button
function ModelRoleRow({
  provider,
  model,
  onPick,
}: {
  provider?: string | null;
  model?: string | null;
  onPick: () => void;
}) {
  const label = provider && model ? `${provider} / ${model}` : "Not set";
  return (
    <div className="model-picker">
      <span className="model-picker__current" style={{
        fontFamily: "var(--font-mono)",
        fontSize: "var(--text-sm)",
        color: provider ? "var(--fg)" : "var(--grey-500)",
        overflow: "hidden",
        textOverflow: "ellipsis",
        whiteSpace: "nowrap",
      }}>
        {label}
      </span>
      <Button variant="outline" onPress={onPick}>Change{"\u2026"}</Button>
    </div>
  );
}

function ModelsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [toolModels, setToolModels] = useState<string[]>([]);

  // Seed model fields from live active roles if settings don't already have them
  useEffect(() => {
    api.getActiveRoles().then((roles) => {
      if (!s.chat_provider && roles.chat) {
        patch("chat_provider", roles.chat.provider);
        patch("chat_model", roles.chat.model);
      }
    }).catch(() => {/* non-fatal */});

    // Fetch available GGUF models for the tool-caller dropdown
    api.listModels().then((models) => {
      const gguf = models
        .filter((m) => m.provider === "gguf" || m.provider === "local")
        .map((m) => m.name);
      setToolModels(gguf);
    }).catch(() => {/* non-fatal */});
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const temp = s.llm_temperature ?? 0.7;
  const maxTokenOpts = [128, 256, 512, 1024, 2048, 4096];

  return (
    <>
      <Section title="AI Models">
        <Row label="Main LLM" hint="Handles all conversation, reasoning, and response generation">
          <ModelRoleRow
            provider={s.chat_provider}
            model={s.chat_model}
            onPick={() => setPickerOpen(true)}
          />
        </Row>

        <Row label="Tool Caller" hint="Small specialist model for structured tool-call arguments (optional)">
          <select
            value={s.tool_model ?? ""}
            onChange={(e) => patch("tool_model", e.target.value || null)}
            style={{ width: "100%", padding: "var(--space-2)", borderRadius: "var(--radius-2)", border: "1px solid var(--border)", background: "var(--surface)" }}
          >
            <option value="">None (use main LLM for tool calls)</option>
            {toolModels.map((name) => (
              <option key={name} value={name}>{name}</option>
            ))}
          </select>
          {(!s.tool_model || s.tool_model === s.chat_model) && (
            <span style={{ fontSize: "0.75rem", color: "var(--text-secondary)", marginTop: 4, display: "block" }}>
              Same model as main LLM — zero swap overhead
            </span>
          )}
        </Row>
      </Section>

      <Section title="Response Quality">
        <Row label="Thinking Mode" hint="Enable internal reasoning for better analysis, planning, and complex answers">
          <select
            style={selectFallback}
            value={s.thinking_mode ?? "auto"}
            onChange={(e) => patch("thinking_mode", e.target.value)}
          >
            <option value="auto">Auto (enable for capable models)</option>
            <option value="on">Always On</option>
            <option value="off">Off</option>
          </select>
        </Row>
        <Row label="Show thinking" hint="Display the model's reasoning process in chat bubbles">
          <Switch
            isSelected={s.show_thinking ?? false}
            onChange={(v) => patch("show_thinking", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Answer Review" hint="Adversarial critic reviews answers for completeness, accuracy, and depth before delivery">
          <select
            style={selectFallback}
            value={s.review_mode ?? "off"}
            onChange={(e) => patch("review_mode", e.target.value)}
          >
            <option value="off">Off</option>
            <option value="auto">Auto (review factual/analytical questions only)</option>
            <option value="on">Always On (review every answer)</option>
          </select>
        </Row>
        <Row label="Review rounds" hint="Maximum review-revision cycles before accepting (1-3)">
          <select
            style={selectFallback}
            value={s.review_max_rounds ?? 1}
            onChange={(e) => patch("review_max_rounds", Number(e.target.value))}
          >
            <option value={1}>1 round</option>
            <option value={2}>2 rounds</option>
            <option value={3}>3 rounds</option>
          </select>
        </Row>
        <Row label="Review quality bar" hint="Minimum score (1-5) to accept an answer without revision">
          <select
            style={selectFallback}
            value={s.review_pass_threshold ?? 3}
            onChange={(e) => patch("review_pass_threshold", Number(e.target.value))}
          >
            <option value={2}>2 - Lenient</option>
            <option value={3}>3 - Balanced (default)</option>
            <option value={4}>4 - Strict</option>
            <option value={5}>5 - Very Strict</option>
          </select>
        </Row>
        <Row label={`Creativity: ${temp.toFixed(1)}`} hint="Higher = more creative; lower = more focused and consistent">
          <input
            type="range"
            min={0}
            max={2}
            step={0.1}
            value={temp}
            onChange={(e) => patch("llm_temperature", Number(e.target.value))}
            style={{ width: "100%" }}
          />
        </Row>
        <Row label="Response length" hint="Maximum length of each response">
          <select
            style={selectFallback}
            value={s.llm_max_tokens ?? 1024}
            onChange={(e) => patch("llm_max_tokens", Number(e.target.value))}
          >
            {maxTokenOpts.map((n) => <option key={n} value={n}>{n.toLocaleString()} tokens</option>)}
          </select>
        </Row>
      </Section>

      <Section title="Context & Embeddings">
        <Row label="Context window override" hint="Override the model's context window size in tokens (0 = use model default)">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "120px" }}
              min={0}
              max={131072}
              value={s.context_window_override ?? 0}
              onChange={(e) => patch("context_window_override", Number(e.target.value))}
            />
            <span className="muted-12">tokens</span>
          </div>
        </Row>
        <Row label="Embedding provider" hint="Provider for text embeddings used by memory search">
          <select
            style={selectFallback}
            value={s.embedding_provider ?? "fastembed"}
            onChange={(e) => patch("embedding_provider", e.target.value)}
          >
            <option value="fastembed">FastEmbed (local ONNX)</option>
            <option value="none">None</option>
          </select>
        </Row>
        <Row label="Embedding model" hint="Active embedding model name from the registry">
          <input
            style={{ ...nativeInput, opacity: (s.embedding_provider ?? "fastembed") !== "none" ? 1 : 0.45 }}
            disabled={(s.embedding_provider ?? "fastembed") === "none"}
            value={s.active_embedding_model ?? ""}
            onChange={(e) => patch("active_embedding_model", e.target.value)}
            placeholder="all-MiniLM-L6-v2"
          />
        </Row>
      </Section>

      {pickerOpen && (
        <ModelPickerModal
          role="chat"
          currentProvider={s.chat_provider}
          currentModel={s.chat_model}
          onSelect={(provider, model) => {
            patch("chat_provider", provider);
            patch("chat_model", model);
            setPickerOpen(false);
          }}
          onClose={() => setPickerOpen(false)}
        />
      )}
    </>
  );
}

function PromptsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [customEnabled, setCustomEnabled] = useState(!!s.custom_system_prompt);
  const addendum = s.prompt_addendum ?? "";
  const customPrompt = s.custom_system_prompt ?? "";

  return (
    <>
      <Section title="Prompt Style">
        <RadioGroup
          aria-label="Prompt style"
          value={s.prompt_style ?? "balanced"}
          onChange={(v) => patch("prompt_style", v)}
        >
          <Radio value="balanced">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Balanced</div>
                <div className="row__hint">Natural conversation, medium length responses</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="concise">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Concise</div>
                <div className="row__hint">Short, direct answers. Minimal explanation</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="technical">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Technical</div>
                <div className="row__hint">Precise, detailed. Favors accuracy over brevity</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="warm">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Warm</div>
                <div className="row__hint">Friendly, encouraging tone. Conversational style</div>
              </div>
            </Radio.Content>
          </Radio>
        </RadioGroup>
      </Section>

      <Section title="Prompt Addendum">
        <Row label="Additional context" hint="Appended to every system prompt">
          <div style={{ position: "relative" }}>
            <textarea
              style={{ ...nativeInput, height: "80px", resize: "vertical" as const, width: "100%" }}
              value={addendum}
              maxLength={500}
              onChange={(e) => patch("prompt_addendum", e.target.value)}
              placeholder="Extra instructions appended to every request..."
            />
            <span style={charCounter}>{addendum.length}/500</span>
          </div>
        </Row>
      </Section>

      <Section title="Custom System Prompt">
        <Row label="Enable custom prompt">
          <Switch
            isSelected={customEnabled}
            onChange={(v) => {
              setCustomEnabled(v);
              if (!v) patch("custom_system_prompt", null);
            }}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="System prompt" hint="Replaces the built-in system prompt entirely">
          <div style={{ position: "relative" }}>
            <textarea
              style={{ ...nativeInput, height: "140px", resize: "vertical" as const, width: "100%", opacity: customEnabled ? 1 : 0.45 }}
              disabled={!customEnabled}
              value={customPrompt}
              maxLength={4000}
              onChange={(e) => patch("custom_system_prompt", e.target.value)}
              placeholder="You are a helpful AI assistant..."
            />
            {customEnabled && <span style={charCounter}>{customPrompt.length}/4000</span>}
          </div>
        </Row>
      </Section>
    </>
  );
}

function LocationTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const enabled = s.weather_enabled ?? false;

  return (
    <>
      <Section title="Weather">
        <Row label="Enable weather" hint="Allow the assistant to fetch current weather data">
          <Switch
            isSelected={enabled}
            onChange={(v) => patch("weather_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Enable weather
          </Switch>
        </Row>
        <Row label="Location name" hint="Human-readable name (e.g. Nairobi, Kenya)">
          <input
            style={{ ...nativeInput, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_location_name ?? ""}
            onChange={(e) => patch("weather_location_name", e.target.value)}
            placeholder="Nairobi, Kenya"
          />
        </Row>
        <Row label="Latitude">
          <input
            type="number"
            role="spinbutton"
            step={0.0001}
            style={{ ...nativeInput, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_latitude ?? ""}
            onChange={(e) => patch("weather_latitude", Number(e.target.value))}
            placeholder="-1.2921"
          />
        </Row>
        <Row label="Longitude">
          <input
            type="number"
            role="spinbutton"
            step={0.0001}
            style={{ ...nativeInput, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_longitude ?? ""}
            onChange={(e) => patch("weather_longitude", Number(e.target.value))}
            placeholder="36.8219"
          />
        </Row>
      </Section>
    </>
  );
}

function MemoryTuningSection({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [showTuning, setShowTuning] = useState(false);

  return (
    <>
      <button
        style={advancedToggleStyle}
        onClick={() => setShowTuning((v) => !v)}
        aria-expanded={showTuning}
      >
        {showTuning ? "\u25BE" : "\u25B8"} Advanced memory tuning
      </button>

      {showTuning && (
        <Section title="Memory Tuning">
          <Row label="Max facts per turn" hint="Maximum memories extracted per conversation turn (1-10)">
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "80px" }}
              min={1}
              max={10}
              value={s.memory_extraction_max_facts ?? 3}
              onChange={(e) => patch("memory_extraction_max_facts", Number(e.target.value))}
            />
          </Row>
          <Row label="Extraction cooldown" hint="Minimum seconds between extraction runs">
            <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
              <input
                type="number"
                role="spinbutton"
                style={{ ...nativeInput, width: "80px" }}
                min={1}
                max={300}
                value={s.memory_extraction_interval_secs ?? 10}
                onChange={(e) => patch("memory_extraction_interval_secs", Number(e.target.value))}
              />
              <span className="muted-12">seconds</span>
            </div>
          </Row>
          <Row label="Cleanup interval" hint="How often the cleanup task runs">
            <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
              <input
                type="number"
                role="spinbutton"
                style={{ ...nativeInput, width: "80px" }}
                min={1}
                max={168}
                value={s.memory_cleanup_interval_hours ?? 6}
                onChange={(e) => patch("memory_cleanup_interval_hours", Number(e.target.value))}
              />
              <span className="muted-12">hours</span>
            </div>
          </Row>
          <Row label="Consolidation interval" hint="How often the consolidation task runs">
            <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
              <input
                type="number"
                role="spinbutton"
                style={{ ...nativeInput, width: "80px" }}
                min={1}
                max={168}
                value={s.memory_consolidation_interval_hours ?? 24}
                onChange={(e) => patch("memory_consolidation_interval_hours", Number(e.target.value))}
              />
              <span className="muted-12">hours</span>
            </div>
          </Row>
          <Row label="Consolidation batch" hint="Max memories processed per consolidation pass">
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "80px" }}
              min={5}
              max={100}
              value={s.memory_consolidation_batch_size ?? 20}
              onChange={(e) => patch("memory_consolidation_batch_size", Number(e.target.value))}
            />
          </Row>
          <Row label={`Prune threshold: ${(s.memory_prune_threshold ?? 0.05).toFixed(2)}`} hint="Memories below this effective score are deleted">
            <input
              type="range"
              min={0}
              max={0.5}
              step={0.01}
              value={s.memory_prune_threshold ?? 0.05}
              onChange={(e) => patch("memory_prune_threshold", Number(e.target.value))}
              style={{ width: "100%" }}
            />
          </Row>
          <Row label={`Archive threshold: ${(s.memory_archive_threshold ?? 0.15).toFixed(2)}`} hint="Memories below this effective score are archived (hidden)">
            <input
              type="range"
              min={0}
              max={1.0}
              step={0.01}
              value={s.memory_archive_threshold ?? 0.15}
              onChange={(e) => patch("memory_archive_threshold", Number(e.target.value))}
              style={{ width: "100%" }}
            />
          </Row>
        </Section>
      )}
    </>
  );
}

function AgentTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const memInject = s.agent_memory_inject ?? false;

  return (
    <>
      <Section title="How thorough should Pond be?">
        <RadioGroup
          aria-label="Agent mode"
          value={s.agent_goose_mode ?? "auto"}
          onChange={(v) => patch("agent_goose_mode", v)}
        >
          <Radio value="auto">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Smart (recommended)</div>
                <div className="row__hint">Pond decides when to look things up or take actions</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="chat">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Chat only</div>
                <div className="row__hint">Conversation only -- Pond won't use any tools</div>
              </div>
            </Radio.Content>
          </Radio>
          <Radio value="smart">
            <Radio.Control><Radio.Indicator /></Radio.Control>
            <Radio.Content>
              <div>
                <div style={{ fontWeight: 500 }}>Proactive</div>
                <div className="row__hint">Pond actively uses tools to give more detailed answers</div>
              </div>
            </Radio.Content>
          </Radio>
        </RadioGroup>
      </Section>

      <Section title="Behaviour">
        <Row label="Fast path" hint="Answer trivial messages (greetings, thanks) instantly without invoking the LLM">
          <Switch
            isSelected={s.fast_path_enabled ?? true}
            onChange={(v) => patch("fast_path_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Instant replies for simple messages
          </Switch>
        </Row>
        <Row label="How thorough" hint="How many steps Pond will take to answer a question (1-50)">
          <input
            type="number"
            role="spinbutton"
            style={nativeInput}
            min={1}
            max={50}
            value={s.agent_max_turns ?? 20}
            onChange={(e) => patch("agent_max_turns", Number(e.target.value))}
          />
        </Row>
        <Row label="Timeout" hint="Maximum seconds before a response is cut off (0 = no limit)">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "100px" }}
              min={0}
              max={3600}
              value={s.agent_timeout_secs ?? 300}
              onChange={(e) => patch("agent_timeout_secs", Number(e.target.value))}
            />
            <span className="muted-12">seconds</span>
          </div>
        </Row>
        <Row label="Tool output compaction" hint="Compress tool results to save context tokens (disable for debugging)">
          <Switch
            isSelected={s.tool_output_compaction ?? true}
            onChange={(v) => patch("tool_output_compaction", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Compact tool outputs
          </Switch>
        </Row>
        <Row label="KV cache reuse" hint="Keep system prompt stable for faster inference on local models">
          <Switch
            isSelected={s.prefix_cache_prompt ?? true}
            onChange={(v) => patch("prefix_cache_prompt", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Prompt prefix caching
          </Switch>
        </Row>
      </Section>

      <Section title="Memory">
        <Row label="Remember context" hint="Pond recalls facts from past conversations to give better answers">
          <Switch
            isSelected={memInject}
            onChange={(v) => patch("agent_memory_inject", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Use conversation memory
          </Switch>
        </Row>
        <Row label="How much to recall" hint="Number of past memories to include (1-20)">
          <input
            type="number"
            role="spinbutton"
            style={{ ...nativeInput, opacity: memInject ? 1 : 0.45 }}
            disabled={!memInject}
            min={1}
            max={20}
            value={s.agent_memory_limit ?? 5}
            onChange={(e) => patch("agent_memory_limit", Number(e.target.value))}
          />
        </Row>
        <Row label="Auto-extract memories" hint="Automatically learn facts from each conversation turn">
          <Switch
            isSelected={s.memory_extraction_enabled ?? true}
            onChange={(v) => patch("memory_extraction_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Extract memories
          </Switch>
        </Row>
        <Row label="Memory cleanup" hint="Periodically prune and archive decayed memories">
          <Switch
            isSelected={s.memory_cleanup_enabled ?? true}
            onChange={(v) => patch("memory_cleanup_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Auto-cleanup
          </Switch>
        </Row>
        <Row label="Auto-consolidation" hint="Merge duplicate/contradicting memories during idle periods (15 min inactivity)">
          <Switch
            isSelected={s.memory_consolidation_enabled ?? false}
            onChange={(v) => patch("memory_consolidation_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Consolidate memories
          </Switch>
        </Row>
        <Row label="Consolidation mode" hint="Single-pass (1 LLM call, fast) or Adversarial (Proposer/Adversary/Judge, thorough)">
          <select
            style={selectFallback}
            value={s.memory_consolidation_mode ?? "single"}
            onChange={(e) => patch("memory_consolidation_mode", e.target.value)}
          >
            <option value="single">Single-pass (fast)</option>
            <option value="adversarial">Adversarial (3-stage)</option>
          </select>
        </Row>
        <Row label="Memory graph" hint="Experimental: use causal graph traversal for memory retrieval">
          <Switch
            isSelected={s.memory_graph_enabled ?? false}
            onChange={(v) => patch("memory_graph_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Graph-based recall
          </Switch>
        </Row>
      </Section>

      <MemoryTuningSection s={s} patch={patch} />
    </>
  );
}

function DataTab({
  s,
  patch,
  serverUrl,
  onServerUrlChange,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  serverUrl: string;
  onServerUrlChange: (v: string) => void;
}) {
  return (
    <>
      <Section title="Data Retention">
        <Row label="Event logs" hint="How many days to keep event log entries">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "80px" }}
              min={1}
              max={365}
              value={s.retention_event_log_days ?? 30}
              onChange={(e) => patch("retention_event_log_days", Number(e.target.value))}
            />
            <span className="muted-12">days</span>
          </div>
        </Row>
        <Row label="Sensor readings" hint="How many days to keep sensor data">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "80px" }}
              min={1}
              max={365}
              value={s.retention_sensor_days ?? 7}
              onChange={(e) => patch("retention_sensor_days", Number(e.target.value))}
            />
            <span className="muted-12">days</span>
          </div>
        </Row>
        <Row label="Session messages" hint="Maximum messages to keep per session">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              role="spinbutton"
              style={{ ...nativeInput, width: "100px" }}
              min={10}
              max={10000}
              value={s.retention_session_messages_keep ?? 500}
              onChange={(e) => patch("retention_session_messages_keep", Number(e.target.value))}
            />
            <span className="muted-12">messages</span>
          </div>
        </Row>
      </Section>

      <Section title="Telemetry & Monitoring">
        <Row label="Telemetry" hint="Record per-turn metrics (TTFT, token counts, tool latency)">
          <Switch
            isSelected={s.telemetry_enabled ?? true}
            onChange={(v) => patch("telemetry_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Record telemetry
          </Switch>
        </Row>
        <Row label="Context monitoring" hint="Track context window fill rate and warn before saturation">
          <Switch
            isSelected={s.context_monitor_enabled ?? true}
            onChange={(v) => patch("context_monitor_enabled", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Monitor context usage
          </Switch>
        </Row>
        <Row label="Compact encoding" hint="Use compact encoding for prompts to reduce token count by 30-60%">
          <Switch
            isSelected={s.compact_encoding ?? true}
            onChange={(v) => patch("compact_encoding", v)}
          >
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Compact encoding
          </Switch>
        </Row>
      </Section>

      <Section title="Cloud Cost Comparison">
        <Row label="Input price" hint="Cloud API input token price per million (for savings estimate)">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <span className="muted-12">$</span>
            <input
              type="number"
              role="spinbutton"
              step={0.1}
              style={{ ...nativeInput, width: "100px" }}
              min={0}
              value={s.cloud_input_price_per_million ?? 2.5}
              onChange={(e) => patch("cloud_input_price_per_million", Number(e.target.value))}
            />
            <span className="muted-12">/ 1M tokens</span>
          </div>
        </Row>
        <Row label="Output price" hint="Cloud API output token price per million">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <span className="muted-12">$</span>
            <input
              type="number"
              role="spinbutton"
              step={0.1}
              style={{ ...nativeInput, width: "100px" }}
              min={0}
              value={s.cloud_output_price_per_million ?? 10.0}
              onChange={(e) => patch("cloud_output_price_per_million", Number(e.target.value))}
            />
            <span className="muted-12">/ 1M tokens</span>
          </div>
        </Row>
      </Section>

      <Section title="Desktop">
        <Row label="Server URL" hint="pond-server base URL">
          <input
            style={nativeInput}
            value={serverUrl}
            onChange={(e) => onServerUrlChange(e.target.value)}
            placeholder="http://127.0.0.1:4000"
          />
        </Row>
      </Section>
    </>
  );
}

// ── Main Settings Component ───────────────────────────────────

export function Settings() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const [tab, setTab]           = useState<SettingsTab>("identity");
  const [settings, setSettings] = useState<Partial<SettingsType>>({});
  const [loading, setLoading]   = useState(true);
  const [saving, setSaving]     = useState(false);
  const [saved, setSaved]       = useState(false);
  const [error, setError]       = useState<string | null>(null);
  const [hotkey, setHotkey]     = useState("CmdOrCtrl+Shift+V");

  useEffect(() => {
    api.getSettings()
      .then((s) => setSettings(s))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  async function save() {
    setSaving(true);
    setSaved(false);
    setError(null);
    try {
      const updated = await api.updateSettings(settings);
      setSettings(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  }

  async function applyHotkey() {
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!isTauri) return;
    try { await invoke("set_hotkey", { hotkey }); }
    catch (e) { setError(String(e)); }
  }

  function patch(key: keyof SettingsType, value: unknown) {
    setSettings((prev) => ({ ...prev, [key]: value }));
  }

  return (
    <div className="screen">
      <PageHeader
        title="Settings"
        action={
          <Button variant="primary" onPress={save} isDisabled={saving || loading}>
            {saving ? "Saving..." : saved ? "Saved" : "Save Settings"}
          </Button>
        }
      />

      {/* Tab bar */}
      <Tabs
        selectedKey={tab}
        onSelectionChange={(k) => setTab(k as SettingsTab)}
      >
        <Tabs.ListContainer>
          <Tabs.List aria-label="Settings sections" className="settings-tabs">
            {TABS.map((t) => (
              <Tabs.Tab key={t.id} id={t.id} onClick={() => setTab(t.id)}>
                <Tabs.Indicator />
                {t.label}
              </Tabs.Tab>
            ))}
          </Tabs.List>
        </Tabs.ListContainer>
      </Tabs>

      {/* Error banner */}
      {error && <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0, flexShrink: 0 }}>{error}</p>}

      {/* Panel area */}
      <div className="settings-body">
        {loading ? (
          <p className="muted-12">Loading settings...</p>
        ) : (
          <>
            {tab === "identity"  && <IdentityTab  s={settings} patch={patch} />}
            {tab === "voice"     && <VoiceTab s={settings} patch={patch} hotkey={hotkey} setHotkey={setHotkey} applyHotkey={applyHotkey} refreshSettings={async () => { try { const u = await api.getSettings(); setSettings(u); } catch { /* ignore */ } }} />}
            {tab === "models"    && <ModelsTab    s={settings} patch={patch} />}
            {tab === "prompts"   && <PromptsTab   s={settings} patch={patch} />}
            {tab === "location"  && <LocationTab  s={settings} patch={patch} />}
            {tab === "agent"     && <AgentTab     s={settings} patch={patch} />}
            {tab === "data"      && <DataTab s={settings} patch={patch} serverUrl={state.serverUrl} onServerUrlChange={(v) => dispatch({ type: "SET_SERVER_URL", payload: v })} />}
            {tab === "tools"     && <ToolsTab s={settings} patch={patch} />}
          </>
        )}
      </div>
    </div>
  );
}

// ── Tools Tab ─────────────────────────────────────────────────

function ToolsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading]       = useState(true);
  const [error, setError]           = useState<string | null>(null);
  const [showForm, setShowForm]     = useState(false);
  const [addName, setAddName]       = useState("");
  const [addKind, setAddKind]       = useState<"stdio" | "sse">("stdio");
  const [addCmd, setAddCmd]         = useState("");
  const [adding, setAdding]         = useState(false);

  function load() {
    setLoading(true);
    api.listExtensions()
      .then((r) => setExtensions(r.extensions))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function toggle(name: string, enabled: boolean) {
    try {
      await api.toggleExtension(name, enabled);
      setExtensions((prev) => prev.map((e) => e.name === name ? { ...e, enabled } : e));
    } catch (e) { setError(String(e)); }
  }

  async function remove(name: string) {
    try { await api.removeExtension(name); load(); } catch (e) { setError(String(e)); }
  }

  async function add() {
    if (!addName.trim() || !addCmd.trim()) return;
    setAdding(true);
    try {
      await api.addExtension({ name: addName.trim(), kind: addKind, command: addCmd.trim() });
      setAddName(""); setAddCmd(""); setShowForm(false); load();
    } catch (e) { setError(String(e)); } finally { setAdding(false); }
  }

  return (
    <>
      <Section title="Built-in Tool Modules">
        <Row label="Memory" hint="Recall, save, and forget memories">
          <Switch isSelected={s.ext_memory_enabled ?? true} onChange={(v) => patch("ext_memory_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Schedules" hint="Create, manage, and run scheduled tasks">
          <Switch isSelected={s.ext_schedule_enabled ?? true} onChange={(v) => patch("ext_schedule_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Weather" hint="Fetch current weather data">
          <Switch isSelected={s.ext_weather_enabled ?? true} onChange={(v) => patch("ext_weather_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Knowledge" hint="Wikipedia search and article retrieval">
          <Switch isSelected={s.ext_knowledge_enabled ?? true} onChange={(v) => patch("ext_knowledge_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="System" hint="Shell commands, file access, notifications, system info">
          <Switch isSelected={s.ext_system_enabled ?? true} onChange={(v) => patch("ext_system_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Devices" hint="Device registry, profile info, model assignments">
          <Switch isSelected={s.ext_device_enabled ?? true} onChange={(v) => patch("ext_device_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="News" hint="Top stories and headline search">
          <Switch isSelected={s.ext_news_enabled ?? true} onChange={(v) => patch("ext_news_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Finance" hint="Stock quotes, crypto prices, and currency exchange rates">
          <Switch isSelected={s.ext_finance_enabled ?? true} onChange={(v) => patch("ext_finance_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
        <Row label="Discovery" hint="Country info, product lookup, web search">
          <Switch isSelected={s.ext_discovery_enabled ?? true} onChange={(v) => patch("ext_discovery_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
          </Switch>
        </Row>
      </Section>

      <Section title="Tool Behaviour">
        <Row label="Tool result cache" hint="Cache deterministic tool results to avoid redundant calls">
          <Switch isSelected={s.tool_cache_enabled ?? true} onChange={(v) => patch("tool_cache_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Cache tool results
          </Switch>
        </Row>
        <Row label="Tool call validation" hint="Validate and repair tool call JSON from small models before execution">
          <Switch isSelected={s.tool_call_validation ?? true} onChange={(v) => patch("tool_call_validation", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Validate tool calls
          </Switch>
        </Row>
        <Row label="Tool request detection" hint="Detect natural-language tool requests in LLM output and execute them">
          <Switch isSelected={s.tool_request_detection ?? true} onChange={(v) => patch("tool_request_detection", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Detect tool requests
          </Switch>
        </Row>
        <Row label="Multi-tool" hint="Experimental: detect and dispatch multiple tool intents concurrently">
          <Switch isSelected={s.multi_tool_enabled ?? false} onChange={(v) => patch("multi_tool_enabled", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Concurrent multi-tool
          </Switch>
        </Row>
      </Section>

      <Section title="Scheduling">
        <Row label="Result notifications" hint="Broadcast schedule results as desktop notifications">
          <Switch isSelected={s.schedule_result_notify ?? true} onChange={(v) => patch("schedule_result_notify", v)}>
            <Switch.Control><Switch.Thumb /></Switch.Control>
            Notify on completion
          </Switch>
        </Row>
        <Row label="Max concurrent" hint="Maximum scheduled tasks running simultaneously (1-10)">
          <input
            type="number"
            role="spinbutton"
            style={{ ...nativeInput, width: "80px" }}
            min={1}
            max={10}
            value={s.schedule_max_concurrent ?? 2}
            onChange={(e) => patch("schedule_max_concurrent", Number(e.target.value))}
          />
        </Row>
        <Row label="History per task" hint="Maximum execution history entries retained per schedule">
          <input
            type="number"
            role="spinbutton"
            style={{ ...nativeInput, width: "80px" }}
            min={5}
            max={500}
            value={s.schedule_max_runs_per_task ?? 50}
            onChange={(e) => patch("schedule_max_runs_per_task", Number(e.target.value))}
          />
        </Row>
      </Section>

      <Section title="External Extensions">
        <div style={{ display: "flex", gap: "var(--space-2)" }}>
          <Button variant="outline" onPress={() => setShowForm((v) => !v)}>
            <Plus size={14} /> Add Extension
          </Button>
        </div>
      </Section>

      {showForm && (
        <Card className="card">
          <CardContent style={{ display: "flex", flexDirection: "column", gap: "var(--space-3)" }}>
            <input
              style={nativeInput}
              value={addName}
              onChange={(e) => setAddName(e.target.value)}
              placeholder="Name (e.g. developer)"
              aria-label="Extension name"
            />
            <select
              style={selectFallback}
              value={addKind}
              onChange={(e) => setAddKind(e.target.value as "stdio" | "sse")}
            >
              <option value="stdio">stdio</option>
              <option value="sse">SSE</option>
            </select>
            <input
              style={nativeInput}
              value={addCmd}
              onChange={(e) => setAddCmd(e.target.value)}
              placeholder="Command or URI"
              aria-label="Extension command or URI"
            />
            <Button variant="primary" onPress={add} isDisabled={adding || !addName.trim() || !addCmd.trim()}>
              {adding ? "Adding..." : "Add"}
            </Button>
          </CardContent>
        </Card>
      )}

      {error && <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>{error}</p>}

      {loading ? (
        <p className="muted-12">Loading extensions...</p>
      ) : extensions.length === 0 ? (
        <p className="muted-12">No extensions configured. Add one above to enable tool use.</p>
      ) : (
        <div className="ext-list">
          {extensions.map((ext) => (
            <div key={ext.name} className="ext-row">
              <Switch isSelected={ext.enabled} onChange={() => toggle(ext.name, !ext.enabled)} aria-label={`Toggle ${ext.name}`}><Switch.Control><Switch.Thumb /></Switch.Control></Switch>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" as const }}>
                  <span className="ext-row__name">{ext.name}</span>
                  <Chip size="sm" variant="soft">{ext.kind}</Chip>
                  {!ext.enabled && <Chip size="sm" variant="soft" color="warning">Disabled</Chip>}
                </div>
                {ext.description && <p style={{ margin: "2px 0 4px", fontSize: "var(--text-xs)", color: "var(--grey-500)" }}>{ext.description}</p>}
                {ext.tools.length > 0 && (
                  <div style={{ display: "flex", flexWrap: "wrap" as const, gap: "4px", marginTop: "4px" }}>
                    {ext.tools.slice(0, 8).map((t) => (
                      <code key={t} style={{ fontSize: "11px", background: "var(--grey-50)", border: "1px solid var(--grey-200)", borderRadius: "4px", padding: "1px 5px", fontFamily: "var(--font-mono)" }}>
                        {t.replace(`${ext.name}__`, "")}
                      </code>
                    ))}
                    {ext.tools.length > 8 && <span className="muted-12">+{ext.tools.length - 8} more</span>}
                  </div>
                )}
              </div>
              <Button variant="danger-soft" onPress={() => remove(ext.name)} aria-label={`Remove ${ext.name}`}>
                <Trash2 size={14} />
              </Button>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

// ── Styles ────────────────────────────────────────────────────

/**
 * Native <input> and <select> styling for cases where HeroUI components
 * don't fit (e.g. type="number" needing role="spinbutton" for tests,
 * <select> with <option> children).
 */
const nativeInput: React.CSSProperties = {
  height: "38px",
  border: "1px solid var(--grey-200)",
  borderRadius: "10px",
  padding: "0 12px",
  fontSize: "var(--text-base)",
  fontFamily: "var(--font-body)",
  fontWeight: "var(--weight-regular)" as unknown as number,
  background: "#fff",
  color: "var(--fg)",
  width: "100%",
  userSelect: "text",
  transition: "border-color 150ms, box-shadow 150ms",
};

const selectFallback: React.CSSProperties = {
  ...nativeInput,
  cursor: "pointer",
  appearance: "none",
  backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
  backgroundRepeat: "no-repeat",
  backgroundPosition: "right 12px center",
  paddingRight: "32px",
};

const charCounter: React.CSSProperties = {
  position: "absolute",
  bottom: "6px",
  right: "8px",
  fontSize: "var(--text-xs)",
  color: "var(--grey-500)",
  pointerEvents: "none",
};

const wakeWordNote: React.CSSProperties = {
  display: "flex",
  alignItems: "flex-start",
  gap: "6px",
  padding: "8px 10px",
  background: "rgba(255,149,0,0.08)",
  border: "1px solid rgba(255,149,0,0.25)",
  borderRadius: "8px",
  fontSize: "var(--text-xs)",
  color: "var(--grey-600)",
  lineHeight: "1.5",
};

const wakeWordNoteIcon: React.CSSProperties = {
  color: "#FF9500",
  flexShrink: 0,
  marginTop: "1px",
  fontWeight: 700,
  fontStyle: "italic",
};

const calibrationRow: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: "6px 0",
};

const calibrationDot: React.CSSProperties = {
  width: "8px",
  height: "8px",
  borderRadius: "50%",
  flexShrink: 0,
};

const calibrationLabel: React.CSSProperties = {
  fontSize: "var(--text-sm)",
  fontFamily: "var(--font-body)",
  color: "var(--grey-600)",
};

const advancedToggleStyle: React.CSSProperties = {
  background: "none",
  border: "none",
  cursor: "pointer",
  fontSize: "var(--text-sm)",
  color: "var(--grey-600)",
  padding: "0",
  textAlign: "left",
  fontFamily: "var(--font-body)",
  display: "flex",
  alignItems: "center",
  gap: "var(--space-1)",
};
