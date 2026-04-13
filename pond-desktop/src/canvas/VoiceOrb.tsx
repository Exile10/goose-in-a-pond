import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

export type OrbState = "idle" | "listening" | "thinking" | "speaking" | "error";

interface VoiceOrbProps {
  state: OrbState;
}

// Number of waveform bars rendered inside the orb during listening
const NUM_BARS = 7;

export default function VoiceOrb({ state }: VoiceOrbProps) {
  const [barHeights, setBarHeights] = useState<number[]>(Array(NUM_BARS).fill(4));
  const levelBufferRef = useRef<number[]>([]);

  // Listen for audio-level events from the Rust backend
  useEffect(() => {
    let unlisten: (() => void) | null = null;

    listen<number>("audio-level", (event) => {
      levelBufferRef.current.push(event.payload);
    }).then((fn) => {
      unlisten = fn;
    });

    // Smooth the level into bar heights at 30fps
    const interval = setInterval(() => {
      const buf = levelBufferRef.current.splice(0);
      if (buf.length === 0 || state !== "listening") return;
      const avg = buf.reduce((a, b) => a + b, 0) / buf.length;
      // Distribute energy across bars with a natural envelope
      const envelope = [0.4, 0.65, 0.85, 1.0, 0.85, 0.65, 0.4];
      setBarHeights(envelope.map((e) => Math.max(4, Math.round(avg * e * 28))));
    }, 33);

    return () => {
      clearInterval(interval);
      unlisten?.();
    };
  }, [state]);

  // Reset bars when not listening
  useEffect(() => {
    if (state !== "listening") {
      setBarHeights(Array(NUM_BARS).fill(4));
    }
  }, [state]);

  const stateLabel: Record<OrbState, string> = {
    idle: "Idle",
    listening: "Listening",
    thinking: "Thinking",
    speaking: "Speaking",
    error: "Error",
  };

  return (
    <div className="voice-orb-container">
      <div className={`voice-orb ${state}`}>
        {state === "listening" ? (
          <div className="orb-bars">
            {barHeights.map((h, i) => (
              <div
                key={i}
                className="orb-bar"
                style={{ height: `${h}px` }}
              />
            ))}
          </div>
        ) : (
          <span className="orb-icon">{stateIcon(state)}</span>
        )}
      </div>
      <span className="orb-state-label">{stateLabel[state]}</span>
    </div>
  );
}

function stateIcon(state: OrbState): string {
  switch (state) {
    case "thinking":  return "🧠";
    case "speaking":  return "🔊";
    case "error":     return "⚠";
    default:          return "🎙";
  }
}
