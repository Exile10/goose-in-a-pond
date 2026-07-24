import { useState, useEffect, useCallback, useRef } from "react";
import { Play, RefreshCw, Loader2, RotateCcw } from "lucide-react";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle, Slider } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Settings } from "../../../api/types";

// ─── Icon path strings ───────────────────────────────────────
const SICN = {
  mic:     "M12 2a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V5a3 3 0 0 1 3-3zM19 10a7 7 0 0 1-14 0M12 19v3M8 22h8",
  speaker: "M11 5L6 9H2v6h4l5 4zM19 5a10 10 0 0 1 0 14M15.5 8.5a5 5 0 0 1 0 7",
  text:    "M4 6h16M4 10h16M4 14h10",
} as const;

// ─── STT model options ────────────────────────────────────────
interface SttOption {
  label: string;
  value: string;
}

const STT_OPTIONS: SttOption[] = [
  { label: "Whisper base",          value: "ggml-base.bin"         },
  { label: "Whisper small",         value: "ggml-small.bin"        },
  { label: "Whisper large-v3-turbo", value: "ggml-large-v3-turbo.bin" },
];

// ─── TTS voice options ────────────────────────────────────────
interface TtsVoiceOption {
  label: string;
  value: string;
}

const TTS_VOICE_OPTIONS: TtsVoiceOption[] = [
  { label: "Lessac (default)", value: "en_US-lessac-medium.onnx" },
  { label: "Ryan",             value: "en_US-ryan-medium.onnx"   },
  { label: "Jenny",            value: "en_US-jenny-dioco-medium.onnx" },
];

// ─── Mock fallback ────────────────────────────────────────────
const MOCK_VOICE_SETTINGS: Partial<Settings> = {
  voice_wake_word: "goose",
  active_whisper_model: "ggml-base.bin",
  voice_tts_voice: "en_US-lessac-medium.onnx",
};

// Speaking rate is a Hub-layer UI concern (not yet in Settings type).
// The field `tts_speed` will be wired when the backend exposes it.
// TODO Phase 8 wave 4: add tts_speed to Settings type + api.updateSettings({ tts_speed })
type TtsSpeed = number;
const DEFAULT_TTS_SPEED: TtsSpeed = 100;

interface VoiceDetailProps {
  go: (route: string) => void;
}

