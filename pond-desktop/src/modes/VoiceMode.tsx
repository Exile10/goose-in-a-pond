import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Button } from "@heroui/react";
import { ChevronLeft, Mic, Square, Trash2 } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { AudioWaves } from "../components/AudioWaves";
import { TranscriptFeed } from "../components/TranscriptFeed";
import { resolveVoiceDecision } from "../voiceSummon";
import { nextTranscriptId } from "../state/reducer";
import { api } from "../api/PondApiClient";

// ── State colours ────────────────────────────────────────────
const STATE_COLORS: Record<string, string> = {
  idle:      "#8E8E93",
  recording: "#8C52FF",
  thinking:  "#FF9500",
  speaking:  "#34C759",
  error:     "#FF3B30",
};

const STATE_LABELS: Record<string, string> = {
  idle:      "Ready",
  recording: "Listening…",
  thinking:  "Thinking…",
  speaking:  "Speaking…",
  error:     "Error",
};

// ── Countdown ring ───────────────────────────────────────────
function CountdownRing({ seconds, maxSeconds }: { seconds: number; maxSeconds: number }) {
  const r = 16;
  const circumference = 2 * Math.PI * r;
  const progress = maxSeconds > 0 ? seconds / maxSeconds : 0;
  const dashOffset = circumference * (1 - progress);

  return (
    <svg width="40" height="40" style={{ transform: "rotate(-90deg)" }}>
      <circle cx="20" cy="20" r={r} fill="none" stroke="rgba(255,59,48,0.15)" strokeWidth="3" />
      <circle
        cx="20" cy="20" r={r}
        fill="none"
        stroke="#FF3B30"
        strokeWidth="3"
        strokeLinecap="round"
        strokeDasharray={circumference}
        strokeDashoffset={dashOffset}
        style={{ transition: "stroke-dashoffset 0.25s linear" }}
      />
      <text
        x="20" y="20"
        textAnchor="middle"
        dominantBaseline="central"
        style={{ transform: "rotate(90deg)", transformOrigin: "20px 20px", fontSize: "10px", fontWeight: 600, fill: "#FF3B30", fontFamily: "var(--font-mono)" }}
      >
        {seconds}
      </text>
    </svg>
  );
}

// ── Main component ───────────────────────────────────────────

