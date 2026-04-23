import { useEffect, useRef, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Button } from "@heroui/react";
import { ChevronLeft, Mic, Square, Trash2 } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { AudioWaves } from "../components/AudioWaves";
import { TranscriptFeed } from "../components/TranscriptFeed";
import { resolveVoiceDecision } from "../voiceSummon";
import { api } from "../api/PondApiClient";

// ── State colours ────────────────────────────────────────────
const STATE_COLORS: Record<string, string> = {
  idle:      "#8E8E93",
  wait:      "#8C4BFF",  // dim brand purple — passive wake-word watch
  recording: "#8C4BFF",
  thinking:  "#FF9500",
  speaking:  "#34C759",
  error:     "#FF3B30",
};

const STATE_LABELS: Record<string, string> = {
  idle:      "Ready",
  wait:      "Waiting…",
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
  const prevVoiceStateRef = useRef(state.voiceState);

  // Live refs to the latest startRecording / stopAndSend so that the
  // wake-word listener (registered once at mount) always calls the
  // current version of these functions — avoiding stale-closure bugs
  // where maxSecs or sessionToken captured at mount are outdated.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const startRecordingRef = useRef<() => Promise<void>>(async () => {});
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const stopAndSendRef = useRef<() => Promise<void>>(async () => {});

  // Always keep refs pointing at the current render's versions.
  // (Must be assigned in the render body, before any effects run.)
  // These are updated below after the functions are defined.

  // Active wake word (shown in the wait state UI)
  const [wakeWord, setWakeWord] = useState("");

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

  // Track audio levels (Tauri only — listen() crashes in browser env)
  useEffect(() => {
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!isTauri) return;
    let unlisten: (() => void) | null = null;
    listen<number>("audio-level", (e) => {
      audioLevelRef.current = e.payload;
      setAudioLevel(e.payload);
    }).then((u) => { unlisten = u; });
    return () => { unlisten?.(); };
  }, []);

  // ── Wake word listener ───────────────────────────────────────
  // On mount: load wake word setting and start passive listening loop if configured.
  useEffect(() => {
    let wakeUnlisten: (() => void) | null = null;
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

    api.getSettings()
      .then(async (s) => {
        const wakeWord = (s as Record<string, unknown>).voice_wake_word;
        if (!wakeWord || typeof wakeWord !== "string" || !wakeWord.trim()) return;

        setWakeWord(wakeWord.trim());

        // Enter wait state and start the passive listener
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
        if (isTauri) {
          try {
            await invoke("start_wake_listener", { wakeWord: wakeWord.trim() });
          } catch (e) {
            // Most likely cause: microphone permission denied on macOS.
            // Show the error so the user knows why it's silent.
            const msg = String(e);
            const isMicDenied =
              msg.toLowerCase().includes("permission") ||
              msg.toLowerCase().includes("access") ||
              msg.toLowerCase().includes("device") ||
              msg.toLowerCase().includes("denied");

            dispatch({
              type: "SET_VOICE_ERROR",
              payload: isMicDenied
                ? "Microphone access denied. Go to System Settings → Privacy → Microphone and allow this app."
                : `Wake listener failed: ${msg}`,
            });
            dispatch({ type: "SET_VOICE_STATE", payload: "error" });
            return; // don't register the listener if the backend failed
          }

          // Listen for wake word detection → hand off to record.
          wakeUnlisten = await listen("wake-word-detected", () => {
            invoke("stop_wake_listener").catch(() => undefined);
            startRecordingRef.current();
          });
        }
      })
      .catch(() => undefined);

    return () => {
      // Teardown: stop passive listener and return to idle
      wakeUnlisten?.();
      if (isTauri) {
        invoke("stop_wake_listener").catch(() => undefined);
      }
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Speak → Wait loop closure ────────────────────────────────
  // After the pipeline finishes (speaking/error → idle), restart the passive
  // wake listener so the cycle completes: wait → listen → think → speak → wait
  useEffect(() => {
    const prev = prevVoiceStateRef.current;
    prevVoiceStateRef.current = state.voiceState;

    if (state.voiceState === "idle" && (prev === "speaking" || prev === "error")) {
      const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
      api.getSettings()
        .then(async (s) => {
          const ww = (s as Record<string, unknown>).voice_wake_word;
          if (!ww || typeof ww !== "string" || !ww.trim()) return;
          setWakeWord(ww.trim());
          dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
          if (isTauri) {
            await invoke("start_wake_listener", { wakeWord: ww.trim() }).catch(() => undefined);
          }
        })
        .catch(() => undefined);
    }
  }, [state.voiceState]); // eslint-disable-line react-hooks/exhaustive-deps

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

    if (decision === "start-recording") startRecordingRef.current();
    else if (decision === "stop-and-send") stopAndSendRef.current();
  }, [state.voiceRequestId]); // eslint-disable-line react-hooks/exhaustive-deps

  async function startRecording() {
    try {
      await invoke("start_recording");
      // Use a ref-wrapped callback so stopAndSend is always the latest version
      // even when called from the countdown timer set up here.
      startCountdown(maxSecs, () => stopAndSendRef.current());
    } catch (e) {
      console.error("start_recording failed:", e);
    }
  }

  async function stopAndSend() {
    clearTimers();
    try {
      dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
      const wavBytes = await invoke<number[]>("stop_recording");
      // Read directly from state — this function is always the latest render's
      // version (kept alive via stopAndSendRef), so state is never stale here.
      const authToken = state.sessionToken ?? "";
      const sessionId = state.sessionId ?? undefined;
      await invoke("run_voice_pipeline", { wavBytes, authToken, sessionId });
    } catch (e) {
      console.error("voice pipeline failed:", e);
      dispatch({ type: "SET_VOICE_ERROR", payload: String(e) });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
    }
  }

  // Sync live refs — runs on every render so the wake listener always calls
  // the most up-to-date versions of these functions.
  startRecordingRef.current = startRecording;
  stopAndSendRef.current = stopAndSend;

  async function abortRecording() {
    clearTimers();
    try {
      await invoke("abort_recording");
      // Return to "wait" if the wake listener is active, otherwise "idle"
      const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
      if (isTauri) {
        const s = await api.getSettings().catch(() => ({}));
        const wakeWord = (s as Record<string, unknown>).voice_wake_word;
        if (wakeWord && typeof wakeWord === "string" && wakeWord.trim()) {
          await invoke("start_wake_listener", { wakeWord: wakeWord.trim() }).catch(() => undefined);
          dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
          return;
        }
      }
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
  const isWaiting   = voiceState === "wait";
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

        {isWaiting && (
          <div style={styles.waitRow}>
            <span style={styles.waitIndicator} aria-label="wake word active">
              <span style={styles.waitPulse} />
              Listening for &ldquo;{wakeWord}&rdquo;
            </span>
            <Button
              variant="ghost"
              size="sm"
              onPress={startRecording}
              isDisabled={serverDown}
              style={styles.recordNowBtn}
            >
              <Mic size={13} />
              Record now
            </Button>
          </div>
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
          <div style={styles.errorRow}>
            {state.voiceError?.includes("Microphone access denied") && (
              <Button
                variant="ghost"
                size="sm"
                onPress={() => {
                  const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
                  if (isTauri) {
                    invoke("open_privacy_mic").catch(() => undefined);
                  }
                }}
                style={styles.openSettingsBtn}
              >
                Open Privacy Settings
              </Button>
            )}
            <Button
              variant="ghost"
              onPress={() => {
                dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                dispatch({ type: "SET_VOICE_ERROR", payload: null });
              }}
            >
              Dismiss
            </Button>
          </div>
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
  waitRow: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
  },
  waitIndicator: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    fontSize: "13px",
    color: "#8C4BFF",
    fontWeight: 500,
  },
  waitPulse: {
    width: "8px",
    height: "8px",
    borderRadius: "50%",
    background: "#8C4BFF",
    flexShrink: 0,
    animation: "pulse 1.4s ease infinite",
  },
  recordNowBtn: {
    display: "flex",
    alignItems: "center",
    gap: "4px",
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
  errorRow: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  openSettingsBtn: {
    fontSize: "12px",
    color: "#FF3B30",
    borderColor: "rgba(255,59,48,0.3)",
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