export function VoiceDetail({ go }: VoiceDetailProps) {
  // ── State ──────────────────────────────────────────────────
  const [settings, setSettings]       = useState<Partial<Settings>>(MOCK_VOICE_SETTINGS);
  const [loading, setLoading]         = useState(true);
  const [error, setError]             = useState<string | null>(null);
  const [flash, setFlash]             = useState<{ text: string; ok: boolean } | null>(null);

  // Local-only speaking rate (not yet in Settings type)
  const [ttsSpeed, setTtsSpeed]       = useState<TtsSpeed>(DEFAULT_TTS_SPEED);

  // Reset calibration state
  const [resetting, setResetting]     = useState(false);

  // Debounce timer for slider
  const sliderTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const flashTimer  = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ── Flash helper ───────────────────────────────────────────
  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }

  // ── Load settings on mount ─────────────────────────────────
  const loadSettings = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const s = await api.getSettings();
      setSettings(s);
      // TODO Phase 8 wave 4: read s.tts_speed here once backend exposes it
    } catch (e) {
      console.warn("[VoiceDetail] API offline — using mock fallback:", e);
      setError("Could not reach the server. Showing offline view.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadSettings();
    return () => {
      if (sliderTimer.current) clearTimeout(sliderTimer.current);
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadSettings]);

  // ── Hands-free toggle ──────────────────────────────────────
  // voice_hands_free is not yet in Settings type — wired as a local state with
  // TODO comment; backend field will be `voice_hands_free` when added.
  // TODO Phase 8 wave 4: add voice_hands_free to Settings type + wire updateSettings
  const [handsFreePending, setHandsFreePending] = useState(false);
  const handsFree = false; // placeholder — replace with settings.voice_hands_free ?? false

  async function handleHandsFreeChange(on: boolean) {
    if (handsFreePending) return;
    setHandsFreePending(true);
    try {
      // TODO Phase 8 wave 4: await api.updateSettings({ voice_hands_free: on });
      // For now we log and show a flash so the UX is not completely silent.
      console.info("[VoiceDetail] hands-free toggled:", on, "(not persisted — field pending in backend)");
      showFlash(on ? "Hands-free enabled (coming in wave 4)" : "Hands-free disabled (coming in wave 4)", true);
    } finally {
      setHandsFreePending(false);
    }
  }

  // ── STT model select ───────────────────────────────────────
  async function handleSttChange(value: string) {
    setSettings((prev) => ({ ...prev, active_whisper_model: value }));
    try {
      await api.updateSettings({ active_whisper_model: value });
      showFlash("Speech recognition model updated.");
    } catch (e) {
      // Revert
      setSettings((prev) => ({ ...prev, active_whisper_model: settings.active_whisper_model }));
      showFlash(`Failed to update STT model: ${String(e)}`, false);
    }
  }

  // ── TTS voice select ───────────────────────────────────────
  async function handleTtsVoiceChange(value: string) {
    setSettings((prev) => ({ ...prev, voice_tts_voice: value }));
    try {
      await api.updateSettings({ voice_tts_voice: value });
      showFlash("Voice updated.");
    } catch (e) {
      setSettings((prev) => ({ ...prev, voice_tts_voice: settings.voice_tts_voice }));
      showFlash(`Failed to update voice: ${String(e)}`, false);
    }
  }

  // ── Speaking rate slider (debounced) ───────────────────────
  function handleSpeedChange(v: number) {
    setTtsSpeed(v);
    if (sliderTimer.current) clearTimeout(sliderTimer.current);
    sliderTimer.current = setTimeout(async () => {
      try {
        // TODO Phase 8 wave 4: await api.updateSettings({ tts_speed: v });
        console.info("[VoiceDetail] tts_speed:", v, "(not persisted — field pending in backend)");
      } catch (e) {
        showFlash(`Failed to update speaking rate: ${String(e)}`, false);
      }
    }, 400);
  }

  // ── Reset wake-word calibration ────────────────────────────
  async function handleResetCalibration() {
    setResetting(true);
    try {
      await api.resetWakeWordCalibration();
      setSettings((prev) => ({ ...prev, voice_wake_word_transcriptions: [] }));
      showFlash("Wake word calibration cleared.");
    } catch (e) {
      showFlash(`Failed to reset calibration: ${String(e)}`, false);
    } finally {
      setResetting(false);
    }
  }

  // ── Preview TTS ────────────────────────────────────────────
  // TODO Phase 8 wave 4: call api.previewTts(voice, sample) when method is added
  const [previewTitle] = useState("Preview coming in wave 4");

  // ── Derived helpers ────────────────────────────────────────
  const currentStt    = settings.active_whisper_model ?? "ggml-base.bin";
  const currentVoice  = settings.voice_tts_voice ?? "en_US-lessac-medium.onnx";
  const wakeWord      = settings.voice_wake_word ?? "";
  const transcriptions = settings.voice_wake_word_transcriptions ?? [];
  const isCalibrated  = transcriptions.length > 0;

  const sttLabel  = STT_OPTIONS.find((o) => o.value === currentStt)?.label ?? currentStt;
  const voiceLabel = TTS_VOICE_OPTIONS.find((o) => o.value === currentVoice)?.label ?? currentVoice;

  return (
    <DetailShell
      title="Voice"
      subtitle="Wake word, speech recognition and Goose's voice."
      accent="#DB2777"
      onBack={() => go("settings")}
      headRight={
        <button
          className="mrow__btn"
          type="button"
          onClick={loadSettings}
          aria-label="Refresh voice settings"
          style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
        >
          <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
        </button>
      }
    >
      {/* Flash feedback */}
      {flash && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: flash.ok ? "#f0fdf4" : "#fef2f2",
            color:      flash.ok ? "#16a34a" : "#dc2626",
            border:     `1px solid ${flash.ok ? "#bbf7d0" : "#fecaca"}`,
          }}
          role="status"
          aria-live="polite"
        >
          {flash.text}
        </div>
      )}

      {/* Offline error banner */}
      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: "#fffbeb",
            color: "#92400e",
            border: "1px solid #fde68a",
          }}
        >
          {error}
        </div>
      )}

      {/* ── Listening card ──────────────────────────────────── */}
      <Card title="Listening">
        <Row
          label="Hands-free mode"
          sub="Always listening for the wake word"
          control={
            <Toggle
              on={handsFree}
              onChange={handleHandsFreeChange}
            />
          }
        />

        {/* Wake word + calibration status */}
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Wake word</span>
            <span className="srow__sub">
              {loading ? (
                <span style={{ display: "inline-block", height: 10, width: 80, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
              ) : wakeWord ? (
                <>
                  <span style={{ fontFamily: "var(--font-mono)", fontSize: 12, color: "#7C3AED", fontWeight: 600 }}>
                    &ldquo;{wakeWord}&rdquo;
                  </span>
                  {" "}
                  <span
                    style={{
                      display: "inline-block",
                      width: 6,
                      height: 6,
                      borderRadius: "50%",
                      background: isCalibrated ? "#16a34a" : "#f59e0b",
                      verticalAlign: "middle",
                      marginLeft: 4,
                    }}
                    title={isCalibrated ? `${transcriptions.length} calibration variant${transcriptions.length !== 1 ? "s" : ""}` : "Not calibrated"}
                  />
                  {" "}
                  <span style={{ fontSize: 11, color: "var(--color-text-tertiary)" }}>
                    {isCalibrated ? `${transcriptions.length} variant${transcriptions.length !== 1 ? "s" : ""}` : "not calibrated"}
                  </span>
                </>
              ) : (
                <span style={{ color: "var(--color-text-tertiary)", fontStyle: "italic" }}>not set</span>
              )}
            </span>
          </span>
          <span className="srow__control" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
            {isCalibrated && (
              <button
                className="mrow__btn"
                type="button"
                disabled={resetting}
                onClick={handleResetCalibration}
                aria-label="Reset wake word calibration"
                title="Clear all calibration data — detector reverts to raw phrase matching"
                style={{ display: "flex", alignItems: "center", gap: 4, fontSize: 12 }}
              >
                {resetting
                  ? <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} />
                  : <RotateCcw size={12} strokeWidth={2} />
                }
                Reset
              </button>
            )}
          </span>
        </div>

        <Row
          label="Push-to-talk shortcut"
          sub="Cmd+Shift+V from anywhere"
          control={<Toggle on={true} />}
        />
      </Card>

      {/* ── Speech-to-text card ─────────────────────────────── */}
      <Card title="Speech-to-text">
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Recognition model</span>
            <span className="srow__sub">
              {loading ? (
                <span style={{ display: "inline-block", height: 10, width: 120, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
              ) : (
                sttLabel
              )}
            </span>
          </span>
          <span className="srow__control" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
            <select
              aria-label="STT model"
              value={currentStt}
              onChange={(e) => handleSttChange(e.target.value)}
              disabled={loading}
              style={{
                height: 32,
                border: "1px solid var(--line, #e2e8f0)",
                borderRadius: 8,
                padding: "0 28px 0 10px",
                fontSize: 13,
                fontFamily: "var(--font-body, inherit)",
                background: "var(--tile-bg, #fff)",
                color: "var(--ink, #1a1a2e)",
                cursor: loading ? "not-allowed" : "pointer",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 10px center",
                minWidth: 180,
              }}
            >
              {STT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </span>
        </div>

        <Row
          label="Language"
          control={
            <select
              aria-label="Recognition language"
              defaultValue="auto"
              style={{
                height: 32,
                border: "1px solid var(--line, #e2e8f0)",
                borderRadius: 8,
                padding: "0 28px 0 10px",
                fontSize: 13,
                fontFamily: "var(--font-body, inherit)",
                background: "var(--tile-bg, #fff)",
                color: "var(--ink, #1a1a2e)",
                cursor: "pointer",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 10px center",
                minWidth: 120,
              }}
            >
              <option value="auto">Auto</option>
              <option value="en">English</option>
            </select>
          }
        />
      </Card>

      {/* ── Goose's voice card ──────────────────────────────── */}
      <Card
        title="Goose's voice"
        right={
          <button
            className="mrow__btn"
            type="button"
            disabled
            title={previewTitle}
            style={{ display: "flex", alignItems: "center", gap: 4, opacity: 0.45, cursor: "not-allowed" }}
          >
            <Play size={12} color="#7C3AED" strokeWidth={2} /> Preview
          </button>
        }
      >
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Voice</span>
            <span className="srow__sub">
              {loading ? (
                <span style={{ display: "inline-block", height: 10, width: 100, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
              ) : (
                voiceLabel
              )}
            </span>
          </span>
          <span className="srow__control" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
            <select
              aria-label="TTS voice"
              value={currentVoice}
              onChange={(e) => handleTtsVoiceChange(e.target.value)}
              disabled={loading}
              style={{
                height: 32,
                border: "1px solid var(--line, #e2e8f0)",
                borderRadius: 8,
                padding: "0 28px 0 10px",
                fontSize: 13,
                fontFamily: "var(--font-body, inherit)",
                background: "var(--tile-bg, #fff)",
                color: "var(--ink, #1a1a2e)",
                cursor: loading ? "not-allowed" : "pointer",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 10px center",
                minWidth: 180,
              }}
            >
              {TTS_VOICE_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </span>
        </div>

        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Speaking rate</span>
            {/* TODO Phase 8 wave 4: persist via api.updateSettings({ tts_speed }) */}
          </span>
          <span className="srow__control" style={{ flex: 1, maxWidth: 240 }}>
            <Slider
              min={50}
              max={150}
              value={ttsSpeed}
              suffix="%"
              onChange={handleSpeedChange}
            />
          </span>
        </div>
      </Card>

      {/* ── HubIco usage (invisible — keeps import alive for linter) ── */}
      {/* Using SICN paths inline on SVGs keeps the bundle clean */}
      <svg width={0} height={0} aria-hidden style={{ position: "absolute" }}>
        <symbol id="v-mic"     viewBox="0 0 24 24"><path d={SICN.mic}     fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" /></symbol>
        <symbol id="v-speaker" viewBox="0 0 24 24"><path d={SICN.speaker} fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" /></symbol>
        <symbol id="v-text"    viewBox="0 0 24 24"><path d={SICN.text}    fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" /></symbol>
      </svg>
    </DetailShell>
  );
}
