import { useState, useEffect, useId } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api } from "../api/PondApiClient";
import { useAppDispatch, useAppState } from "../state/AppContext";
import type { Settings as SettingsType } from "../api/types";

// ── Tab definitions ───────────────────────────────────────────

type SettingsTab = "identity" | "voice" | "models" | "prompts" | "location" | "agent" | "data";

const TABS: Array<{ id: SettingsTab; label: string }> = [
  { id: "identity", label: "Identity" },
  { id: "voice",    label: "Voice" },
  { id: "models",   label: "Models" },
  { id: "prompts",  label: "Prompts" },
  { id: "location", label: "Location" },
  { id: "agent",    label: "Agent" },
  { id: "data",     label: "Data" },
];

// ── Shared form primitives ────────────────────────────────────

function FormSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section style={sectionStyles.root}>
      <h4 style={sectionStyles.title}>{title}</h4>
      <div style={sectionStyles.body}>{children}</div>
    </section>
  );
}

function FormRow({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div style={rowStyles.root}>
      <div style={rowStyles.labelCol}>
        <span style={rowStyles.label}>{label}</span>
        {hint && <span style={rowStyles.hint}>{hint}</span>}
      </div>
      <div style={rowStyles.control}>{children}</div>
    </div>
  );
}

interface ToggleProps {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
  label?: string;
}

function Toggle({ checked, onChange, disabled = false, label }: ToggleProps) {
  return (
    <label className="toggle-wrap">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        disabled={disabled}
      />
      <span className="toggle-track">
        <span className="toggle-thumb" />
      </span>
      {label && <span className="toggle-label">{label}</span>}
    </label>
  );
}

interface RadioOption<T extends string> {
  value: T;
  label: string;
  desc?: string;
}

interface RadioGroupProps<T extends string> {
  options: RadioOption<T>[];
  value: T | undefined;
  onChange: (v: T) => void;
  name: string;
}

function RadioGroup<T extends string>({ options, value, onChange, name }: RadioGroupProps<T>) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--space-2)" }}>
      {options.map((opt) => (
        <label
          key={opt.value}
          className={`radio-option${value === opt.value ? " selected" : ""}`}
          onClick={() => onChange(opt.value)}
        >
          <input
            type="radio"
            name={name}
            value={opt.value}
            checked={value === opt.value}
            onChange={() => onChange(opt.value)}
          />
          <div>
            <div className="radio-label">{opt.label}</div>
            {opt.desc && <div className="radio-desc">{opt.desc}</div>}
          </div>
        </label>
      ))}
    </div>
  );
}

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
    <div style={panelStyles.root}>
      <FormSection title="Personal">
        <FormRow label="Your name" hint="How Goose addresses you">
          <input
            style={inp}
            value={s.user_name ?? ""}
            onChange={(e) => patch("user_name", e.target.value)}
            placeholder="Friend"
          />
        </FormRow>
        <FormRow label="Assistant name" hint="What you call your assistant">
          <input
            style={inp}
            value={s.assistant_name ?? ""}
            onChange={(e) => patch("assistant_name", e.target.value)}
            placeholder="Goose"
          />
        </FormRow>
        <FormRow label="Timezone">
          <select
            style={inp}
            value={s.timezone ?? "UTC"}
            onChange={(e) => patch("timezone", e.target.value)}
          >
            {TIMEZONES.map((tz) => (
              <option key={tz} value={tz}>{tz}</option>
            ))}
          </select>
        </FormRow>
      </FormSection>

      <FormSection title="Personality">
        <FormRow label="Personality" hint="Describe how your assistant should behave">
          <textarea
            style={{ ...inp, height: "80px", resize: "vertical" }}
            value={s.assistant_personality ?? s.personality ?? ""}
            onChange={(e) => patch("assistant_personality", e.target.value)}
            placeholder="Friendly, concise, and helpful"
          />
        </FormRow>
      </FormSection>
    </div>
  );
}

