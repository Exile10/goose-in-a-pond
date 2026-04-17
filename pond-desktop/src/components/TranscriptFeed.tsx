import { useEffect, useRef } from "react";
import { Chip } from "@heroui/react";
import type { ContextCard, TranscriptMessage } from "../state/reducer";
import { ContextCard as ContextCardView } from "./ContextCard";

interface Props {
  messages: TranscriptMessage[];
  /** Context cards to render inline after the agent message they correspond to */
  contextCards?: ContextCard[];
  /** Legacy fixed max-height (ignored when fillHeight is true) */
  maxHeight?: string;
  /** Fill the parent flex container vertically (Voice Mode) */
  fillHeight?: boolean;
  compact?: boolean;
}

export function TranscriptFeed({
  messages,
  contextCards = [],
  maxHeight = "200px",
  fillHeight = false,
  compact = false,
}: Props) {
  const bottomRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when new messages arrive
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, contextCards]);

  const rootStyle: React.CSSProperties = fillHeight
    ? { ...styles.root, flex: 1, maxHeight: "none" }
    : { ...styles.root, maxHeight };

  if (messages.length === 0) {
    return (
      <div style={rootStyle}>
        <p style={styles.empty}>
          {compact ? "Say something…" : "No messages yet. Start listening to begin."}
        </p>
      </div>
    );
  }

  return (
    <div style={rootStyle} role="log" aria-live="polite" aria-label="Conversation">
      {messages.map((msg, idx) => {
        // Cards whose timestamp falls after this message and before the next
        const nextMsg = messages[idx + 1];
        const inlineCards = msg.role === "agent"
          ? contextCards.filter((c) => {
              const afterThis  = c.timestamp_ms >= msg.timestamp;
              const beforeNext = !nextMsg || c.timestamp_ms < nextMsg.timestamp;
              return afterThis && beforeNext;
            })
          : [];

        return (
          <div key={msg.id}>
            <div
              style={{
                ...styles.message,
                ...(msg.role === "user" ? styles.userMsg : styles.agentMsg),
                ...(compact ? styles.compact : {}),
              }}
            >
              {!compact && (
                <Chip
                  size="sm"
                  variant={msg.role === "user" ? "soft" : "primary"}
                >
                  {msg.role === "user" ? "You" : "Pond"}
                </Chip>
              )}
              <p style={{ ...styles.text, fontSize: compact ? "12px" : "13px" }}>
                {msg.text || (msg.role === "agent" ? <span style={styles.thinking}>●●●</span> : "")}
              </p>
            </div>

            {/* Inline context cards after agent messages */}
            {inlineCards.length > 0 && (
              <div style={styles.cardsRow}>
                {inlineCards.map((card) => (
                  <div key={card.id} style={styles.cardWrapper}>
                    <ContextCardView card={card} />
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
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
  cardsRow: {
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    paddingLeft: "4px",
    marginTop: "4px",
    marginBottom: "4px",
  },
  cardWrapper: {
    maxWidth: "340px",
  },
};
