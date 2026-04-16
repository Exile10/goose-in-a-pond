import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./canvas.css";
import { VoiceOrb } from "../components/VoiceOrb";
import { TranscriptFeed } from "../components/TranscriptFeed";
import { ContextCard } from "../components/ContextCard";
import type { VoiceState, TranscriptMessage, ContextCard as ContextCardType } from "../state/reducer";
import { nextTranscriptId, nextCardId } from "../state/reducer";

const SILENCE_TIMEOUT_MS = 1500;

export function CanvasOverlay() {
  const [orbState, setOrbState]     = useState<VoiceState>("idle");
  const [transcript, setTranscript] = useState<TranscriptMessage[]>([]);
  const [cards, setCards]           = useState<ContextCardType[]>([]);
  const [audioLevel, setAudioLevel] = useState(0);

  const isRecordingRef  = useRef(false);
  const silenceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ── Silence detection ───────────────────────────────────
  const clearSilenceTimer = useCallback(() => {
    if (silenceTimerRef.current !== null) {
      clearTimeout(silenceTimerRef.current);
      silenceTimerRef.current = null;
    }
  }, []);

  const startSilenceTimer = useCallback(() => {
    clearSilenceTimer();
    silenceTimerRef.current = setTimeout(() => {
      if (isRecordingRef.current) stopAndSend();
    }, SILENCE_TIMEOUT_MS);
  }, [clearSilenceTimer]); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Voice pipeline ───────────────────────────────────────
  async function startRecording() {
    isRecordingRef.current = true;
    setOrbState("recording");
    setTranscript([]);
    setCards([]);
    try {
      await invoke("start_recording");
    } catch (e) {
      console.error("start_recording:", e);
    }
  }

  async function stopAndSend() {
    if (!isRecordingRef.current) return;
    isRecordingRef.current = false;
    clearSilenceTimer();
    setOrbState("thinking");
    try {
      const wavBytes = await invoke<number[]>("stop_recording");
      // Seed empty agent message for token streaming
      setTranscript((prev) => [
        ...prev,
        { id: nextTranscriptId(), role: "agent" as const, text: "", timestamp: Date.now() },
      ]);
      await invoke("run_voice_pipeline", { wavBytes, sessionId: undefined, authToken: "" });
    } catch (e) {
      console.error("voice pipeline:", e);
      setOrbState("error");
      setTimeout(() => setOrbState("idle"), 3000);
    }
  }

  async function hide() {
    clearSilenceTimer();
    if (isRecordingRef.current) {
      isRecordingRef.current = false;
      try { await invoke("abort_recording"); } catch { /* ignore */ }
    }
    setOrbState("idle");
    try { await invoke("hide_canvas"); } catch { /* ignore */ }
  }

  // ── Keyboard ─────────────────────────────────────────────
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => { if (e.key === "Escape") hide(); };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Tauri events ─────────────────────────────────────────
  useEffect(() => {
    const unlisten: Array<() => void> = [];

    listen("canvas-start-listen", () => {
      if (!isRecordingRef.current) startRecording();
    }).then((u) => unlisten.push(u));

    listen("canvas-hotkey", () => {
      if (isRecordingRef.current) stopAndSend();
    }).then((u) => unlisten.push(u));

    listen("recording-started", () => {
      isRecordingRef.current = true;
      setOrbState("recording");
    }).then((u) => unlisten.push(u));

    listen("recording-aborted", () => {
      isRecordingRef.current = false;
      clearSilenceTimer();
      setOrbState("idle");
    }).then((u) => unlisten.push(u));

    listen<number>("audio-level", (e) => {
      const level = e.payload;
      setAudioLevel(level);
      if (isRecordingRef.current) {
        if (level > 0.015) clearSilenceTimer();
        else startSilenceTimer();
      }
    }).then((u) => unlisten.push(u));

    listen<string>("transcript", (e) => {
      setTranscript((prev) => [
        ...prev,
        { id: nextTranscriptId(), role: "user" as const, text: e.payload, timestamp: Date.now() },
      ]);
      setOrbState("thinking");
    }).then((u) => unlisten.push(u));

    listen<{ token: string; done: boolean }>("response-token", (e) => {
      setTranscript((prev) => {
        if (!prev.length) return prev;
        const last = prev[prev.length - 1];
        if (last.role !== "agent") return prev;
        return [...prev.slice(0, -1), { ...last, text: last.text + e.payload.token }];
      });
    }).then((u) => unlisten.push(u));

    listen<{ tool: string; data: Record<string, unknown>; timestamp_ms: number }>(
      "tool-result",
      (e) => {
        setCards((prev) => [
          ...prev,
          { id: nextCardId(), tool: e.payload.tool, data: e.payload.data, timestamp_ms: e.payload.timestamp_ms },
        ]);
      },
    ).then((u) => unlisten.push(u));

    listen("tts-start", () => setOrbState("speaking")).then((u) => unlisten.push(u));
    listen("tts-end",   () => setOrbState("idle")).then((u) => unlisten.push(u));

    listen<string>("pipeline-error", () => {
      setOrbState("error");
      setTimeout(() => setOrbState("idle"), 3500);
    }).then((u) => unlisten.push(u));

    return () => {
      clearSilenceTimer();
      unlisten.forEach((u) => u());
    };
  }, [clearSilenceTimer, startSilenceTimer]);

  const stateLabels: Record<VoiceState, string> = {
    idle:      "Idle — say something",
    recording: "Listening…",
    thinking:  "Thinking…",
    speaking:  "Speaking…",
    error:     "Error",
  };

  return (
    <div className="canvas-overlay">
      {/* Header — drag handle */}
      <header
        className="canvas-header"
        onMouseDown={async () => {
          try { await getCurrentWindow().startDragging(); } catch { /* ignore */ }
        }}
      >
        <div className="canvas-drag-pill" />
        <VoiceOrb state={orbState} size="sm" audioLevel={audioLevel} />
        <span className="canvas-status-label">{stateLabels[orbState]}</span>
        <button
          className="canvas-dismiss"
          onClick={hide}
          onMouseDown={(e) => e.stopPropagation()}
          aria-label="Close canvas"
        >
          ×
        </button>
      </header>

      {/* Transcript */}
      <div className="canvas-transcript">
        <TranscriptFeed messages={transcript} maxHeight="120px" compact />
      </div>

      {/* Context cards */}
      {cards.length > 0 && (
        <div className="canvas-cards">
          {cards.map((card) => (
            <div key={card.id} className="canvas-card-in">
              <ContextCard card={card} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
