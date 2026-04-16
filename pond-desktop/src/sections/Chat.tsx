import { useState, useRef, useEffect, useCallback } from "react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type { ChatEvent } from "../api/types";

interface Message {
  id: number;
  role: "user" | "agent";
  text: string;
  streaming?: boolean;
}

let msgId = 0;

export function Chat() {
  const state = useAppState();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const bottomRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const sendMessage = useCallback(async () => {
    const text = input.trim();
    if (!text || busy || !state.serverOnline) return;

    setInput("");
    setBusy(true);

    const userMsg: Message = { id: ++msgId, role: "user", text };
    const agentMsg: Message = { id: ++msgId, role: "agent", text: "", streaming: true };
    setMessages((prev) => [...prev, userMsg, agentMsg]);

    try {
      api.setToken(state.sessionToken);
      for await (const event of api.chatStream(text, undefined, state.sessionToken ?? undefined)) {
        const ev = event as ChatEvent;
        if (ev.type === "text" && ev.content) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, text: last.text + ev.content }];
          });
        }
      }
    } catch (e) {
      setMessages((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent") return prev;
        return [...prev.slice(0, -1), { ...last, text: `Error: ${String(e)}`, streaming: false }];
      });
    } finally {
      setMessages((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent") return prev;
        return [...prev.slice(0, -1), { ...last, streaming: false }];
      });
      setBusy(false);
      textareaRef.current?.focus();
    }
  }, [input, busy, state.serverOnline, state.sessionToken]);

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      sendMessage();
    }
  }

  return (
    <div style={styles.root}>
      {/* Messages */}
      <div style={styles.messages} role="log" aria-live="polite">
        {messages.length === 0 && (
          <div style={styles.empty}>
            <p style={styles.emptyTitle}>Start a conversation</p>
            <p style={styles.emptyHint}>Ask Pond anything. Type a message or use voice mode.</p>
          </div>
        )}
        {messages.map((msg) => (
          <div
            key={msg.id}
            style={{
              ...styles.bubble,
              ...(msg.role === "user" ? styles.userBubble : styles.agentBubble),
            }}
          >
            <span style={{
              ...styles.roleLabel,
              color: msg.role === "agent" ? "var(--color-accent)" : "var(--color-text-secondary)",
            }}>
              {msg.role === "user" ? "You" : "Pond"}
            </span>
            <p style={styles.bubbleText}>
              {msg.text || (msg.streaming ? <span style={styles.thinkingDots}>●●●</span> : "")}
            </p>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>

      {/* Composer */}
      <div style={styles.composer}>
        <textarea
          ref={textareaRef}
          style={styles.textarea}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Message Pond… (⌘↵ to send)"
          disabled={!state.serverOnline || busy}
          rows={1}
          aria-label="Message input"
        />
        <button
          style={{
            ...styles.sendBtn,
            opacity: (!input.trim() || !state.serverOnline || busy) ? 0.4 : 1,
          }}
          onClick={sendMessage}
          disabled={!input.trim() || !state.serverOnline || busy}
          aria-label="Send message"
        >
          ↑
        </button>
      </div>
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    display: "flex",
    flexDirection: "column",
    height: "calc(100vh - var(--toolbar-height) - var(--space-6) * 2)",
    gap: "var(--space-4)",
    maxWidth: "var(--content-max-width)",
  },
  messages: {
    flex: 1,
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-4)",
  },
  empty: {
    flex: 1,
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: "var(--space-2)",
    padding: "var(--space-12) 0",
    textAlign: "center",
  },
  emptyTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-lg)",
    color: "var(--color-text)",
    margin: 0,
  },
  emptyHint: {
    fontSize: "var(--text-sm)",
    color: "var(--color-text-tertiary)",
    margin: 0,
  },
  bubble: {
    display: "flex",
    flexDirection: "column",
    gap: "4px",
    maxWidth: "80%",
  },
  userBubble: { alignSelf: "flex-end", alignItems: "flex-end" },
  agentBubble: { alignSelf: "flex-start", alignItems: "flex-start" },
  roleLabel: {
    fontSize: "var(--text-xs)",
    fontWeight: 600,
    fontFamily: "var(--font-display)",
    textTransform: "uppercase" as const,
    letterSpacing: "0.04em",
  },
  bubbleText: {
    margin: 0,
    fontSize: "var(--text-base)",
    lineHeight: "var(--leading-base)",
    color: "var(--color-text)",
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-lg)",
    padding: "var(--space-3) var(--space-4)",
    userSelect: "text",
    wordBreak: "break-word",
  },
  thinkingDots: {
    color: "var(--color-text-tertiary)",
    animation: "pulse 1.4s ease infinite",
  },
  composer: {
    display: "flex",
    gap: "var(--space-2)",
    alignItems: "flex-end",
    background: "var(--color-bg)",
    border: "1px solid var(--color-border-strong)",
    borderRadius: "var(--radius-lg)",
    padding: "var(--space-2)",
    flexShrink: 0,
  },
  textarea: {
    flex: 1,
    border: "none",
    background: "transparent",
    resize: "none",
    fontSize: "var(--text-base)",
    fontFamily: "var(--font-body)",
    color: "var(--color-text)",
    lineHeight: "var(--leading-base)",
    outline: "none",
    padding: "var(--space-2) var(--space-3)",
    minHeight: "36px",
    maxHeight: "120px",
    overflowY: "auto",
    userSelect: "text",
  },
  sendBtn: {
    width: "36px",
    height: "36px",
    flexShrink: 0,
    background: "var(--color-accent)",
    color: "#FFFFFF",
    border: "none",
    borderRadius: "var(--radius-md)",
    fontSize: "18px",
    cursor: "pointer",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    transition: "opacity var(--transition-fast)",
  },
};