function VoiceTab({
  s,
  patch,
  hotkey,
  setHotkey,
  applyHotkey,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  hotkey: string;
  setHotkey: (v: string) => void;
  applyHotkey: () => void;
}) {
  const recDur = s.voice_recording_duration_secs ?? 5;

  return (
    <div style={panelStyles.root}>
      <FormSection title="Wake Word">
        <FormRow label="Wake word" hint="Word or phrase that activates voice mode">
          <input
            style={inp}
            value={s.voice_wake_word ?? s.wake_word ?? ""}
            onChange={(e) => patch("voice_wake_word", e.target.value)}
            placeholder="goose"
          />
        </FormRow>
        <FormRow label="Voice hotkey" hint="Global keyboard shortcut to activate voice">
          <div style={{ display: "flex", gap: "var(--space-2)" }}>
            <input
              style={inp}
              value={hotkey}
              onChange={(e) => setHotkey(e.target.value)}
              placeholder="CmdOrCtrl+Shift+V"
            />
            <button style={inlineBtn} onClick={applyHotkey}>Apply</button>
          </div>
        </FormRow>
      </FormSection>

      <FormSection title="Recording">
        <FormRow label={`Max duration: ${recDur}s`} hint="Maximum microphone recording time">
          <input
            type="range"
            min={2}
            max={15}
            step={1}
            value={recDur}
            onChange={(e) => patch("voice_recording_duration_secs", Number(e.target.value))}
          />
        </FormRow>
      </FormSection>

      <FormSection title="Speech Recognition (STT)">
        <FormRow label="Whisper URL" hint="Default: http://127.0.0.1:9000">
          <input
            style={inp}
            value={s.voice_whisper_url ?? ""}
            onChange={(e) => patch("voice_whisper_url", e.target.value)}
            placeholder="http://127.0.0.1:9000"
          />
        </FormRow>
        <FormRow label="Whisper model" hint="e.g. ggml-base.bin">
          <input
            style={inp}
            value={s.active_whisper_model ?? ""}
            onChange={(e) => patch("active_whisper_model", e.target.value)}
            placeholder="ggml-base.bin"
          />
        </FormRow>
      </FormSection>

      <FormSection title="Text-to-Speech (TTS)">
        <FormRow label="TTS model" hint="Piper model filename">
          <input
            style={inp}
            value={s.active_tts_model ?? ""}
            onChange={(e) => patch("active_tts_model", e.target.value)}
            placeholder="en_US-lessac-medium.onnx"
          />
        </FormRow>
        <FormRow label="TTS voice" hint="Voice configuration file">
          <input
            style={inp}
            value={s.voice_tts_voice ?? ""}
            onChange={(e) => patch("voice_tts_voice", e.target.value)}
            placeholder="en_US-lessac-medium.onnx"
          />
        </FormRow>
      </FormSection>
    </div>
  );
}

function ModelsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [thinkSameAsChat, setThinkSameAsChat] = useState(
    !s.think_provider && !s.think_model
  );
  const [taskSameAsChat, setTaskSameAsChat] = useState(
    !s.task_provider && !s.task_model
  );

  const temp = s.llm_temperature ?? 0.7;
  const maxTokenOpts = [128, 256, 512, 1024, 2048, 4096];

  const llmProviderOpts = [
    { value: "llamafile" as const, label: "Llamafile", desc: "Local HTTP server compatible with OpenAI API" },
    { value: "ollama" as const, label: "Ollama", desc: "Local model server with broad model support" },
    { value: "local" as const, label: "Local (GGUF)", desc: "In-process GGUF inference — no server needed" },
    { value: "mock" as const, label: "Mock", desc: "Deterministic mock responses for testing" },
  ];

  return (
    <div style={panelStyles.root}>
      <FormSection title="LLM Provider">
        <RadioGroup
          name="llm_provider"
          options={llmProviderOpts}
          value={(s.llm_provider ?? "llamafile") as "llamafile" | "ollama" | "local" | "mock"}
          onChange={(v) => patch("llm_provider", v)}
        />
      </FormSection>

      <FormSection title="Chat Model">
        <FormRow label="Provider">
          <input style={inp} value={s.chat_provider ?? ""} onChange={(e) => patch("chat_provider", e.target.value)} placeholder="openai" />
        </FormRow>
        <FormRow label="Model">
          <input style={inp} value={s.chat_model ?? ""} onChange={(e) => patch("chat_model", e.target.value)} placeholder="gpt-4o-mini" />
        </FormRow>
      </FormSection>

      <FormSection title="Think Model">
        <FormRow label="Same as Chat">
          <Toggle
            checked={thinkSameAsChat}
            onChange={(v) => {
              setThinkSameAsChat(v);
              if (v) { patch("think_provider", null); patch("think_model", null); }
            }}
          />
        </FormRow>
        <FormRow label="Provider">
          <input style={{ ...inp, opacity: thinkSameAsChat ? 0.45 : 1 }} disabled={thinkSameAsChat} value={s.think_provider ?? ""} onChange={(e) => patch("think_provider", e.target.value)} placeholder="openai" />
        </FormRow>
        <FormRow label="Model">
          <input style={{ ...inp, opacity: thinkSameAsChat ? 0.45 : 1 }} disabled={thinkSameAsChat} value={s.think_model ?? ""} onChange={(e) => patch("think_model", e.target.value)} placeholder="o1-mini" />
        </FormRow>
      </FormSection>

      <FormSection title="Task Model">
        <FormRow label="Same as Chat">
          <Toggle
            checked={taskSameAsChat}
            onChange={(v) => {
              setTaskSameAsChat(v);
              if (v) { patch("task_provider", null); patch("task_model", null); }
            }}
          />
        </FormRow>
        <FormRow label="Provider">
          <input style={{ ...inp, opacity: taskSameAsChat ? 0.45 : 1 }} disabled={taskSameAsChat} value={s.task_provider ?? ""} onChange={(e) => patch("task_provider", e.target.value)} placeholder="openai" />
        </FormRow>
        <FormRow label="Model">
          <input style={{ ...inp, opacity: taskSameAsChat ? 0.45 : 1 }} disabled={taskSameAsChat} value={s.task_model ?? ""} onChange={(e) => patch("task_model", e.target.value)} placeholder="gpt-4o-mini" />
        </FormRow>
      </FormSection>

      <FormSection title="Inference Parameters">
        <FormRow label={`Temperature: ${temp.toFixed(1)}`} hint="Higher = more creative, lower = more focused">
          <input
            type="range"
            min={0}
            max={2}
            step={0.1}
            value={temp}
            onChange={(e) => patch("llm_temperature", Number(e.target.value))}
          />
        </FormRow>
        <FormRow label="Max tokens" hint="Maximum response length">
          <select style={inp} value={s.llm_max_tokens ?? 1024} onChange={(e) => patch("llm_max_tokens", Number(e.target.value))}>
            {maxTokenOpts.map((n) => <option key={n} value={n}>{n.toLocaleString()}</option>)}
          </select>
        </FormRow>
      </FormSection>
    </div>
  );
}

function PromptsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [customEnabled, setCustomEnabled] = useState(
    !!s.custom_system_prompt
  );
  const addendum = s.prompt_addendum ?? "";
  const customPrompt = s.custom_system_prompt ?? "";

  const styleOpts = [
    { value: "balanced" as const, label: "Balanced", desc: "Natural conversation, medium length responses" },
    { value: "concise" as const, label: "Concise", desc: "Short, direct answers. Minimal explanation" },
    { value: "technical" as const, label: "Technical", desc: "Precise, detailed. Favors accuracy over brevity" },
    { value: "warm" as const, label: "Warm", desc: "Friendly, encouraging tone. Conversational style" },
  ];

  return (
    <div style={panelStyles.root}>
      <FormSection title="Prompt Style">
        <RadioGroup
          name="prompt_style"
          options={styleOpts}
          value={(s.prompt_style ?? "balanced") as "balanced" | "concise" | "technical" | "warm"}
          onChange={(v) => patch("prompt_style", v)}
        />
      </FormSection>

      <FormSection title="Prompt Addendum">
        <FormRow label="Additional context" hint="Appended to every system prompt">
          <div style={{ position: "relative" }}>
            <textarea
              style={{ ...inp, height: "80px", resize: "vertical", width: "100%" }}
              value={addendum}
              maxLength={500}
              onChange={(e) => patch("prompt_addendum", e.target.value)}
              placeholder="Extra instructions appended to every request…"
            />
            <span style={charCounter}>{addendum.length}/500</span>
          </div>
        </FormRow>
      </FormSection>

      <FormSection title="Custom System Prompt">
        <FormRow label="Enable custom prompt">
          <Toggle
            checked={customEnabled}
            onChange={(v) => {
              setCustomEnabled(v);
              if (!v) patch("custom_system_prompt", null);
            }}
          />
        </FormRow>
        <FormRow label="System prompt" hint="Replaces the built-in system prompt entirely">
          <div style={{ position: "relative" }}>
            <textarea
              style={{ ...inp, height: "140px", resize: "vertical", width: "100%", opacity: customEnabled ? 1 : 0.45 }}
              disabled={!customEnabled}
              value={customPrompt}
              maxLength={4000}
              onChange={(e) => patch("custom_system_prompt", e.target.value)}
              placeholder="You are a helpful AI assistant…"
            />
            {customEnabled && <span style={charCounter}>{customPrompt.length}/4000</span>}
          </div>
        </FormRow>
      </FormSection>
    </div>
  );
}

function LocationTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const enabled = s.weather_enabled ?? false;

  return (
    <div style={panelStyles.root}>
      <FormSection title="Weather">
        <FormRow label="Enable weather" hint="Allow the assistant to fetch current weather data">
          <Toggle
            checked={enabled}
            onChange={(v) => patch("weather_enabled", v)}
          />
        </FormRow>
        <FormRow label="Location name" hint="Human-readable name (e.g. Nairobi, Kenya)">
          <input
            style={{ ...inp, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_location_name ?? ""}
            onChange={(e) => patch("weather_location_name", e.target.value)}
            placeholder="Nairobi, Kenya"
          />
        </FormRow>
        <FormRow label="Latitude">
          <input
            type="number"
            step={0.0001}
            style={{ ...inp, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_latitude ?? ""}
            onChange={(e) => patch("weather_latitude", Number(e.target.value))}
            placeholder="-1.2921"
          />
        </FormRow>
        <FormRow label="Longitude">
          <input
            type="number"
            step={0.0001}
            style={{ ...inp, opacity: enabled ? 1 : 0.45 }}
            disabled={!enabled}
            value={s.weather_longitude ?? ""}
            onChange={(e) => patch("weather_longitude", Number(e.target.value))}
            placeholder="36.8219"
          />
        </FormRow>
      </FormSection>
    </div>
  );
}

function AgentTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const memInject = s.agent_memory_inject ?? false;

  const modeOpts = [
    { value: "auto" as const, label: "Auto", desc: "Goose decides when to use tools" },
    { value: "chat" as const, label: "Chat only", desc: "Chat only — no tool calls" },
    { value: "smart" as const, label: "Smart", desc: "Proactively uses tools to complete tasks" },
  ];

  return (
    <div style={panelStyles.root}>
      <FormSection title="Agent Mode">
        <RadioGroup
          name="agent_goose_mode"
          options={modeOpts}
          value={(s.agent_goose_mode ?? "auto") as "auto" | "chat" | "smart"}
          onChange={(v) => patch("agent_goose_mode", v)}
        />
      </FormSection>

      <FormSection title="Limits">
        <FormRow label="Max turns" hint="Maximum agent tool-call turns per request (1–50)">
          <input
            type="number"
            style={inp}
            min={1}
            max={50}
            value={s.agent_max_turns ?? 20}
            onChange={(e) => patch("agent_max_turns", Number(e.target.value))}
          />
        </FormRow>
      </FormSection>

      <FormSection title="Memory">
        <FormRow label="Inject memories" hint="Include recent memories in every system prompt">
          <Toggle
            checked={memInject}
            onChange={(v) => patch("agent_memory_inject", v)}
          />
        </FormRow>
        <FormRow label="Memory limit" hint="Max number of memories to inject (1–20)">
          <input
            type="number"
            style={{ ...inp, opacity: memInject ? 1 : 0.45 }}
            disabled={!memInject}
            min={1}
            max={20}
            value={s.agent_memory_limit ?? 5}
            onChange={(e) => patch("agent_memory_limit", Number(e.target.value))}
          />
        </FormRow>
      </FormSection>
    </div>
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
    <div style={panelStyles.root}>
      <FormSection title="Data Retention">
        <FormRow label="Event logs" hint="How many days to keep event log entries">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              style={{ ...inp, width: "80px" }}
              min={1}
              max={365}
              value={s.retention_event_log_days ?? 30}
              onChange={(e) => patch("retention_event_log_days", Number(e.target.value))}
            />
            <span style={unitLabel}>days</span>
          </div>
        </FormRow>
        <FormRow label="Sensor readings" hint="How many days to keep sensor data">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              style={{ ...inp, width: "80px" }}
              min={1}
              max={365}
              value={s.retention_sensor_days ?? 7}
              onChange={(e) => patch("retention_sensor_days", Number(e.target.value))}
            />
            <span style={unitLabel}>days</span>
          </div>
        </FormRow>
        <FormRow label="Session messages" hint="Maximum messages to keep per session">
          <div style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
            <input
              type="number"
              style={{ ...inp, width: "100px" }}
              min={10}
              max={10000}
              value={s.retention_session_messages_keep ?? 500}
              onChange={(e) => patch("retention_session_messages_keep", Number(e.target.value))}
            />
            <span style={unitLabel}>messages</span>
          </div>
        </FormRow>
      </FormSection>

      <hr style={{ border: "none", borderTop: "1px solid var(--color-border)", margin: "var(--space-2) 0" }} />

      <FormSection title="Desktop">
        <FormRow label="Server URL" hint="pond-server base URL">
          <input
            style={inp}
            value={serverUrl}
            onChange={(e) => onServerUrlChange(e.target.value)}
            placeholder="http://127.0.0.1:4000"
          />
        </FormRow>
      </FormSection>
    </div>
  );
}

