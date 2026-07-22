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
  // Tracks which transcript entry response-token should append to. Needed
  // because the backend's own "transcript" (user text) event arrives *during*
  // run_voice_pipeline — after the empty agent placeholder below is already
  // seeded — so "the last message is the agent" is not a safe assumption;
  // tokens must target this id directly instead of array position.
  const pendingAgentIdRef = useRef<number | null>(null);

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
      const agentId = nextTranscriptId();
      pendingAgentIdRef.current = agentId;
      setTranscript((prev) => [
        ...prev,
        { id: agentId, role: "agent" as const, text: "", timestamp: Date.now() },
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
  //
  // Collect the listen() *promises* themselves (not their resolved unlisten
  // functions via .then()). React 18 StrictMode double-invokes this effect in
  // dev (mount -> cleanup -> mount again); listen() hasn't resolved yet when
  // that first cleanup runs, so a `.then((u) => arr.push(u))` pattern leaves
  // the array empty at cleanup time — the first mount's listeners are never
  // removed, and every real Tauri event ends up handled twice (each
  // response-token duplicated into local transcript state). Pushing the
  // promise synchronously and resolving it in cleanup closes that race.
  useEffect(() => {
    const unlistenPromises: Array<Promise<() => void>> = [];

    unlistenPromises.push(listen("canvas-start-listen", () => {
      if (!isRecordingRef.current) startRecording();
    }));

    unlistenPromises.push(listen("canvas-hotkey", () => {
      if (isRecordingRef.current) stopAndSend();
    }));

    // recording-started: no longer used — startRecording() sets state
    // directly, so calibration recordings don't cross-contaminate.

    unlistenPromises.push(listen("recording-aborted", () => {
      isRecordingRef.current = false;
      clearSilenceTimer();
      setOrbState("idle");
    }));

    unlistenPromises.push(listen<number>("audio-level", (e) => {
      const level = e.payload;
      setAudioLevel(level);
      if (isRecordingRef.current) {
        if (level > 0.015) clearSilenceTimer();
        else startSilenceTimer();
      }
    }));

    unlistenPromises.push(listen<{ text: string }>("transcript", (e) => {
      setTranscript((prev) => [
        ...prev,
        { id: nextTranscriptId(), role: "user" as const, text: e.payload.text, timestamp: Date.now() },
      ]);
      setOrbState("thinking");
    }));

    unlistenPromises.push(listen<{ token: string; done: boolean }>("response-token", (e) => {
      const targetId = pendingAgentIdRef.current;
      if (targetId === null) return;
      if (e.payload.done) { pendingAgentIdRef.current = null; return; }
      setTranscript((prev) =>
        prev.map((m) => (m.id === targetId ? { ...m, text: m.text + e.payload.token } : m)),
      );
    }));

    unlistenPromises.push(listen<{ tool: string; data: Record<string, unknown>; timestamp_ms: number }>(
      "tool-result",
      (e) => {
        setCards((prev) => [
          ...prev,
          { id: nextCardId(), tool: e.payload.tool, data: e.payload.data, timestamp_ms: e.payload.timestamp_ms },
        ]);
      },
    ));

    unlistenPromises.push(listen("tts-start", () => setOrbState("speaking")));
    unlistenPromises.push(listen("tts-end",   () => setOrbState("idle")));

    unlistenPromises.push(listen<string>("pipeline-error", () => {
      setOrbState("error");
      setTimeout(() => setOrbState("idle"), 3500);
    }));

    return () => {
      clearSilenceTimer();
      unlistenPromises.forEach((p) => p.then((u) => u()).catch(() => {}));
    };
  }, [clearSilenceTimer, startSilenceTimer]);

  const stateLabels: Record<VoiceState, string> = {
    idle:      "Idle — say something",
    wait:      "Waiting for wake word…",
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