export function VoiceMode() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  // Live audio level for waveform (useState so canvas re-renders with level)
  const [audioLevel, setAudioLevel] = useState(0);
  const audioLevelRef = useRef(0);
  const voiceHandledRef = useRef(0);

  // Auto-stop timer
  const [maxSecs, setMaxSecs] = useState(30);
  const [secsLeft, setSecsLeft] = useState(0);
  const stopTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const tickRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Hide canvas overlay
  useEffect(() => {
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (isTauri) invoke("hide_canvas").catch(() => undefined);
  }, []);

  // Load max recording duration from settings
  useEffect(() => {
    api.getSettings()
      .then((s) => {
        const dur = (s as Record<string, unknown>).voice_recording_duration_secs;
        if (typeof dur === "number" && dur > 0) setMaxSecs(dur);
      })
      .catch(() => undefined);
  }, []);

  // Track audio levels
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<number>("audio-level", (e) => {
      audioLevelRef.current = e.payload;
      setAudioLevel(e.payload);
    }).then((u) => { unlisten = u; });
    return () => { unlisten?.(); };
  }, []);

  // Clear countdown timers
  const clearTimers = useCallback(() => {
    if (stopTimerRef.current) { clearTimeout(stopTimerRef.current); stopTimerRef.current = null; }
    if (tickRef.current) { clearInterval(tickRef.current); tickRef.current = null; }
    setSecsLeft(0);
  }, []);

  // Start countdown visuals + auto-stop
  const startCountdown = useCallback((secs: number, onExpire: () => void) => {
    setSecsLeft(secs);
    tickRef.current = setInterval(() => {
      setSecsLeft((prev) => {
        if (prev <= 1) {
          clearInterval(tickRef.current!);
          tickRef.current = null;
          return 0;
        }
        return prev - 1;
      });
    }, 1000);
    stopTimerRef.current = setTimeout(onExpire, secs * 1000);
  }, []);

  // Clear timers when recording ends
  useEffect(() => {
    if (state.voiceState !== "recording") clearTimers();
  }, [state.voiceState, clearTimers]);

  // Respond to global voice activation hotkey
  useEffect(() => {
    if (state.voiceRequestId === voiceHandledRef.current) return;
    voiceHandledRef.current = state.voiceRequestId;

    const decision = resolveVoiceDecision({
      serverHealthy: state.serverOnline,
      isRecording: state.voiceState === "recording",
      isProcessing: state.voiceState === "thinking" || state.voiceState === "speaking",
    });

    if (decision === "start-recording") startRecording();
    else if (decision === "stop-and-send") stopAndSend();
  }, [state.voiceRequestId]); // eslint-disable-line react-hooks/exhaustive-deps

  async function startRecording() {
    try {
      await invoke("start_recording");
      startCountdown(maxSecs, stopAndSend);
    } catch (e) {
      console.error("start_recording failed:", e);
    }
  }

  async function stopAndSend() {
    clearTimers();
    try {
      dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
      const wavBytes = await invoke<number[]>("stop_recording");
      const authToken = state.sessionToken ?? "";
      const sessionId = state.sessionId ?? undefined;
      await invoke("run_voice_pipeline", { wavBytes, authToken, sessionId });
    } catch (e) {
      console.error("voice pipeline failed:", e);
      dispatch({ type: "SET_VOICE_ERROR", payload: String(e) });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
    }
  }

  async function abortRecording() {
    clearTimers();
    try {
      await invoke("abort_recording");
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    } catch { /* ignore */ }
  }

  function clearConversation() {
    dispatch({ type: "CLEAR_TRANSCRIPT" });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
  }

  function backToGui() {
    dispatch({ type: "SET_MODE", payload: "gui" });
  }

  const { voiceState } = state;
  const isRecording = voiceState === "recording";
  const isThinking  = voiceState === "thinking";
  const isSpeaking  = voiceState === "speaking";
  const isIdle      = voiceState === "idle";
  const isError     = voiceState === "error";
  const serverDown  = !state.serverOnline;
  const stateColor  = STATE_COLORS[voiceState] ?? "#8E8E93";
  const stateLabel  = serverDown ? "Server offline" : (isError && state.voiceError ? state.voiceError : STATE_LABELS[voiceState]);

  return (
    <div style={styles.root}>

      {/* ── Zone 1: Status bar ─────────────────────────────── */}
      <div style={styles.statusBar}>
        <Button variant="ghost" size="sm" onPress={backToGui} aria-label="Back">
          <ChevronLeft size={14} /> Back
        </Button>

        <div style={styles.statusCenter}>
          <span style={{ ...styles.statusDot, background: stateColor }} />
          <span style={{ ...styles.statusLabel, color: stateColor }}>
            {stateLabel}
          </span>
          {state.sessionId && (
            <span style={styles.sessionHint}>
              · Session {state.sessionId.slice(0, 6)}
            </span>
          )}
        </div>

        <Button
          variant="ghost"
          size="sm"
          onPress={clearConversation}
          isDisabled={state.transcript.length === 0}
          aria-label="Clear conversation"
        >
          <Trash2 size={13} />
        </Button>
      </div>

      {/* ── Zone 2: Waveform ──────────────────────────────── */}
      <div style={styles.waveZone}>
        <AudioWaves
          state={voiceState}
          audioLevel={audioLevel}
          size="lg"
          style={{ maxWidth: "480px", margin: "0 auto" }}
        />
      </div>

      {/* ── Zone 3: Transcript (fills remaining height) ───── */}
      <div style={styles.transcriptZone}>
        <TranscriptFeed
          messages={state.transcript}
          contextCards={state.contextCards}
          fillHeight
        />
      </div>

      {/* ── Zone 4: Action bar ────────────────────────────── */}
      <div style={styles.actionBar}>
        {isIdle && (
          <Button
            variant="primary"
            onPress={startRecording}
            isDisabled={serverDown}
            style={styles.primaryBtn}
          >
            <Mic size={16} />
            Start Listening
          </Button>
        )}

        {isRecording && (
          <div style={styles.recordingRow}>
            <CountdownRing seconds={secsLeft} maxSeconds={maxSecs} />
            <Button
              variant="ghost"
              onPress={stopAndSend}
              style={styles.stopBtn}
            >
              <Square size={14} fill="#FF3B30" color="#FF3B30" />
              Send
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onPress={abortRecording}
              style={styles.abortBtn}
            >
              Cancel
            </Button>
          </div>
        )}

        {isThinking && (
          <div style={styles.busyRow}>
            <span style={styles.busyDots}>●●●</span>
            <span style={styles.busyLabel}>Thinking…</span>
          </div>
        )}

        {isSpeaking && (
          <Button
            variant="ghost"
            onPress={abortRecording}
            style={styles.interruptBtn}
          >
            Interrupt
          </Button>
        )}

        {isError && (
          <Button
            variant="ghost"
            onPress={() => {
              dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
              dispatch({ type: "SET_VOICE_ERROR", payload: null });
            }}
          >
            Dismiss
          </Button>
        )}

        <p style={styles.hint}>
          <kbd style={styles.kbd}>⌘⇧V</kbd>
          {" / "}
          <kbd style={styles.kbd}>Ctrl+Shift+V</kbd>
          {" to activate"}
        </p>
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    height: "100%",
    background: "var(--color-bg)",
    overflow: "hidden",
  },

  // ── Status bar
  statusBar: {
    height: "48px",
    flexShrink: 0,
    borderBottom: "1px solid var(--color-border)",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "0 var(--space-4)",
    gap: "var(--space-3)",
  },
  statusCenter: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    flex: 1,
    justifyContent: "center",
    overflow: "hidden",
  },
  statusDot: {
    width: "7px",
    height: "7px",
    borderRadius: "50%",
    flexShrink: 0,
    transition: "background 0.3s",
  },
  statusLabel: {
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "13px",
    transition: "color 0.3s",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  sessionHint: {
    fontSize: "11px",
    color: "var(--color-text-tertiary)",
    whiteSpace: "nowrap",
  },

  // ── Waveform zone
  waveZone: {
    flexShrink: 0,
    padding: "16px var(--space-6) 8px",
  },

  // ── Transcript zone
  transcriptZone: {
    flex: 1,
    overflow: "hidden",
    padding: "0 var(--space-5)",
    display: "flex",
    flexDirection: "column",
  },

  // ── Action bar
  actionBar: {
    flexShrink: 0,
    height: "64px",
    borderTop: "1px solid var(--color-border)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    gap: "var(--space-3)",
    padding: "0 var(--space-5)",
  },

  // Button variants
  primaryBtn: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
  },
  recordingRow: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  stopBtn: {
    display: "flex",
    alignItems: "center",
    gap: "5px",
    color: "#FF3B30",
    borderColor: "rgba(255,59,48,0.3)",
  },
  abortBtn: {
    fontSize: "12px",
    color: "var(--color-text-secondary)",
  },
  busyRow: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  busyDots: {
    color: "var(--color-text-tertiary)",
    letterSpacing: "2px",
    animation: "pulse 1.4s ease infinite",
  },
  busyLabel: {
    fontSize: "13px",
    color: "var(--color-text-secondary)",
  },
  interruptBtn: {
    color: "var(--color-text-secondary)",
  },

  hint: {
    position: "absolute",
    bottom: "4px",
    right: "var(--space-5)",
    fontSize: "10px",
    color: "var(--color-text-tertiary)",
    margin: 0,
  },
  kbd: {
    display: "inline-block",
    fontFamily: "var(--font-mono)",
    fontSize: "9px",
    background: "rgba(23,22,22,0.07)",
    border: "1px solid rgba(23,22,22,0.14)",
    borderRadius: "4px",
    padding: "0px 4px",
    color: "var(--color-text-secondary)",
  },
};
