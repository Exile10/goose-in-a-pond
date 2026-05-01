// ────────────────────────────────────────────────────────────
// VoiceMode — Pure UI component
//
// Zero backend logic. Renders orb, waveform, transcript,
// and action buttons. All logic in useVoicePipeline.
// ────────────────────────────────────────────────────────────

import React from "react";
import { Button } from "@heroui/react";
import { ChevronLeft, Mic, Square, Trash2 } from "lucide-react";
import { VoiceOrb } from "../../components/VoiceOrb";
import { AudioWaves } from "../../components/AudioWaves";
import { TranscriptFeed } from "../../components/TranscriptFeed";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { useVoicePipeline } from "./useVoicePipeline";

// ── CountdownRing (inline SVG) ─────────────────────────────

function CountdownRing({ seconds, maxSeconds }: { seconds: number; maxSeconds: number }) {
  const r = 12;
  const c = 2 * Math.PI * r;
  const frac = maxSeconds > 0 ? seconds / maxSeconds : 0;
  return (
    <svg width={32} height={32} viewBox="0 0 32 32" style={{ flexShrink: 0 }}>
      <circle cx={16} cy={16} r={r} fill="none" stroke="var(--grey-200)" strokeWidth={2.5} />
      <circle
        cx={16} cy={16} r={r} fill="none"
        stroke="var(--color-accent)" strokeWidth={2.5}
        strokeDasharray={c} strokeDashoffset={c * (1 - frac)}
        strokeLinecap="round"
        transform="rotate(-90 16 16)"
        style={{ transition: "stroke-dashoffset 1s linear" }}
      />
      <text x={16} y={16} textAnchor="middle" dominantBaseline="central"
        style={{ fontSize: 9, fontFamily: "var(--font-mono)", fill: "var(--color-text-secondary)" }}>
        {seconds}
      </text>
    </svg>
  );
}

// ── State labels & colors ──────────────────────────────────

const STATE_LABELS: Record<string, string> = {
  idle: "Ready", wait: "Waiting\u2026", recording: "Listening\u2026",
  thinking: "Thinking\u2026", speaking: "Speaking\u2026", error: "Error",
};

const STATE_COLORS: Record<string, string> = {
  idle: "#8E8E93", wait: "#8C4BFF", recording: "#8C4BFF",
  thinking: "#FF9500", speaking: "#34C759", error: "#FF3B30",
};

// ── Component ──────────────────────────────────────────────

