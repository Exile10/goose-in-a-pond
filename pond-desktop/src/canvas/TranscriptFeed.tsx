import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

interface TranscriptLine {
  id: number;
  role: "user" | "agent";
  text: string;
}

export default function TranscriptFeed() {
  const [lines, setLines] = useState<TranscriptLine[]>([]);
  const [agentBuffer, setAgentBuffer] = useState("");
  const feedRef = useRef<HTMLDivElement>(null);
  const idRef = useRef(0);

  // Listen for user transcript (one-shot after recording)
  useEffect(() => {
    const unlistenTranscript = listen<{ text: string }>("transcript", (event) => {
      const id = ++idRef.current;
      setLines((prev) => [...prev.slice(-20), { id, role: "user", text: event.payload.text }]);
    });

    // Listen for streaming agent response tokens
    const unlistenToken = listen<{ token: string; done: boolean }>(
      "response-token",
      (event) => {
        if (event.payload.done) {
          // Flush buffer as completed agent line
          setAgentBuffer((buf) => {
            if (buf.trim()) {
              const id = ++idRef.current;
              setLines((prev) => [...prev.slice(-20), { id, role: "agent", text: buf }]);
            }
            return "";
          });
        } else {
          setAgentBuffer((buf) => buf + event.payload.token);
        }
      }
    );

    return () => {
      unlistenTranscript.then((fn) => fn());
      unlistenToken.then((fn) => fn());
    };
  }, []);

  // Auto-scroll to bottom on new content
  useEffect(() => {
    const el = feedRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines, agentBuffer]);

  if (lines.length === 0 && !agentBuffer) return null;

  return (
    <div className="transcript-feed" ref={feedRef}>
      {lines.map((line) => (
        <div key={line.id} className={`transcript-line ${line.role}`}>
          {line.text}
        </div>
      ))}
      {/* Streaming agent buffer (not yet committed) */}
      {agentBuffer && (
        <div className="transcript-line agent">{agentBuffer}</div>
      )}
    </div>
  );
}