// ── Main Settings Component ───────────────────────────────────

export function Settings() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const [tab, setTab]         = useState<SettingsTab>("identity");
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
    <div style={styles.root}>
      {/* Tab bar — mirrors Agent.tsx pattern */}
      <div style={styles.tabs}>
        {TABS.map((t) => (
          <button
            key={t.id}
            style={{ ...styles.tab, ...(tab === t.id ? styles.tabActive : {}) }}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* Error banner */}
      {error && <p style={styles.error}>{error}</p>}

      {/* Panel area */}
      <div style={styles.panel}>
        {loading ? (
          <p style={styles.hint}>Loading settings…</p>
        ) : (
          <>
            {tab === "identity"  && <IdentityTab  s={settings} patch={patch} />}
            {tab === "voice"     && <VoiceTab s={settings} patch={patch} hotkey={hotkey} setHotkey={setHotkey} applyHotkey={applyHotkey} />}
            {tab === "models"    && <ModelsTab    s={settings} patch={patch} />}
            {tab === "prompts"   && <PromptsTab   s={settings} patch={patch} />}
            {tab === "location"  && <LocationTab  s={settings} patch={patch} />}
            {tab === "agent"     && <AgentTab     s={settings} patch={patch} />}
            {tab === "data"      && <DataTab s={settings} patch={patch} serverUrl={state.serverUrl} onServerUrlChange={(v) => dispatch({ type: "SET_SERVER_URL", payload: v })} />}
          </>
        )}
      </div>

      {/* Save bar */}
      <div style={styles.saveBar}>
        <button style={styles.saveBtn} onClick={save} disabled={saving || loading}>
          {saving ? "Saving…" : saved ? "Saved ✓" : "Save Settings"}
        </button>
      </div>
    </div>
  );
}

