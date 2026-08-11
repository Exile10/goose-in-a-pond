import { useState, useEffect, lazy, Suspense } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Switch,
  Button,
  Card,
  CardContent,
  Chip,
  RadioGroup,
  Radio,
} from "@heroui/react";
import { Trash2, Plus, LayoutGrid } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppDispatch, useAppState } from "../state/AppContext";
import type { Settings as SettingsType, Extension } from "../api/types";
import { ModelPickerModal } from "../components/ModelPickerModal";
import { Section, Row, ErrorBanner, SkeletonList } from "../components/shared";
import { HubIco } from "../hub/primitives/HubIco";
import { HP_PATHS } from "../hub/primitives/icons";
import { SETTINGS } from "../hub/data/settingsConfig";
import type { SettingsRowId } from "../hub/data/settingsConfig";
import type { GuiSection } from "../desktopState";
import { AppearanceView } from "../hub/views/settings/Appearance";
const WakeWordCalibration = lazy(() => import("../components/WakeWordCalibration").then(m => ({ default: m.WakeWordCalibration })));

// ── Patch diffing ─────────────────────────────────────────────
//
// `PUT /api/v1/settings` is a PATCH endpoint, and its key set is the server's
// only record of user INTENT: those keys are marked `is_user_set`, which
// permanently exempts them from future default-adoption migrations (see
// docs/developer/settings-defaults-and-user-intent.md).
//
// This panel batches edits behind one Save button, so it has to reconstruct
// that key set itself. Sending the whole loaded object instead — which is what
// it used to do — marked every setting as deliberately chosen on the first
// Save, from a click that changed nothing, and clobbered any field another
// surface (the Models tab, the phone, a calibration run) had changed since the
// panel loaded.

/**
 * Deep value equality for settings values.
 *
 * Compare by VALUE, not identity: several fields are arrays or maps
 * (`voice_wake_word_transcriptions`, `retention_events_by_category`) that the
 * UI replaces wholesale, so a reference check would report every one of them
 * as edited on every Save. Object key order is not significant (the server
 * serialises a `HashMap`); array order is. Settings are plain JSON, so no
 * cycle handling is needed.
 */
