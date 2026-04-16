import { useEffect, useRef } from "react";
import type { TranscriptMessage } from "../state/reducer";

interface Props {
  messages: TranscriptMessage[];
  maxHeight?: string;
  compact?: boolean;
}

export function TranscriptFeed({ messages, maxHeight = "200px", compact = false }: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  if (messages.length === 0) {
    return (
      <div style={{ ...styles.root, maxHeight }}>
        <p style={styles.empty}>
          {compact ? "Say something…" : "No messages yet. Summon Pond to start."}
        </p>
      </div>
    );
  }

  return (
    <div style={{ ...styles.root, maxHeight }} role="log" aria-live="polite" aria-label="Conversation">
      {messages.map((msg) => (
        <div
          key={msg.id}
          style={{
            ...styles.message,
            ...(msg.role === "user" ? styles.userMsg : styles.agentMsg),
            ...(compact ? styles.compact : {}),
          }}
        >
          {!compact && (
            <span style={{
              ...styles.roleLabel,
              color: msg.role === "user" ? "rgba(23,22,22,0.45)" : "#8C52FF",
            }}>
              {msg.role === "user" ? "You" : "Pond"}
            </span>
          )}
          <p style={{ ...styles.text, fontSize: compact ? "12px" : "13px" }}>
            {msg.text || (msg.role === "agent" ? <span style={styles.thinking}>●●●</span> : "")}
          </p>
        </div>
      ))}
      <div ref={bottomRef} />
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    width: "100%",
    padding: "4px 0",
  },
  empty: {
    textAlign: "center",
    fontSize: "12px",
    color: "rgba(23,22,22,0.38)",
    margin: "12px 0",
  },
  message: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  userMsg: {
    alignItems: "flex-end",
  },
  agentMsg: {
    alignItems: "flex-start",
  },
  compact: {
    gap: "1px",
  },
  roleLabel: {
    fontSize: "10px",
    fontWeight: 600,
    fontFamily: '"Quicksand", sans-serif',
    textTransform: "uppercase",
    letterSpacing: "0.04em",
  },
  text: {
    margin: 0,
    lineHeight: "1.5",
    color: "#171616",
    maxWidth: "88%",
    padding: "6px 10px",
    borderRadius: "10px",
    background: "rgba(23,22,22,0.05)",
    wordBreak: "break-word",
    userSelect: "text",
  },
  thinking: {
    color: "rgba(23,22,22,0.30)",
    animation: "pulse 1.4s ease infinite",
  },
};