// ── Styles ────────────────────────────────────────────────────

const inp: React.CSSProperties = {
  height: "36px",
  border: "1px solid var(--color-border-strong)",
  borderRadius: "var(--radius-md)",
  padding: "0 var(--space-3)",
  fontSize: "var(--text-base)",
  fontFamily: "var(--font-body)",
  background: "var(--color-bg)",
  color: "var(--color-text)",
  width: "100%",
  userSelect: "text",
};

const inlineBtn: React.CSSProperties = {
  height: "36px",
  padding: "0 var(--space-4)",
  borderRadius: "var(--radius-md)",
  border: "1px solid var(--color-border-strong)",
  background: "transparent",
  cursor: "pointer",
  fontSize: "var(--text-base)",
  fontFamily: "var(--font-body)",
  fontWeight: 500,
  color: "var(--color-text)",
  flexShrink: 0,
  whiteSpace: "nowrap",
};

const charCounter: React.CSSProperties = {
  position: "absolute",
  bottom: "6px",
  right: "8px",
  fontSize: "var(--text-xs)",
  color: "var(--color-text-tertiary)",
  pointerEvents: "none",
};

const unitLabel: React.CSSProperties = {
  fontSize: "var(--text-base)",
  color: "var(--color-text-secondary)",
  whiteSpace: "nowrap",
};

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    height: "100%",
    gap: "var(--space-4)",
    maxWidth: "var(--content-max-width)",
  },
  tabs: {
    display: "flex",
    gap: "2px",
    borderBottom: "1px solid var(--color-border)",
    paddingBottom: "0",
    flexWrap: "wrap" as const,
    flexShrink: 0,
  },
  tab: {
    height: "32px",
    padding: "0 var(--space-3)",
    borderRadius: "var(--radius-sm) var(--radius-sm) 0 0",
    border: "none",
    background: "transparent",
    cursor: "pointer",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    color: "var(--color-text-secondary)",
    transition: "color var(--transition-fast)",
    borderBottom: "2px solid transparent",
    marginBottom: "-1px",
  },
  tabActive: {
    color: "var(--color-accent)",
    borderBottom: "2px solid var(--color-accent)",
  },
  error: {
    color: "var(--color-destructive)",
    fontSize: "var(--text-sm)",
    margin: 0,
    flexShrink: 0,
  },
  panel: {
    flex: 1,
    overflowY: "auto",
  },
  hint: {
    color: "var(--color-text-tertiary)",
    fontSize: "var(--text-sm)",
    margin: 0,
  },
  saveBar: {
    flexShrink: 0,
    paddingTop: "var(--space-3)",
    borderTop: "1px solid var(--color-border)",
  },
  saveBtn: {
    height: "36px",
    padding: "0 var(--space-6)",
    borderRadius: "var(--radius-md)",
    background: "var(--color-accent)",
    color: "#FFFFFF",
    border: "none",
    cursor: "pointer",
    fontSize: "var(--text-base)",
    fontWeight: 600,
    fontFamily: "var(--font-body)",
  },
};

const panelStyles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-5)",
  },
};

const sectionStyles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-3)",
  },
  title: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-base)",
    color: "var(--color-text)",
    margin: 0,
    paddingBottom: "var(--space-2)",
    borderBottom: "1px solid var(--color-border)",
  },
  body: {
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-3)",
  },
};

const rowStyles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    alignItems: "flex-start",
    gap: "var(--space-4)",
  },
  labelCol: {
    width: "160px",
    flexShrink: 0,
    paddingTop: "8px",
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  label: {
    fontSize: "var(--text-base)",
    color: "var(--color-text)",
    fontWeight: 500,
  },
  hint: {
    fontSize: "var(--text-xs)",
    color: "var(--color-text-tertiary)",
    lineHeight: "1.4",
  },
  control: {
    flex: 1,
    minWidth: 0,
  },
};
