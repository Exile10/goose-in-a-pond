import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

interface TraceStep {
  label: string;
  timestamp: number;
}

export default function AgentTrace() {
  const [steps, setSteps] = useState<TraceStep[]>([]);

  useEffect(() => {
    const unlistenTool = listen<{ tool: string }>("tool-result", (event) => {
      const label = event.payload.tool.replace("giap__", "").replace(/_/g, " ");
      setSteps((prev) => {
        const next = [...prev, { label, timestamp: Date.now() }];
        return next.slice(-6); // Keep last 6 steps
      });
    });

    // Clear trace when a new conversation turn starts
    const unlistenTranscript = listen("transcript", () => {
      setSteps([]);
    });

    return () => {
      unlistenTool.then((fn) => fn());
      unlistenTranscript.then((fn) => fn());
    };
  }, []);

  if (steps.length === 0) return null;

  return (
    <div className="agent-trace">
      {steps.map((step, i) => (
        <>
          {i > 0 && <span className="agent-trace-arrow" key={`arrow-${i}`}>›</span>}
          <span className="agent-trace-step" key={`step-${i}`}>{step.label}</span>
        </>
      ))}
    </div>
  );
}