export function settingsValueEquals(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  const aIsArray = Array.isArray(a);
  if (aIsArray !== Array.isArray(b)) return false;
  if (aIsArray) {
    const av = a as unknown[];
    const bv = b as unknown[];
    return av.length === bv.length && av.every((v, i) => settingsValueEquals(v, bv[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const aKeys = Object.keys(ao);
  if (aKeys.length !== Object.keys(bo).length) return false;
  return aKeys.every(
    (k) => Object.prototype.hasOwnProperty.call(bo, k) && settingsValueEquals(ao[k], bo[k]),
  );
}

/**
 * The keys of `current` whose value differs from `baseline` — the last state
 * the server told us about. Keys the panel never touched are omitted, so an
 * untouched field is neither re-sent nor marked as chosen.
 *
 * A key present in `baseline` but not in `current` is NOT reported: the
 * endpoint is a patch and has no way to express a deletion.
 */
export function diffSettings(
  baseline: Partial<SettingsType>,
  current: Partial<SettingsType>,
): Partial<SettingsType> {
  const out: Record<string, unknown> = {};
  const base = baseline as Record<string, unknown>;
  for (const [key, value] of Object.entries(current)) {
    if (!settingsValueEquals(value, base[key])) out[key] = value;
  }
  return out as Partial<SettingsType>;
}

/**
 * Fold a fresh server snapshot into local state without discarding edits the
 * user has made but not yet saved: a key they have not touched (local still
 * equals baseline) takes the server's value, a key they have touched keeps
 * theirs. Without this, a background refresh would silently revert in-progress
 * typing, and — worse — would leave the baseline disagreeing with the values on
 * screen, so the next Save would re-send fields nobody edited.
 */
export function foldServerState(
  prev: Partial<SettingsType>,
  baseline: Partial<SettingsType>,
  server: Partial<SettingsType>,
): Partial<SettingsType> {
  const next = { ...prev } as Record<string, unknown>;
  const prevRec = prev as Record<string, unknown>;
  const baseRec = baseline as Record<string, unknown>;
  for (const [key, value] of Object.entries(server)) {
    if (settingsValueEquals(prevRec[key], baseRec[key])) next[key] = value;
  }
  return next as Partial<SettingsType>;
}

/**
 * Detached copy of a server snapshot, so the baseline can never alias a nested
 * object that some later edit mutates in place (which would hide that edit
 * from the diff).
 */
function snapshot(s: Partial<SettingsType>): Partial<SettingsType> {
  if (typeof structuredClone === "function") return structuredClone(s);
  return JSON.parse(JSON.stringify(s)) as Partial<SettingsType>;
}

// ── Types ─────────────────────────────────────────────────────

// Rows that navigate to another section rather than opening a detail panel
const NAV_ROWS: Partial<Record<SettingsRowId, GuiSection>> = {
  logs:    "logs",
  rooms:   "devices",
  cameras: "devices",
};

// Rows that show a Save button in the detail header
const SAVES_SETTINGS = new Set<SettingsRowId>(["account", "models", "prompts", "voice", "memory", "extensions", "privacy"]);

const DETAIL_TITLE: Record<SettingsRowId, string> = {
  models:        "Models",
  prompts:       "Prompts",
  voice:         "Voice",
  memory:        "Memory & Agent",
  extensions:    "Extensions (MCP)",
  logs:          "Logs",
  privacy:       "Privacy & Data",
  rooms:         "Rooms & Devices",
  cameras:       "Cameras",
  notifications: "Notifications",
  appearance:    "Appearance",
  account:       "Account",
};

const TIMEZONES = [
  "UTC","America/New_York","America/Chicago","America/Denver","America/Los_Angeles",
  "America/Anchorage","Pacific/Honolulu","America/Toronto","America/Vancouver",
  "Europe/London","Europe/Paris","Europe/Berlin","Europe/Rome","Europe/Moscow",
  "Asia/Tokyo","Asia/Shanghai","Asia/Kolkata","Asia/Dubai","Australia/Sydney",
];

// ── Tab content components ────────────────────────────────────

function IdentityTab({
  s, patch, onRestartOnboarding,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  onRestartOnboarding: () => void;
}) {
  const [restarting, setRestarting] = useState(false);

  async function restart() {
    if (restarting) return;
    setRestarting(true);
    try {
      await api.resetOnboarding();
      onRestartOnboarding();
    } catch {
      setRestarting(false);
    }
  }

  return (
    <>
      <Section title="Personal">
        <Row label="Your name" hint="How Goose addresses you">
          <input className="native-input" value={s.user_name ?? ""} onChange={(e) => patch("user_name", e.target.value)} placeholder="Friend" />
        </Row>
        <Row label="Assistant name" hint="What you call your assistant">
          <input className="native-input" value={s.assistant_name ?? ""} onChange={(e) => patch("assistant_name", e.target.value)} placeholder="Goose" />
        </Row>
        <Row label="Timezone">
          <select className="native-select" value={s.timezone ?? "UTC"} onChange={(e) => patch("timezone", e.target.value)}>
            {TIMEZONES.map((tz) => <option key={tz} value={tz}>{tz}</option>)}
          </select>
        </Row>
      </Section>
      <Section title="Personality">
        <Row label="Personality" hint="Describe how your assistant should behave">
          <textarea className="native-textarea native-textarea--sm" value={s.assistant_personality ?? ""} onChange={(e) => patch("assistant_personality", e.target.value)} placeholder="Friendly, concise, and helpful" />
        </Row>
      </Section>
      {/* Weather/Location — restored after #159 dropped the standalone LocationTab.
          Weather is a live feature; without this the location was only editable
          during onboarding. */}
      <Section title="Location & Weather">
        <Row label="Enable weather" hint="Allow the assistant to fetch current weather for your location">
          <Switch aria-label="Enable weather" isSelected={s.weather_enabled ?? false} onChange={(v) => patch("weather_enabled", v)}>
            <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
          </Switch>
        </Row>
        <Row label="Location name" hint="Human-readable name (e.g. Nairobi, Kenya)">
          <input
            className="native-input"
            style={{ opacity: (s.weather_enabled ?? false) ? 1 : 0.45 }}
            disabled={!(s.weather_enabled ?? false)}
            value={s.weather_location_name ?? ""}
            onChange={(e) => patch("weather_location_name", e.target.value)}
            placeholder="Nairobi, Kenya"
          />
        </Row>
        <Row label="Latitude">
          <input
            type="number"
            step={0.0001}
            className="native-input"
            style={{ opacity: (s.weather_enabled ?? false) ? 1 : 0.45 }}
            disabled={!(s.weather_enabled ?? false)}
            value={s.weather_latitude ?? ""}
            onChange={(e) => patch("weather_latitude", e.target.value === "" ? null : Number(e.target.value))}
            placeholder="-1.2921"
          />
        </Row>
        <Row label="Longitude">
          <input
            type="number"
            step={0.0001}
            className="native-input"
            style={{ opacity: (s.weather_enabled ?? false) ? 1 : 0.45 }}
            disabled={!(s.weather_enabled ?? false)}
            value={s.weather_longitude ?? ""}
            onChange={(e) => patch("weather_longitude", e.target.value === "" ? null : Number(e.target.value))}
            placeholder="36.8219"
          />
        </Row>
      </Section>
      <Section title="Onboarding">
        <Row label="Start over" hint="Reset first-time setup and walk through the onboarding wizard again. Your settings are kept.">
          <Button variant="outline" size="sm" isDisabled={restarting} onPress={restart}>
            {restarting ? "Restarting…" : "Restart onboarding"}
          </Button>
        </Row>
      </Section>
    </>
  );
}

function VoiceTab({
  s, patch, hotkey, setHotkey, applyHotkey, refreshSettings, commitField, devMode,
}: {
  s: Partial<SettingsType>;
  patch: (k: keyof SettingsType, v: unknown) => void;
  hotkey: string;
  setHotkey: (v: string) => void;
  applyHotkey: () => void;
  refreshSettings: () => Promise<void>;
  commitField: (k: keyof SettingsType, v: unknown) => Promise<void>;
  devMode: boolean;
}) {
  const [showAdvanced, setShowAdvanced] = useState(false);
  const showDev = devMode || showAdvanced;
  const [calibrating, setCalibrating]   = useState(false);
  const recDur     = s.voice_recording_duration_secs ?? 30;
  const wakePhrase = (s.voice_wake_word ?? "").trim();
  const transcriptions = s.voice_wake_word_transcriptions ?? [];
  const isCalibrated   = transcriptions.length > 0;

  // Calibration records against the STORED wake phrase, so an unsaved edit has
  // to be persisted first. `commitField` sends it only when it actually differs
  // from the server's copy and adopts the echo — calibrating without having
  // touched the phrase used to PUT it anyway, marking `voice_wake_word` as
  // deliberately chosen from a click that changed nothing, and left the panel
  // baseline behind so the next Save re-sent it.
  async function startCalibration() {
    if (wakePhrase) {
      try { await commitField("voice_wake_word", wakePhrase); await api.resetWakeWordCalibration(); } catch { /* ignore */ }
    }
    setCalibrating(true);
  }

  return (
    <>
      <Section title="Activation">
        <Row label="Keyboard shortcut" hint="Press this to activate voice from anywhere">
          <div className="shortcut-row">
            <input className="native-input native-input--flex" value={hotkey} onChange={(e) => setHotkey(e.target.value)} placeholder="CmdOrCtrl+Shift+V" />
            <Button variant="outline" onPress={applyHotkey}>Apply</Button>
          </div>
        </Row>
        <Row label="Wake phrase" hint="Say this phrase to activate voice mode">
          <input className="native-input" value={s.voice_wake_word ?? ""} onChange={(e) => patch("voice_wake_word", e.target.value)} placeholder="goose" disabled={calibrating} />
        </Row>
        {!calibrating && wakePhrase && (
          <div className="calibration-row">
            <span className="calibration-dot" data-calibrated={isCalibrated} />
            <span className="calibration-label">
              {isCalibrated ? `Calibrated (${transcriptions.length} variant${transcriptions.length !== 1 ? "s" : ""})` : "Not calibrated"}
            </span>
            <div className="calibration-row__actions">
              {/* `voice_wake_word_transcriptions` is SERVER-owned: the calibrate
                  endpoints append to it and DELETE clears it. Re-read rather
                  than patching a local copy, so the panel's baseline tracks the
                  server and Save never sends this field back. */}
              {isCalibrated && <Button variant="outline" size="sm" onPress={async () => { try { await api.resetWakeWordCalibration(); await refreshSettings(); } catch { /* ignore */ } }}>Clear</Button>}
              <Button variant="outline" size="sm" onPress={startCalibration}>{isCalibrated ? "Re-calibrate" : "Calibrate"}</Button>
            </div>
          </div>
        )}
        {calibrating && wakePhrase && (
          <Suspense fallback={<p className="muted-12">Loading calibration...</p>}>
            <WakeWordCalibration phrase={wakePhrase} onComplete={async () => { setCalibrating(false); await refreshSettings(); }} onCancel={() => setCalibrating(false)} />
          </Suspense>
        )}
        {!calibrating && (
          <div className="wake-word-note">
            <span className="wake-word-note__icon">i</span>
            <span>{wakePhrase ? "Calibrating improves detection accuracy by learning how Whisper transcribes your voice." : "When set, the app listens passively and activates when the phrase is heard. Leave blank for keyboard shortcut only."}</span>
          </div>
        )}
      </Section>
      <Section title="Recording">
        <Row label={`Max listen time: ${recDur}s`} hint="Auto-stops recording after this duration">
          <input type="range" min={5} max={120} step={5} value={recDur} onChange={(e) => patch("voice_recording_duration_secs", Number(e.target.value))} className="range-full" />
        </Row>
      </Section>
      {!devMode && (
        <button className="advanced-toggle" onClick={() => setShowAdvanced((v) => !v)} aria-expanded={showAdvanced}>
          {showAdvanced ? "▾" : "▸"} Advanced voice settings
        </button>
      )}
      {showDev && (
        <>
          <Section title="Transcription">
            <Row label="Server address"><input className="native-input" value={s.voice_whisper_url ?? ""} onChange={(e) => patch("voice_whisper_url", e.target.value)} placeholder="http://127.0.0.1:9000" /></Row>
            <Row label="Model file"><input className="native-input" value={s.active_whisper_model ?? ""} onChange={(e) => patch("active_whisper_model", e.target.value)} placeholder="ggml-base.bin" /></Row>
          </Section>
          <Section title="Wake-Word Detection">
            <Row label="KWS Whisper URL"><input className="native-input" value={s.voice_kws_whisper_url ?? ""} onChange={(e) => patch("voice_kws_whisper_url", e.target.value || null)} placeholder="Same as transcription server" /></Row>
            <Row label={`Energy threshold: ${(s.voice_kws_energy_threshold ?? 0.003).toFixed(3)}`}>
              <input type="range" min={0} max={0.05} step={0.001} value={s.voice_kws_energy_threshold ?? 0.003} onChange={(e) => patch("voice_kws_energy_threshold", Number(e.target.value))} className="range-full" />
            </Row>
            <Row label={`Silence cutoff: ${s.voice_kws_post_trigger_silence_ms ?? 400}ms`}>
              <input type="range" min={0} max={2000} step={50} value={s.voice_kws_post_trigger_silence_ms ?? 400} onChange={(e) => patch("voice_kws_post_trigger_silence_ms", Number(e.target.value))} className="range-full" />
            </Row>
            <Row label={`Cooldown: ${s.voice_kws_cooldown_ms ?? 2000}ms`}>
              <input type="range" min={500} max={5000} step={100} value={s.voice_kws_cooldown_ms ?? 2000} onChange={(e) => patch("voice_kws_cooldown_ms", Number(e.target.value))} className="range-full" />
            </Row>
          </Section>
          <Section title="Speech Synthesis">
            <Row label="Voice model"><input className="native-input" value={s.active_tts_model ?? ""} onChange={(e) => patch("active_tts_model", e.target.value)} placeholder="en_US-lessac-medium.onnx" /></Row>
            <Row label="Voice name">
              <div className="settings-inline-row">
                <input className="native-input native-input--flex" value={s.voice_tts_voice ?? ""} onChange={(e) => patch("voice_tts_voice", e.target.value)} placeholder="en_US-lessac-medium.onnx" />
                <Button variant="outline" isDisabled>Preview</Button>
              </div>
            </Row>
          </Section>
        </>
      )}
    </>
  );
}

function ModelRoleRow({ provider, model, onPick }: { provider?: string | null; model?: string | null; onPick: () => void }) {
  const label = provider && model ? `${provider} / ${model}` : "Not set";
  return (
    <div className="model-picker">
      <span className="model-picker__current" data-set={!!provider}>{label}</span>
      <Button variant="outline" onPress={onPick}>Change{"…"}</Button>
    </div>
  );
}

function ModelsTab({ s, patch, devMode }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void; devMode: boolean }) {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [toolModels, setToolModels] = useState<string[]>([]);
  const temp = s.llm_temperature ?? 0.7;
  const maxTokenOpts = [128, 256, 512, 1024, 2048, 4096];

  useEffect(() => {
    api.getActiveRoles().then((roles) => {
      if (!s.chat_provider && roles.chat) { patch("chat_provider", roles.chat.provider); patch("chat_model", roles.chat.model); }
    }).catch(() => {});
    api.listModels().then((models) => {
      setToolModels(models.filter((m) => m.provider === "gguf" || m.provider === "local").map((m) => m.name));
    }).catch(() => {});
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <>
      <Section title="AI Models">
        <Row label="Main assistant" hint="The model that powers all conversation and reasoning">
          <ModelRoleRow provider={s.chat_provider} model={s.chat_model} onPick={() => setPickerOpen(true)} />
        </Row>
        {devMode && (
          <Row label="Tool caller" hint="Small specialist model for structured tool-call arguments (optional)">
            <select className="native-select" value={s.tool_model ?? ""} onChange={(e) => patch("tool_model", e.target.value || null)}>
              <option value="">None (use main model)</option>
              {toolModels.map((name) => <option key={name} value={name}>{name}</option>)}
            </select>
          </Row>
        )}
      </Section>
      <Section title="Response style">
        <Row label={`Creativity: ${temp.toFixed(1)}`} hint="Higher = more varied answers, lower = more consistent"><input type="range" min={0} max={2} step={0.1} value={temp} onChange={(e) => patch("llm_temperature", Number(e.target.value))} className="range-full" /></Row>
        <Row label="Response length"><select className="native-select" value={s.llm_max_tokens ?? 1024} onChange={(e) => patch("llm_max_tokens", Number(e.target.value))}>{maxTokenOpts.map((n) => <option key={n} value={n}>{n.toLocaleString()} tokens</option>)}</select></Row>
      </Section>
      {devMode && (
        <>
          <Section title="Response quality">
            <Row label="Thinking mode"><select className="native-select" value={s.thinking_mode ?? "auto"} onChange={(e) => patch("thinking_mode", e.target.value)}><option value="auto">Auto</option><option value="on">Always on</option><option value="off">Off</option></select></Row>
            <Row label="Thinking length" hint="How long the model may think before answering. Brief keeps on-device replies fast."><select className="native-select" value={s.reasoning_effort ?? "brief"} onChange={(e) => patch("reasoning_effort", e.target.value)}><option value="brief">Brief</option><option value="balanced">Balanced</option><option value="thorough">Thorough</option></select></Row>
            <Row label="Show thinking steps"><Switch aria-label="Show thinking steps" isSelected={s.show_thinking ?? false} onChange={(v) => patch("show_thinking", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Keep thinking steps" hint="Save the model's reasoning so it is still there after a reload. Off by default: this is unreviewed working-out, not the answer."><Switch aria-label="Keep thinking steps" isSelected={s.persist_thinking ?? false} onChange={(v) => patch("persist_thinking", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Show turn stats" hint="Display inference timing and context usage below each response"><Switch aria-label="Show turn stats" isSelected={s.show_turn_stats ?? false} onChange={(v) => patch("show_turn_stats", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Answer review"><select className="native-select" value={s.review_mode ?? "off"} onChange={(e) => patch("review_mode", e.target.value)}><option value="off">Off</option><option value="auto">Auto</option><option value="on">Always on</option></select></Row>
            <Row label="Review rounds"><select className="native-select" value={s.review_max_rounds ?? 1} onChange={(e) => patch("review_max_rounds", Number(e.target.value))}><option value={1}>1 round</option><option value={2}>2 rounds</option><option value={3}>3 rounds</option></select></Row>
            <Row label="Review quality bar"><select className="native-select" value={s.review_pass_threshold ?? 3} onChange={(e) => patch("review_pass_threshold", Number(e.target.value))}><option value={2}>2 – Lenient</option><option value={3}>3 – Balanced</option><option value={4}>4 – Strict</option><option value={5}>5 – Very strict</option></select></Row>
          </Section>
          <Section title="Context & embeddings">
            <Row label="Context window override">
              <div className="settings-inline-row">
                <input type="number" role="spinbutton" className="native-input native-input--w120" min={0} max={131072} value={s.context_window_override ?? 0} onChange={(e) => patch("context_window_override", Number(e.target.value))} />
                <span className="muted-12">tokens</span>
              </div>
            </Row>
            <Row label="Embedding provider"><select className="native-select" value={s.embedding_provider ?? "fastembed"} onChange={(e) => patch("embedding_provider", e.target.value)}><option value="fastembed">FastEmbed (local ONNX)</option><option value="none">None</option></select></Row>
            <Row label="Embedding model"><input className="native-input" disabled={(s.embedding_provider ?? "fastembed") === "none"} value={s.active_embedding_model ?? ""} onChange={(e) => patch("active_embedding_model", e.target.value)} placeholder="all-MiniLM-L6-v2" /></Row>
            <Row label="Tool loading" hint="Relevant keeps the prompt small on-device by loading only the tool groups a conversation needs. The assistant can load more itself at any time."><select className="native-select" value={s.tool_selection_mode ?? "all"} onChange={(e) => patch("tool_selection_mode", e.target.value)}><option value="all">All tools, every turn</option><option value="relevant">Only relevant groups</option></select></Row>
          </Section>
        </>
      )}
      {pickerOpen && (
        <ModelPickerModal role="chat" currentProvider={s.chat_provider} currentModel={s.chat_model}
          onSelect={(provider, model) => { patch("chat_provider", provider); patch("chat_model", model); setPickerOpen(false); }}
          onClose={() => setPickerOpen(false)} />
      )}
    </>
  );
}

function PromptsTab({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  const [customEnabled, setCustomEnabled] = useState(!!s.custom_system_prompt);
  const addendum    = s.prompt_addendum ?? "";
  const customPrompt = s.custom_system_prompt ?? "";

  return (
    <>
      <Section title="Prompt Style">
        <RadioGroup aria-label="Prompt style" value={s.prompt_style ?? "balanced"} onChange={(v) => patch("prompt_style", v)}>
          {[["balanced","Balanced","Natural conversation, medium length responses"],["concise","Concise","Short, direct answers. Minimal explanation"],["technical","Technical","Precise, detailed. Favors accuracy over brevity"],["warm","Warm","Friendly, encouraging tone. Conversational style"]].map(([val, label, hint]) => (
            <Radio key={val} value={val}><Radio.Control><Radio.Indicator /></Radio.Control><Radio.Content><div><div className="option-label">{label}</div><div className="row__hint">{hint}</div></div></Radio.Content></Radio>
          ))}
        </RadioGroup>
      </Section>
      <Section title="Prompt Addendum">
        <Row label="Additional context" hint="Appended to every system prompt">
          <div className="pos-relative">
            <textarea className="native-textarea native-textarea--sm" value={addendum} maxLength={500} onChange={(e) => patch("prompt_addendum", e.target.value)} placeholder="Extra instructions..." />
            <span className="char-counter">{addendum.length}/500</span>
          </div>
        </Row>
      </Section>
      <Section title="Custom System Prompt">
        <Row label="Enable custom prompt">
          <Switch aria-label="Enable custom prompt" isSelected={customEnabled} onChange={(v) => { setCustomEnabled(v); if (!v) patch("custom_system_prompt", null); }}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch>
        </Row>
        <Row label="System prompt" hint="Replaces the built-in system prompt entirely">
          <div className="pos-relative">
            <textarea className="native-textarea native-textarea--lg" disabled={!customEnabled} value={customPrompt} maxLength={4000} onChange={(e) => patch("custom_system_prompt", e.target.value)} placeholder="You are a helpful AI assistant..." />
            {customEnabled && <span className="char-counter">{customPrompt.length}/4000</span>}
          </div>
        </Row>
      </Section>
    </>
  );
}

function AgentTab({ s, patch, devMode }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void; devMode: boolean }) {
  const memInject    = s.agent_memory_inject ?? false;
  const [showTuning, setShowTuning] = useState(false);

  return (
    <>
      {devMode && (
        <>
          <Section title="Inference engine">
            <p className="row__hint row__hint--mb">Choose how Pond runs the AI model. Restart server to apply changes.</p>
            <RadioGroup aria-label="Agent backend" value={s.agent_backend ?? "goose"} onChange={(v) => patch("agent_backend", v)}>
              <Radio value="goose"><Radio.Control><Radio.Indicator /></Radio.Control><Radio.Content><div><div className="option-label">Goose Engine (default)</div><div className="row__hint">Full-featured. Supports cloud providers, community MCP extensions, parallel tool execution.</div></div></Radio.Content></Radio>
              <Radio value="pond"><Radio.Control><Radio.Indicator /></Radio.Control><Radio.Content><div><div className="option-label">Pond Engine (optimised local)</div><div className="row__hint">Fastest for local models. KV-cache persistence, minimal overhead. Local GGUF only.</div></div></Radio.Content></Radio>
            </RadioGroup>
          </Section>
        </>
      )}
      <Section title="How Goose behaves">
        <RadioGroup aria-label="Agent mode" value={s.agent_goose_mode ?? "auto"} onChange={(v) => patch("agent_goose_mode", v)}>
          <Radio value="auto"><Radio.Control><Radio.Indicator /></Radio.Control><Radio.Content><div><div className="option-label">Smart (recommended)</div><div className="row__hint">Goose decides when to look things up or take actions</div></div></Radio.Content></Radio>
          <Radio value="chat"><Radio.Control><Radio.Indicator /></Radio.Control><Radio.Content><div><div className="option-label">Chat only</div><div className="row__hint">Conversation only — Goose won't use any tools</div></div></Radio.Content></Radio>
          <Radio value="smart"><Radio.Control><Radio.Indicator /></Radio.Control><Radio.Content><div><div className="option-label">Proactive</div><div className="row__hint">Goose actively uses tools to give more detailed answers</div></div></Radio.Content></Radio>
        </RadioGroup>
      </Section>
      {devMode && (
        <Section title="Performance">
          <Row label="Max turns" hint="Tool-calling steps allowed per request (0 = unlimited; the idle timeout still applies)"><input type="number" role="spinbutton" className="native-input" min={0} max={500} value={s.agent_max_turns ?? 50} onChange={(e) => patch("agent_max_turns", Number(e.target.value))} /></Row>
          <Row label="Idle timeout" hint="Abort a turn only after this many seconds with no output (0 disables)"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w100" min={0} max={3600} value={s.agent_timeout_secs ?? 300} onChange={(e) => patch("agent_timeout_secs", Number(e.target.value))} /><span className="muted-12">seconds</span></div></Row>
          <Row label="Tool output compaction"><Switch aria-label="Tool output compaction" isSelected={s.tool_output_compaction ?? true} onChange={(v) => patch("tool_output_compaction", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
          <Row label="Prompt cache reuse"><Switch aria-label="Prompt cache reuse" isSelected={s.prefix_cache_prompt ?? true} onChange={(v) => patch("prefix_cache_prompt", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
        </Section>
      )}
      <Section title="Memory">
        <Row label="Remember context"><Switch aria-label="Remember context" isSelected={memInject} onChange={(v) => patch("agent_memory_inject", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
        <Row label="How much to recall"><input type="number" role="spinbutton" className="native-input" disabled={!memInject} min={1} max={20} value={s.agent_memory_limit ?? 5} onChange={(e) => patch("agent_memory_limit", Number(e.target.value))} /></Row>
        <Row label="Auto-extract memories"><Switch aria-label="Auto-extract memories" isSelected={s.memory_extraction_enabled ?? true} onChange={(v) => patch("memory_extraction_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
        <Row label="Memory cleanup"><Switch aria-label="Memory cleanup" isSelected={s.memory_cleanup_enabled ?? true} onChange={(v) => patch("memory_cleanup_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
        <Row label="Auto-consolidation"><Switch aria-label="Auto-consolidation" isSelected={s.memory_consolidation_enabled ?? false} onChange={(v) => patch("memory_consolidation_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
        {devMode && (
          <>
            <Row label="Consolidation mode"><select className="native-select" value={s.memory_consolidation_mode ?? "single"} onChange={(e) => patch("memory_consolidation_mode", e.target.value)}><option value="single">Single-pass (fast)</option><option value="adversarial">Adversarial (3-stage)</option></select></Row>
            <Row label="Memory graph"><Switch aria-label="Memory graph" isSelected={s.memory_graph_enabled ?? false} onChange={(v) => patch("memory_graph_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
          </>
        )}
      </Section>
      {devMode && (
        <>
          <button className="advanced-toggle" onClick={() => setShowTuning((v) => !v)} aria-expanded={showTuning}>
            {showTuning ? "▾" : "▸"} Memory tuning
          </button>
        </>
      )}
      {devMode && showTuning && (
        <Section title="Memory Tuning">
          <Row label="Max facts per turn"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={10} value={s.memory_extraction_max_facts ?? 3} onChange={(e) => patch("memory_extraction_max_facts", Number(e.target.value))} /></Row>
          <Row label="Extraction cooldown"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={300} value={s.memory_extraction_interval_secs ?? 10} onChange={(e) => patch("memory_extraction_interval_secs", Number(e.target.value))} /><span className="muted-12">seconds</span></div></Row>
          <Row label="Cleanup interval"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={168} value={s.memory_cleanup_interval_hours ?? 6} onChange={(e) => patch("memory_cleanup_interval_hours", Number(e.target.value))} /><span className="muted-12">hours</span></div></Row>
          <Row label="Consolidation interval"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={168} value={s.memory_consolidation_interval_hours ?? 24} onChange={(e) => patch("memory_consolidation_interval_hours", Number(e.target.value))} /><span className="muted-12">hours</span></div></Row>
          <Row label="Consolidation batch"><input type="number" role="spinbutton" className="native-input native-input--w80" min={5} max={100} value={s.memory_consolidation_batch_size ?? 20} onChange={(e) => patch("memory_consolidation_batch_size", Number(e.target.value))} /></Row>
          <Row label={`Prune threshold: ${(s.memory_prune_threshold ?? 0.05).toFixed(2)}`}><input type="range" min={0} max={0.5} step={0.01} value={s.memory_prune_threshold ?? 0.05} onChange={(e) => patch("memory_prune_threshold", Number(e.target.value))} className="range-full" /></Row>
          <Row label={`Archive threshold: ${(s.memory_archive_threshold ?? 0.15).toFixed(2)}`}><input type="range" min={0} max={1.0} step={0.01} value={s.memory_archive_threshold ?? 0.15} onChange={(e) => patch("memory_archive_threshold", Number(e.target.value))} className="range-full" /></Row>
        </Section>
      )}
    </>
  );
}

function DataTab({ s, patch, serverUrl, onServerUrlChange, devMode }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void; serverUrl: string; onServerUrlChange: (v: string) => void; devMode: boolean }) {
  return (
    <>
      <Section title="Data retention">
        <Row label="Keep event logs for"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={365} value={s.retention_event_log_days ?? 30} onChange={(e) => patch("retention_event_log_days", Number(e.target.value))} /><span className="muted-12">days</span></div></Row>
        <Row label="Keep sensor readings for"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={365} value={s.retention_sensor_days ?? 7} onChange={(e) => patch("retention_sensor_days", Number(e.target.value))} /><span className="muted-12">days</span></div></Row>
        <Row label="Keep chat history for"><div className="settings-inline-row"><input type="number" role="spinbutton" className="native-input native-input--w100" min={10} max={10000} value={s.retention_session_messages_keep ?? 500} onChange={(e) => patch("retention_session_messages_keep", Number(e.target.value))} /><span className="muted-12">messages</span></div></Row>
      </Section>
      {devMode && (
        <>
          <Section title="Telemetry">
            <Row label="Usage telemetry"><Switch aria-label="Usage telemetry" isSelected={s.telemetry_enabled ?? true} onChange={(v) => patch("telemetry_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Context monitoring"><Switch aria-label="Context monitoring" isSelected={s.context_monitor_enabled ?? true} onChange={(v) => patch("context_monitor_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
          </Section>
          <Section title="Cloud cost comparison">
            <Row label="Input price"><div className="settings-inline-row"><span className="muted-12">$</span><input type="number" role="spinbutton" step={0.1} className="native-input native-input--w100" min={0} value={s.cloud_input_price_per_million ?? 2.5} onChange={(e) => patch("cloud_input_price_per_million", Number(e.target.value))} /><span className="muted-12">/ 1M tokens</span></div></Row>
            <Row label="Output price"><div className="settings-inline-row"><span className="muted-12">$</span><input type="number" role="spinbutton" step={0.1} className="native-input native-input--w100" min={0} value={s.cloud_output_price_per_million ?? 10.0} onChange={(e) => patch("cloud_output_price_per_million", Number(e.target.value))} /><span className="muted-12">/ 1M tokens</span></div></Row>
          </Section>
          <Section title="Server">
            <Row label="Server URL" hint="pond-server base URL">
              <input className="native-input" value={serverUrl} onChange={(e) => onServerUrlChange(e.target.value)} placeholder="http://127.0.0.1:4000" />
            </Row>
          </Section>
        </>
      )}
    </>
  );
}

/**
 * PAI-7 P4 and P6 — the five settings that decide whether the assistant
 * addresses somebody who did not address it.
 *
 * Until this section existed all five were reachable only by `curl`, and one of
 * them decides whether the pond talks to you unasked. A household cannot
 * consent to a feature it cannot see.
 *
 * **Quiet hours come first because the server checks them first.**
 * `decide_unprompted_speech` refuses inside the window before it looks at
 * consent, presence or category, so no combination of the rows below produces
 * speech in it. Putting the switch above the window would teach the opposite of
 * what the code does — that consent is the outer decision and quiet hours a
 * detail inside it.
 *
 * Both switches are read `=== true` for the reason given on the delegation row
 * above: a key the server has not sent, or a settings read that failed, has to
 * mean off.
 *
 * **Not dev-mode gated, deliberately.** Hiding the control for "may it speak to
 * you unasked" behind the developer pill is the same as leaving it headless.
 *
 * One shape here differs from the rows above and it is not a style choice. The
 * switches wrap their control in `Switch.Content`, which is HeroUI's
 * `SwitchButton` and the only part of the compound that renders the
 * `<input role="switch">`. A `<Switch>` whose children are only
 * `Switch.Control` / `Switch.Thumb` renders two spans: no input, no accessible
 * name, no click target, no `onChange`. The tripwire in `Settings.test.tsx`
 * pins which rows on this panel are operable at all, and is written to fail the
 * day the older ones are repaired.
 */
function UnpromptedSection({ s, patch }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void }) {
  return (
    <Section title="Speaking and acting unprompted">
      <p className="row__hint row__hint--mb">
        Both switches here ship off. Quiet hours are checked first and on their own:
        inside that window the assistant stays silent whatever the switches say.
      </p>
      {/* Plain text inputs rather than <input type="time">. Both bounds are free
          text in a key-value settings table, and a value the server cannot parse
          means SILENCE, not "no quiet hours". A time picker renders such a value
          as blank, which reads as "not set" — the opposite of what it does. */}
      <Row label="Quiet hours start" hint="Local 24-hour HH:MM. Between this and the end time the assistant never speaks unprompted, whatever else is set here.">
        <input
          className="native-input"
          aria-label="Quiet hours start"
          value={s.quiet_hours_start ?? "22:00"}
          onChange={(e) => patch("quiet_hours_start", e.target.value)}
          placeholder="22:00"
        />
      </Row>
      <Row label="Quiet hours end" hint="The window runs past midnight when this is earlier than the start. Both the same means silent all day, and a time the server cannot read also means silence.">
        <input
          className="native-input"
          aria-label="Quiet hours end"
          value={s.quiet_hours_end ?? "07:00"}
          onChange={(e) => patch("quiet_hours_end", e.target.value)}
          placeholder="07:00"
        />
      </Row>
      <Row label="Speak without being spoken to" hint="Ships off. Lets the assistant read a notification aloud on its own — outside quiet hours, never while it is already answering, and only to a household member it has heard from recently.">
        <Switch aria-label="Speak without being spoken to" isSelected={s.unprompted_speech_enabled === true} onChange={(v) => patch("unprompted_speech_enabled", v)}>
          <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
        </Switch>
      </Row>
      <Row label="Categories it may speak" hint="Comma-separated. Defaults to alert alone, which leaves out the info notification every scheduled task produces. An entry it does not recognise matches nothing.">
        <input
          className="native-input"
          aria-label="Categories it may speak"
          value={s.unprompted_speech_categories ?? "alert"}
          onChange={(e) => patch("unprompted_speech_categories", e.target.value)}
          placeholder="alert"
        />
      </Row>
      <Row label="Review the house unasked" hint="Ships off. Lets the assistant think about what has happened while nobody is using the pond and propose things for you to approve; it never acts on its own. Needs delegation above, waits for 15 minutes of quiet, and stops as soon as somebody uses the pond.">
        <Switch aria-label="Review the house unasked" isSelected={s.proactive_review_enabled === true} onChange={(v) => patch("proactive_review_enabled", v)}>
          <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
        </Switch>
      </Row>
    </Section>
  );
}

function ToolsTab({ s, patch, devMode }: { s: Partial<SettingsType>; patch: (k: keyof SettingsType, v: unknown) => void; devMode: boolean }) {
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading]       = useState(true);
  const [error, setError]           = useState<string | null>(null);
  const [showForm, setShowForm]     = useState(false);
  const [addName, setAddName]       = useState("");
  const [addKind, setAddKind]       = useState<"stdio" | "sse">("stdio");
  const [addCmd, setAddCmd]         = useState("");
  const [adding, setAdding]         = useState(false);
  // API keys live in the secret store, which returns key NAMES only -- there is
  // no way to read a value back, by design. So the row shows whether a key is
  // configured, accepts a replacement, and can clear it. It never renders one.
  const [secretKeys, setSecretKeys]   = useState<string[]>([]);
  const [secretDraft, setSecretDraft] = useState<Record<string, string>>({});
  const [secretBusy, setSecretBusy]   = useState<string | null>(null);

  function loadSecrets() {
    api.listSecretKeys().then(setSecretKeys).catch(() => setSecretKeys([]));
  }

  async function saveSecret(key: string) {
    const value = (secretDraft[key] ?? "").trim();
    if (!value) return;
    setSecretBusy(key);
    try {
      await api.setSecret(key, value);
      setSecretDraft((d) => ({ ...d, [key]: "" }));
      loadSecrets();
    } catch (e) { setError(String(e)); } finally { setSecretBusy(null); }
  }

  async function clearSecret(key: string) {
    setSecretBusy(key);
    try { await api.deleteSecret(key); loadSecrets(); }
    catch (e) { setError(String(e)); } finally { setSecretBusy(null); }
  }

  function load() {
    setLoading(true);
    api.listExtensions().then((r) => setExtensions(r.extensions)).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }
  useEffect(() => { load(); loadSecrets(); }, []);

  async function toggle(name: string, enabled: boolean) {
    try { await api.toggleExtension(name, enabled); setExtensions((prev) => prev.map((e) => e.name === name ? { ...e, enabled } : e)); } catch (e) { setError(String(e)); }
  }
  async function remove(name: string) {
    try { await api.removeExtension(name); load(); } catch (e) { setError(String(e)); }
  }
  async function add() {
    if (!addName.trim() || !addCmd.trim()) return;
    setAdding(true);
    try { await api.addExtension({ name: addName.trim(), kind: addKind, command: addCmd.trim() }); setAddName(""); setAddCmd(""); setShowForm(false); load(); } catch (e) { setError(String(e)); } finally { setAdding(false); }
  }

  const EXT_TOOLS = [
    ["Memory","Recall, save, and forget memories","ext_memory_enabled"],
    ["Schedules","Create, manage, and run scheduled tasks","ext_schedule_enabled"],
    ["Weather","Fetch current weather data","ext_weather_enabled"],
    ["Knowledge","Wikipedia search and article retrieval","ext_knowledge_enabled"],
    ["System","Shell commands, file access, notifications, system info","ext_system_enabled"],
    ["Devices","Device registry, profile info, model assignments","ext_device_enabled"],
    ["News","Top stories and headline search","ext_news_enabled"],
    ["Finance","Stock quotes, crypto prices, and currency exchange rates","ext_finance_enabled"],
    ["Discovery","Country info, product lookup, web search","ext_discovery_enabled"],
    ["Audit / Privacy tools","Recent activity, activity summary, and privacy-risk report","ext_audit_enabled"],
    ["Vision","Camera event detection and vision queries (read-only event store)","ext_vision_enabled"],
    ["Sensors","Read stored IoT sensor data: latest reading, history, and list sensors","ext_sensor_enabled"],
  ] as const;

  // The second element is the SECRET-STORE key name, not a Settings field: the
  // SCREAMING_SNAKE env-var spelling the repository already uses
  // (BRAVE_API_KEY, SPOTIFY_ACCESS_TOKEN) and which secret_migration writes.
  const API_KEYS = [
    ["Guardian (news)","GUARDIAN_API_KEY","Enables the Guardian news source for the News tools"],
    ["GNews","GNEWS_API_KEY","Enables GNews headline search for the News tools"],
    ["Finnhub (stocks)","FINNHUB_API_KEY","Enables stock quotes in the Finance tools"],
    ["CoinGecko (crypto)","COINGECKO_API_KEY","Enables crypto prices in the Finance tools"],
  ] as const;

  return (
    <>
      <Section title="Built-in Tool Modules">
        {EXT_TOOLS.map(([label, hint, key]) => (
          <Row key={key} label={label} hint={hint}>
            <Switch aria-label={label} isSelected={(s as Record<string, unknown>)[key] !== false} onChange={(v) => patch(key as keyof SettingsType, v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch>
          </Row>
        ))}
        {/*
          Deliberately NOT in EXT_TOOLS above. That map renders `!== false`, so a
          key the server has not sent yet reads as ON while settings are still
          loading — harmless for a module that defaults on, wrong in the widening
          direction for the only one that defaults off. This row is `=== true`:
          absent means off, unreadable means off.
        */}
        <Row label="Delegation to saved roles" hint="Let the assistant hand work to a saved agent role that runs on its own. Off by default: a delegated agent acts without asking.">
          <Switch aria-label="Delegation to saved roles" isSelected={s.ext_orchestrator_enabled === true} onChange={(v) => patch("ext_orchestrator_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch>
        </Row>
      </Section>
      <UnpromptedSection s={s} patch={patch} />
      <Section title="Vision / Cameras">
        <Row label="Enable vision" hint="On-device camera event detection (motion, objects) via the vision MCP server">
          <Switch aria-label="Enable vision" isSelected={s.vision_enabled ?? false} onChange={(v) => patch("vision_enabled", v)}>
            <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
          </Switch>
        </Row>
        <Row label="Camera URL" hint="RTSP/HTTP stream or device path">
          <input className="native-input" style={{ opacity: (s.vision_enabled ?? false) ? 1 : 0.45 }} disabled={!(s.vision_enabled ?? false)} value={s.vision_camera_url ?? ""} onChange={(e) => patch("vision_camera_url", e.target.value)} placeholder="rtsp://… or /dev/video0" />
        </Row>
        <Row label="Camera ID" hint="Label for this camera in events">
          <input className="native-input" style={{ opacity: (s.vision_enabled ?? false) ? 1 : 0.45 }} disabled={!(s.vision_enabled ?? false)} value={s.vision_camera_id ?? ""} onChange={(e) => patch("vision_camera_id", e.target.value)} placeholder="front-door" />
        </Row>
        <Row label="Frames per second" hint="Detection sampling rate">
          <input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={30} style={{ opacity: (s.vision_enabled ?? false) ? 1 : 0.45 }} disabled={!(s.vision_enabled ?? false)} value={s.vision_fps ?? 2} onChange={(e) => patch("vision_fps", Number(e.target.value))} />
        </Row>
        <Row label="Motion threshold" hint="0.0–1.0; higher = less sensitive">
          <input type="number" step={0.01} min={0} max={1} className="native-input native-input--w80" style={{ opacity: (s.vision_enabled ?? false) ? 1 : 0.45 }} disabled={!(s.vision_enabled ?? false)} value={s.vision_motion_threshold ?? 0.1} onChange={(e) => patch("vision_motion_threshold", Number(e.target.value))} />
        </Row>
      </Section>
      <Section title="API Keys / Integrations">
        {API_KEYS.map(([label, key, hint]) => {
          const configured = secretKeys.includes(key);
          const draft = secretDraft[key] ?? "";
          return (
            <Row key={key} label={label} hint={hint}>
              <div className="settings-inline-row">
                <span className="muted-12">{configured ? "Configured" : "Not set"}</span>
                <input
                  type="password"
                  autoComplete="off"
                  className="native-input"
                  aria-label={`${label} API key`}
                  value={draft}
                  onChange={(e) => setSecretDraft((d) => ({ ...d, [key]: e.target.value }))}
                  placeholder={configured ? "Paste a new key to replace" : "Paste key"}
                />
                <Button variant="outline" isDisabled={secretBusy === key || !draft.trim()} onPress={() => saveSecret(key)}>Save</Button>
                {configured && (
                  <Button variant="outline" aria-label={`Clear ${label} API key`} isDisabled={secretBusy === key} onPress={() => clearSecret(key)}><Trash2 size={14} /></Button>
                )}
              </div>
            </Row>
          );
        })}
        <Row label="SearXNG URL" hint="Self-hosted SearXNG instance for private web search (Discovery tools)">
          <input className="native-input" value={s.searxng_url ?? ""} onChange={(e) => patch("searxng_url", e.target.value === "" ? null : e.target.value)} placeholder="http://localhost:8888" />
        </Row>
      </Section>
      {devMode && (
        <>
          <Section title="Tool behaviour">
            <Row label="Tool call validation"><Switch aria-label="Tool call validation" isSelected={s.tool_call_validation ?? true} onChange={(v) => patch("tool_call_validation", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Tool request detection"><Switch aria-label="Tool request detection" isSelected={s.tool_request_detection ?? true} onChange={(v) => patch("tool_request_detection", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Multi-tool"><Switch aria-label="Multi-tool" isSelected={s.multi_tool_enabled ?? false} onChange={(v) => patch("multi_tool_enabled", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
          </Section>
          <Section title="Scheduling">
            <Row label="Result notifications"><Switch aria-label="Result notifications" isSelected={s.schedule_result_notify ?? true} onChange={(v) => patch("schedule_result_notify", v)}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch></Row>
            <Row label="Max concurrent"><input type="number" role="spinbutton" className="native-input native-input--w80" min={1} max={10} value={s.schedule_max_concurrent ?? 2} onChange={(e) => patch("schedule_max_concurrent", Number(e.target.value))} /></Row>
            <Row label="History per task"><input type="number" role="spinbutton" className="native-input native-input--w80" min={5} max={500} value={s.schedule_max_runs_per_task ?? 50} onChange={(e) => patch("schedule_max_runs_per_task", Number(e.target.value))} /></Row>
          </Section>
        </>
      )}
      <Section title="External Extensions">
        <div className="add-ext-row">
          <Button variant="outline" onPress={() => setShowForm((v) => !v)}><Plus size={14} /> Add Extension</Button>
        </div>
      </Section>
      {showForm && (
        <Card className="card"><CardContent className="card-body--col">
          <input className="native-input" value={addName} onChange={(e) => setAddName(e.target.value)} placeholder="Name (e.g. developer)" aria-label="Extension name" />
          <select className="native-select" value={addKind} onChange={(e) => setAddKind(e.target.value as "stdio" | "sse")}><option value="stdio">stdio</option><option value="sse">SSE</option></select>
          <input className="native-input" value={addCmd} onChange={(e) => setAddCmd(e.target.value)} placeholder="Command or URI" aria-label="Extension command or URI" />
          <Button variant="primary" onPress={add} isDisabled={adding || !addName.trim() || !addCmd.trim()}>{adding ? "Adding..." : "Add"}</Button>
        </CardContent></Card>
      )}
      {error && <ErrorBanner error={error} onRetry={() => { setError(null); load(); }} />}
      {loading ? <SkeletonList rows={3} /> : extensions.length === 0 ? (
        <div className="empty-state empty-state--inline"><span>No extensions yet.</span><button className="empty-state__cta" onClick={() => setShowForm(true)}>Add your first extension</button></div>
      ) : (
        <div className="ext-list">
          {extensions.map((ext) => (
            <div key={ext.name} className="ext-row">
              <Switch isSelected={ext.enabled} onChange={() => toggle(ext.name, !ext.enabled)} aria-label={`Toggle ${ext.name}`}><Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content></Switch>
              <div className="ext-row__body">
                <div className="ext-row__title-row">
                  <span className="ext-row__name">{ext.name}</span>
                  <Chip size="sm" variant="soft">{ext.kind}</Chip>
                  {!ext.enabled && <Chip size="sm" variant="soft" color="warning">Disabled</Chip>}
                </div>
                {ext.description && <p className="ext-row__desc">{ext.description}</p>}
                {ext.tools.length > 0 && (
                  <div className="ext-row__tools">
                    {ext.tools.slice(0, 8).map((t) => <code key={t} className="ext-tool-badge">{t.replace(`${ext.name}__`, "")}</code>)}
                    {ext.tools.length > 8 && <span className="muted-12">+{ext.tools.length - 8} more</span>}
                  </div>
                )}
              </div>
              <Button variant="danger-soft" onPress={() => remove(ext.name)} aria-label={`Remove ${ext.name}`}><Trash2 size={14} /></Button>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

// ── Settings row (navigation list item) ──────────────────────

interface SettingsRowProps {
  id: SettingsRowId;
  iconPath: string;
  color: string;
  bg: string;
  label: string;
  sub: string;
  value?: string;
  badge?: string;
  onClick: () => void;
}

function SettingsRow({ iconPath, color, bg, label, sub, value, badge, onClick }: SettingsRowProps) {
  return (
    <div className="set-row" role="button" tabIndex={0} onClick={onClick}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onClick(); } }}
      aria-label={label}
    >
      <span className="set-row__icon" style={{ "--icon-bg": bg } as React.CSSProperties}>
        <HubIco d={iconPath} size={18} color={color} />
      </span>
      <span className="set-row__text">
        <span className="set-row__label">{label}</span>
        <span className="set-row__sub">{sub}</span>
      </span>
      {badge && <span className="set-row__badge">{badge}</span>}
      {value && <span className="set-row__value">{value}</span>}
      <HubIco d={HP_PATHS.chevR} size={17} color="var(--color-text-tertiary,#566178)" />
    </div>
  );
}

// ── Main Settings component ───────────────────────────────────

export function Settings() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  const [detail, setDetail]     = useState<SettingsRowId | null>(null);
  const [settings, setSettings] = useState<Partial<SettingsType>>({});
  // Last state the server told us about. Save PATCHes the difference against
  // this, never the whole object — see the diffing block at the top of the file.
  const [baseline, setBaseline] = useState<Partial<SettingsType>>({});
  const [loading, setLoading]   = useState(true);
  const [saving, setSaving]     = useState(false);
  const [saved, setSaved]       = useState(false);
  const [error, setError]       = useState<string | null>(null);
  const [hotkey, setHotkey]     = useState("CmdOrCtrl+Shift+V");
  const [devMode, setDevMode]   = useState<boolean>(() => {
    try { return localStorage.getItem("settings-dev-mode") === "true"; } catch { return false; }
  });

  function toggleDevMode() {
    setDevMode((v) => {
      try { localStorage.setItem("settings-dev-mode", String(!v)); } catch { /* ignore */ }
      return !v;
    });
  }

  function adoptServerState(s: SettingsType) {
    setSettings(s);
    setBaseline(snapshot(s));
  }

  useEffect(() => {
    api.getSettings().then(adoptServerState).catch((e) => setError(String(e))).finally(() => setLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function flashSaved() {
    setSaved(true);
    setTimeout(() => setSaved(false), 2000);
  }

  async function save() {
    const patchBody = diffSettings(baseline, settings);
    if (Object.keys(patchBody).length === 0) {
      // Nothing to send. Do NOT fall back to PUTting the whole object: that is
      // what marked every key as deliberately chosen and reverted fields
      // changed elsewhere.
      setError(null);
      flashSaved();
      return;
    }
    setSaving(true); setSaved(false); setError(null);
    try {
      const updated = await api.updateSettings(patchBody);
      // Fold against the PATCH WE SENT, not the pre-save baseline. The request
      // succeeded, so those keys are what the server now holds; anything still
      // differing from them was typed while the request was in flight and must
      // be kept.
      //
      // This is also the only thing that lets a float field ever converge. The
      // API serialises `f32` through serde_json, which widens to `f64`, so the
      // echo of `0.8` comes back as 0.800000011920929 — see
      // docs/developer/settings-defaults-and-user-intent.md. Folded against the
      // OLD baseline the key looked locally edited, kept the client's 0.8, and
      // then disagreed with the new baseline forever: every later Save re-sent
      // it and re-marked it user-set, quietly ending default adoption for it.
      setSettings((prev) => foldServerState(prev, { ...baseline, ...patchBody }, updated));
      setBaseline(snapshot(updated));
      flashSaved();
    }
    catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  }

  async function applyHotkey() {
    if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
    try { await invoke("set_hotkey", { hotkey }); } catch (e) { setError(String(e)); }
  }

  /**
   * Re-read the server after something OTHER than this panel wrote settings
   * (wake-word calibration appends `voice_wake_word_transcriptions` server
   * side). The baseline must move with it, or the refreshed values would look
   * like local edits and be re-sent on the next Save.
   */
  async function refreshSettings() {
    try {
      const u = await api.getSettings();
      setSettings((prev) => foldServerState(prev, baseline, u));
      setBaseline(snapshot(u));
    } catch { /* ignore */ }
  }

  /**
   * PUT a single field from outside the Save button, keeping the panel in step.
   *
   * A value that already equals the baseline is NOT sent. `PUT
   * /api/v1/settings` marks every key the request carries as deliberately
   * chosen, so an unconditional write would end default adoption for that key
   * on a click that changed nothing. And because it does write, the response
   * has to be adopted the same way `save()` adopts one, or the panel's baseline
   * stays behind and the next Save re-sends the field.
   */
  async function commitField(key: keyof SettingsType, value: unknown) {
    if (settingsValueEquals(value, (baseline as Record<string, unknown>)[key])) return;
    const patchBody = { [key]: value } as Partial<SettingsType>;
    const updated = await api.updateSettings(patchBody);
    setSettings((prev) => foldServerState(prev, { ...baseline, ...patchBody }, updated));
    setBaseline(snapshot(updated));
  }

  function patch(key: keyof SettingsType, value: unknown) {
    setSettings((prev) => ({ ...prev, [key]: value }));
  }

  function go(id: string) {
    const nav = NAV_ROWS[id as SettingsRowId];
    if (nav) { dispatch({ type: "SET_SECTION", payload: nav }); return; }
    setDetail(id as SettingsRowId);
  }

  function renderDetail(id: SettingsRowId) {
    switch (id) {
      case "account":       return <IdentityTab s={settings} patch={patch} onRestartOnboarding={() => dispatch({ type: "SET_NEEDS_ONBOARDING", payload: true })} />;
      case "models":        return <ModelsTab   s={settings} patch={patch} devMode={devMode} />;
      case "prompts":       return <PromptsTab  s={settings} patch={patch} />;
      case "voice":         return <VoiceTab    s={settings} patch={patch} hotkey={hotkey} setHotkey={setHotkey} applyHotkey={applyHotkey} refreshSettings={refreshSettings} commitField={commitField} devMode={devMode} />;
      case "memory":        return <AgentTab    s={settings} patch={patch} devMode={devMode} />;
      case "extensions":    return <ToolsTab    s={settings} patch={patch} devMode={devMode} />;
      case "privacy":       return <DataTab     s={settings} patch={patch} serverUrl={state.serverUrl} onServerUrlChange={(v) => dispatch({ type: "SET_SERVER_URL", payload: v })} devMode={devMode} />;
      case "appearance":    return <AppearanceView />;
      case "notifications": return <p className="view-sub">Notification settings coming soon.</p>;
      default:              return null;
    }
  }

  // ── Detail view ──
  if (detail) {
    return (
      <div className="set">
        <header className="view-head">
          {detail !== "appearance" && (
            <div>
              <h1 className="view-title">{DETAIL_TITLE[detail]}</h1>
            </div>
          )}
          <div className="set__head-actions">
            <button className="hub-back-btn" onClick={() => setDetail(null)} type="button">
              ← Back
            </button>
            {SAVES_SETTINGS.has(detail) && (
              <Button variant="primary" onPress={save} isDisabled={saving || loading}>
                {saving ? "Saving…" : saved ? "Saved" : "Save"}
              </Button>
            )}
          </div>
        </header>
        {error && <ErrorBanner error={error} onRetry={() => { setError(null); setLoading(true); api.getSettings().then(adoptServerState).catch((e) => setError(String(e))).finally(() => setLoading(false)); }} />}
        <div className="settings-body">
          {loading ? <SkeletonList rows={5} /> : renderDetail(detail)}
        </div>
      </div>
    );
  }

  // ── List view ──
  return (
    <div className="set">
      <header className="view-head">
        <div>
          <h1 className="view-title">Settings</h1>
          <p className="view-sub">Your home, your assistant, and the models that power it.</p>
        </div>
        <div className="set__head-actions">
          <button
            className={`set__dev-pill${devMode ? " is-on" : ""}`}
            onClick={toggleDevMode}
            type="button"
          >
            <span className="set__dev-pill__dot" />
            {devMode ? "Dev mode on" : "Developer mode"}
          </button>
          <Button
            size="sm"
            variant="outline"
            className="hub-preview-btn"
            onPress={() => dispatch({ type: "SET_SECTION", payload: "hub" })}
          >
            <LayoutGrid size={14} />
            <span className="hub-preview-btn__label">Preview Goose Hub redesign</span>
          </Button>
        </div>
      </header>
      <div className="set__groups">
        {/* ── You ── */}
        <div className="set__group">
          <div className="set__glabel">You</div>
          <div className="set__list">
            {SETTINGS.flatMap((g) => g.rows).filter((r) => ["account", "appearance"].includes(r.id)).map((r) => {
              const sub = r.id === "account" && settings.user_name ? settings.user_name : r.sub;
              return <SettingsRow key={r.id} id={r.id} iconPath={r.iconPath} color={r.color} bg={r.bg} label={r.label} sub={sub} value={r.value} badge={r.badge} onClick={() => go(r.id)} />;
            })}
          </div>
        </div>

        {/* ── Assistant ── */}
        <div className="set__group">
          <div className="set__glabel">Assistant</div>
          <div className="set__list">
            {SETTINGS.flatMap((g) => g.rows).filter((r) => ["models", "voice", "prompts", "extensions"].includes(r.id)).map((r) => {
              return <SettingsRow key={r.id} id={r.id} iconPath={r.iconPath} color={r.color} bg={r.bg} label={r.label} sub={r.sub} value={r.value} badge={r.badge} onClick={() => go(r.id)} />;
            })}
          </div>
        </div>

        {/* ── Advanced (dev mode only) ── */}
        {devMode && (
          <div className="set__group">
            <div className="set__glabel">Advanced</div>
            <div className="set__list">
              {SETTINGS.flatMap((g) => g.rows).filter((r) => ["memory", "privacy", "logs"].includes(r.id)).map((r) => {
                return <SettingsRow key={r.id} id={r.id} iconPath={r.iconPath} color={r.color} bg={r.bg} label={r.label} sub={r.sub} value={r.value} badge={r.badge} onClick={() => go(r.id)} />;
              })}
            </div>
          </div>
        )}

      </div>
    </div>
  );
}
