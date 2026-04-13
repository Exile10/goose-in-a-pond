import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import VoiceOrb, { OrbState } from "./VoiceOrb";
import TranscriptFeed from "./TranscriptFeed";
import ContextCards from "./ContextCards";
import AgentTrace from "./AgentTrace";

export default function CanvasOverlay() {
  const [orbState, setOrbState] = useState<OrbState>("idle");
  const [isDragging, setIsDragging] = useState(false);

  // Listen for pipeline state events from Rust backend
  useEffect(() => {
    const unsubs: Array<Promise<() => void>> = [];

    unsubs.push(
      listen("canvas-start-listen", async () => {
        setOrbState("listening");
        await invoke("start_recording").catch(console.error);
      })
    );

    unsubs.push(
      listen("canvas-hotkey", async () => {
        if (orbState === "listening") {
          await commitRecording();
        } else if (orbState === "idle") {
          await getCurrentWindow().hide();
        }
      })
    );

    unsubs.push(
      listen("transcript", () => setOrbState("thinking"))
    );

    unsubs.push(
      listen("tts-start", () => setOrbState("speaking"))
    );

    unsubs.push(
      listen("tts-end", () => setOrbState("idle"))
    );

    unsubs.push(
      listen("pipeline-error", () => {
        setOrbState("error");
        setTimeout(() => setOrbState("idle"), 3000);
      })
    );

    // When response-token done arrives (no TTS), go back to idle
    unsubs.push(
      listen<{ done: boolean }>("response-token", (e) => {
        if (e.payload.done && orbState === "thinking") {
          // If TTS plays, tts-start will override this
          setTimeout(() => {
            setOrbState((s) => (s === "thinking" ? "idle" : s));
          }, 500);
        }
      })
    );

    return () => {
      unsubs.forEach((p) => p.then((fn) => fn()));
    };
  }, [orbState]);

  const commitRecording = useCallback(async () => {
    setOrbState("thinking");
    try {
      const wavBytes = await invoke<number[]>("stop_recording");
      await invoke("run_voice_pipeline", { wavBytes });
    } catch (e) {
      console.error("Voice pipeline error:", e);
      setOrbState("error");
      setTimeout(() => setOrbState("idle"), 3000);
    }
  }, []);

  // VAD: auto-commit after 1.5s of silence (detect via low audio level)
  const silenceTimeoutRef = { current: 0 };
  useEffect(() => {
    const unlisten = listen<number>("audio-level", (e) => {
      if (orbState !== "listening") return;
      const level = e.payload;
      if (level < 0.02) {
        // Silence detected
        if (!silenceTimeoutRef.current) {
          silenceTimeoutRef.current = window.setTimeout(() => {
            silenceTimeoutRef.current = 0;
            commitRecording();
          }, 1500);
        }
      } else {
        // Speech detected — cancel silence timer
        if (silenceTimeoutRef.current) {
          clearTimeout(silenceTimeoutRef.current);
          silenceTimeoutRef.current = 0;
        }
      }
    });
    return () => {
      unlisten.then((fn) => fn());
      if (silenceTimeoutRef.current) clearTimeout(silenceTimeoutRef.current);
    };
  }, [orbState, commitRecording]);

  // Keyboard: Escape dismisses canvas
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        if (orbState === "listening") {
          invoke("abort_recording");
          setOrbState("idle");
        }
        getCurrentWindow().hide();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [orbState]);

  // Drag to reposition — use Tauri's startDragging
  async function onDragStart(e: React.MouseEvent) {
    if ((e.target as HTMLElement).closest(".canvas-dismiss")) return;
    e.preventDefault();
    setIsDragging(true);
    await getCurrentWindow().startDragging();
    setIsDragging(false);
  }

  return (
    <div className="canvas-overlay" style={{ cursor: isDragging ? "grabbing" : undefined }}>
      {/* Drag handle */}
      <div className="canvas-drag-handle" onMouseDown={onDragStart} />

      {/* Dismiss button */}
      <button
        className="canvas-dismiss"
        onClick={() => getCurrentWindow().hide()}
        title="Hide canvas (Escape)"
      >
        ×
      </button>

      {/* Voice orb — always shown */}
      <VoiceOrb state={orbState} />

      {/* Live transcript */}
      <TranscriptFeed />

      {/* Dynamic context cards from tool calls */}
      <ContextCards />

      {/* Agent tool-call breadcrumb */}
      <AgentTrace />
    </div>
  );
}