export function VoiceMode() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const pipeline = useVoicePipeline();

  const { voiceState } = state;
  const isRecording = voiceState === "recording";
  const isThinking = voiceState === "thinking";
  const isSpeaking = voiceState === "speaking";
  const isIdle = voiceState === "idle";
  const isWaiting = voiceState === "wait";
  const isError = voiceState === "error";
  const serverDown = !state.serverOnline;
  const stateColor = STATE_COLORS[voiceState] ?? "#8E8E93";
  const stateLabel = serverDown
    ? "Server offline"
    : isError && state.voiceError
      ? state.voiceError
      : STATE_LABELS[voiceState];

  function backToGui() {
    dispatch({ type: "SET_MODE", payload: "gui" });
  }

  return (
    <div style={styles.root}>
      {/* ── Zone 1: Status bar ─────────────────────────────── */}
      <div style={styles.statusBar}>
        <Button variant="ghost" size="sm" onPress={backToGui} aria-label="Back">
          <ChevronLeft size={14} /> Back
        </Button>
        <div style={styles.statusCenter}>
          <span style={{ ...styles.statusDot, background: stateColor }} />
          <span style={{ ...styles.statusLabel, color: stateColor }}>{stateLabel}</span>
          {state.sessionId && (
            <span style={styles.sessionHint}>&middot; Session {state.sessionId.slice(0, 6)}</span>
          )}
        </div>
        <Button
          variant="ghost" size="sm"
          onPress={pipeline.clearConversation}
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
          audioLevel={pipeline.audioLevel}
          size="lg"
          style={{ maxWidth: "480px", margin: "0 auto" }}
        />
      </div>

      {/* ── Zone 3: Transcript ────────────────────────────── */}
      <div style={styles.transcriptZone}>
        <TranscriptFeed messages={state.transcript} fillHeight />
      </div>

      {/* ── Zone 4: Action bar ────────────────────────────── */}
      <div style={styles.actionBar}>
        {isIdle && (
          <Button variant="primary" onPress={pipeline.startRecording} isDisabled={serverDown} style={styles.primaryBtn}>
            <Mic size={16} /> Start Listening
          </Button>
        )}

        {isWaiting && (
          <div style={styles.waitRow}>
            <span style={styles.waitIndicator}>
              <span style={styles.waitPulse} />
              Listening for &ldquo;{pipeline.wakeWord}&rdquo;
            </span>
            <Button variant="ghost" size="sm" onPress={pipeline.startRecording} isDisabled={serverDown} style={styles.recordNowBtn}>
              <Mic size={13} /> Record now
            </Button>
          </div>
        )}

        {isRecording && (
          <div style={styles.recordingRow}>
            <CountdownRing seconds={pipeline.secsLeft} maxSeconds={pipeline.maxSecs} />
            <Button variant="ghost" onPress={pipeline.stopAndSend} style={styles.stopBtn}>
              <Square size={14} fill="#FF3B30" color="#FF3B30" /> Send
            </Button>
            <Button variant="ghost" size="sm" onPress={pipeline.abort} style={styles.abortBtn}>
              Cancel
            </Button>
          </div>
        )}

        {isThinking && (
          <div style={styles.busyRow}>
            <span style={styles.busyDots}>&bull;&bull;&bull;</span>
            <span style={styles.busyLabel}>Thinking&hellip;</span>
          </div>
        )}

        {isSpeaking && (
          <Button variant="ghost" onPress={pipeline.abort} style={styles.interruptBtn}>
            Interrupt
          </Button>
        )}

        {isError && (
          <div style={styles.errorRow}>
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
          <kbd style={styles.kbd}>&lceil;&Sigma;V</kbd>
          {" / "}
          <kbd style={styles.kbd}>Ctrl+Shift+V</kbd>
          {" to activate"}
        </p>
      </div>
    </div>
  );
}

// ── Inline styles ──────────────────────────────────────────

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", height: "100%", background: "var(--color-bg)", overflow: "hidden" },
  statusBar: { height: 48, flexShrink: 0, borderBottom: "1px solid var(--color-border)", display: "flex", alignItems: "center", justifyContent: "space-between", padding: "0 var(--space-4)", gap: "var(--space-3)" },
  statusCenter: { display: "flex", alignItems: "center", gap: 6, flex: 1, justifyContent: "center", overflow: "hidden" },
  statusDot: { width: 7, height: 7, borderRadius: "50%", flexShrink: 0, transition: "background 0.3s" },
  statusLabel: { fontFamily: "var(--font-display)", fontWeight: 600, fontSize: 13, transition: "color 0.3s", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  sessionHint: { fontSize: 11, color: "var(--color-text-tertiary)", whiteSpace: "nowrap" },
  waveZone: { flexShrink: 0, padding: "16px var(--space-6) 8px" },
  transcriptZone: { flex: 1, overflow: "hidden", padding: "0 var(--space-5)", display: "flex", flexDirection: "column" },
  actionBar: { flexShrink: 0, height: 64, borderTop: "1px solid var(--color-border)", display: "flex", alignItems: "center", justifyContent: "center", gap: "var(--space-3)", padding: "0 var(--space-5)", position: "relative" },
  primaryBtn: { display: "flex", alignItems: "center", gap: 6 },
  recordingRow: { display: "flex", alignItems: "center", gap: 8 },
  stopBtn: { display: "flex", alignItems: "center", gap: 5, color: "#FF3B30", borderColor: "rgba(255,59,48,0.3)" },
  abortBtn: { fontSize: 12, color: "var(--color-text-secondary)" },
  waitRow: { display: "flex", alignItems: "center", gap: 12 },
  waitIndicator: { display: "flex", alignItems: "center", gap: 8, fontSize: 13, color: "#8C4BFF", fontWeight: 500 },
  waitPulse: { width: 8, height: 8, borderRadius: "50%", background: "#8C4BFF", flexShrink: 0, animation: "pulse 1.4s ease infinite" },
  recordNowBtn: { display: "flex", alignItems: "center", gap: 4, fontSize: 12, color: "var(--color-text-secondary)" },
  busyRow: { display: "flex", alignItems: "center", gap: 8 },
  busyDots: { color: "var(--color-text-tertiary)", letterSpacing: 2, animation: "pulse 1.4s ease infinite" },
  busyLabel: { fontSize: 13, color: "var(--color-text-secondary)" },
  interruptBtn: { color: "var(--color-text-secondary)" },
  errorRow: { display: "flex", alignItems: "center", gap: 8 },
  hint: { position: "absolute", bottom: 4, right: "var(--space-5)", fontSize: 10, color: "var(--color-text-tertiary)", margin: 0 },
  kbd: { display: "inline-block", fontFamily: "var(--font-mono)", fontSize: 9, background: "rgba(23,22,22,0.07)", border: "1px solid rgba(23,22,22,0.14)", borderRadius: 4, padding: "0px 4px", color: "var(--color-text-secondary)" },
};
