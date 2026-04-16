import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { VoiceOrb } from "../components/VoiceOrb";
import { TranscriptFeed } from "../components/TranscriptFeed";
import { resolveSummonDecision } from "../voiceSummon";
import { nextTranscriptId } from "../state/reducer";

const STATE_LABELS: Record<string, string> = {
  idle:      "Idle — ready to listen",
  recording: "Listening…",
  thinking:  "Thinking…",
  speaking:  "Speaking…",
  error:     "Something went wrong",
};

export function VoiceMode() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const audioLevelRef = useRef(0);
  const summonHandledRef = useRef(0);

  // Track audio levels for orb animation
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<number>("audio-level", (e) => {
      audioLevelRef.current = e.payload;
    }).then((u) => { unlisten = u; });
    return () => { unlisten?.(); };
  }, []);

  // Respond to global summon hotkey
  useEffect(() => {
    if (state.summonRequestId === summonHandledRef.current) return;
    summonHandledRef.current = state.summonRequestId;

    const decision = resolveSummonDecision({
      serverHealthy: state.serverOnline,
      isRecording: state.voiceState === "recording",
      isProcessing: state.voiceState === "thinking" || state.voiceState === "speaking",
    });

    if (decision === "start-recording") {
      startRecording();
    } else if (decision === "stop-and-send") {
      stopAndSend();
    }
  }, [state.summonRequestId]); // eslint-disable-line react-hooks/exhaustive-deps

  async function startRecording() {
    try {
      await invoke("start_recording");
    } catch (e) {
      console.error("start_recording failed:", e);
    }
  }

  async function stopAndSend() {
    try {
      dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
      const wavBytes = await invoke<number[]>("stop_recording");
      const sessionId = undefined;
      const authToken = state.sessionToken ?? "";
      await invoke("run_voice_pipeline", {
        wavBytes,
        sessionId,
        authToken,
      });
    } catch (e) {
      console.error("voice pipeline failed:", e);
      dispatch({ type: "SET_VOICE_ERROR", payload: String(e) });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
    }
  }

  async function abortRecording() {
    try {
      await invoke("abort_recording");
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    } catch { /* ignore */ }
  }

  function backToGui() {
    dispatch({ type: "SET_MODE", payload: "gui" });
  }

  const isRecording  = state.voiceState === "recording";
  const isBusy       = state.voiceState === "thinking" || state.voiceState === "speaking";
  const isIdle       = state.voiceState === "idle";
  const isError      = state.voiceState === "error";
  const serverDown   = !state.serverOnline;

  return (
    <div style={styles.root}>
      {/* Header */}
      <header style={styles.header}>
        <button style={styles.backBtn} onClick={backToGui} aria-label="Back to GUI mode">
          ← Back
        </button>
        <h1 style={styles.headerTitle}>Voice Mode</h1>
        <div style={{ width: 64 }} />
      </header>

      {/* Center content */}
      <div style={styles.centerCol}>
        <VoiceOrb
          state={state.voiceState}
          size="lg"
          audioLevel={audioLevelRef.current}
        />

        <p style={styles.statusLabel}>
          {serverDown ? "Server offline" : STATE_LABELS[state.voiceState]}
        </p>

        {state.voiceError && (
          <p style={styles.errorText}>{state.voiceError}</p>
        )}

        <TranscriptFeed messages={state.transcript} maxHeight="240px" />

        {/* Action buttons */}
        <div style={styles.actions}>
          {isIdle && (
            <button
              style={{ ...styles.actionBtn, ...styles.primaryBtn }}
              onClick={startRecording}
              disabled={serverDown}
            >
              🎙 Summon
            </button>
          )}
          {isRecording && (
            <>
              <button
                style={{ ...styles.actionBtn, ...styles.primaryBtn }}
                onClick={stopAndSend}
              >
                ↑ Send
              </button>
              <button
                style={{ ...styles.actionBtn, ...styles.ghostBtn }}
                onClick={abortRecording}
              >
                ✕ Cancel
              </button>
            </>
          )}
          {isBusy && (
            <button
              style={{ ...styles.actionBtn, ...styles.ghostBtn }}
              onClick={abortRecording}
            >
              Stop
            </button>
          )}
          {isError && (
            <button
              style={{ ...styles.actionBtn, ...styles.primaryBtn }}
              onClick={() => {
                dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                dispatch({ type: "SET_VOICE_ERROR", payload: null });
              }}
            >
              Dismiss
            </button>
          )}
        </div>

        <p style={styles.hint}>
          Press{" "}
          <kbd style={styles.kbd}>⌘⇧V</kbd>
          {" "}or{" "}
          <kbd style={styles.kbd}>Ctrl+Shift+V</kbd>
          {" "}to summon
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
  header: {
    height: "var(--toolbar-height)",
    borderBottom: "1px solid var(--color-border)",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "0 var(--space-5)",
    flexShrink: 0,
  },
  headerTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-md)",
    color: "var(--color-text)",
    margin: 0,
  },
  backBtn: {
    fontFamily: "var(--font-body)",
    fontSize: "var(--text-base)",
    color: "var(--color-accent)",
    background: "transparent",
    border: "none",
    cursor: "pointer",
    padding: "4px 0",
    width: "64px",
    textAlign: "left",
    fontWeight: 500,
  },
  centerCol: {
    flex: 1,
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: "var(--space-6)",
    padding: "var(--space-8)",
    overflowY: "auto",
  },
  statusLabel: {
    fontFamily: "var(--font-display)",
    fontWeight: 600,
    fontSize: "var(--text-xl)",
    color: "var(--color-text)",
    margin: 0,
    textAlign: "center",
  },
  errorText: {
    fontSize: "var(--text-sm)",
    color: "var(--color-destructive)",
    margin: 0,
    textAlign: "center",
  },
  actions: {
    display: "flex",
    gap: "var(--space-3)",
    alignItems: "center",
  },
  actionBtn: {
    height: "40px",
    padding: "0 var(--space-6)",
    borderRadius: "var(--radius-pill)",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    fontWeight: 600,
    border: "none",
    cursor: "pointer",
    display: "inline-flex",
    alignItems: "center",
    gap: "var(--space-2)",
    transition: "opacity var(--transition-fast), background var(--transition-fast)",
  },
  primaryBtn: {
    background: "var(--color-accent)",
    color: "#FFFFFF",
  },
  ghostBtn: {
    background: "rgba(23,22,22,0.06)",
    color: "var(--color-text-secondary)",
  },
  hint: {
    fontSize: "var(--text-sm)",
    color: "var(--color-text-tertiary)",
    margin: 0,
    textAlign: "center",
  },
  kbd: {
    display: "inline-block",
    fontFamily: "var(--font-mono)",
    fontSize: "10px",
    background: "rgba(23,22,22,0.07)",
    border: "1px solid rgba(23,22,22,0.14)",
    borderRadius: "4px",
    padding: "1px 5px",
    color: "var(--color-text-secondary)",
  },
};
