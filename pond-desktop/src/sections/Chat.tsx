import { useState, useRef, useEffect, useCallback } from "react";
import { Button } from "@heroui/react";
import { ArrowUp } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { nextCardId } from "../state/reducer";
import type { ContextCard as ContextCardType } from "../state/reducer";
import { ContextCard } from "../components/ContextCard";
import type { ChatEvent } from "../api/types";

interface Message {
  id: number;
  role: "user" | "agent";
  text: string;
  streaming?: boolean;
  cards?: ContextCardType[];  // inline tool call results attached to this message
  modelRole?: string;         // which role answered (chat/think/task)
  tokenUsage?: { prompt_tokens: number; completion_tokens: number };
  error?: boolean;            // true when this bubble represents an error
}

let msgId = 0;

export function Chat() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const bottomRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const sessionIdRef = useRef<string | undefined>(state.sessionId ?? undefined);

  // Keep sessionIdRef in sync with state
  useEffect(() => {
    sessionIdRef.current = state.sessionId ?? undefined;
  }, [state.sessionId]);

  // Load most recent session on mount (once server is online)
  useEffect(() => {
    if (!state.serverOnline || messages.length > 0) return;
    api.listSessions()
      .then((sessions) => {
        if (sessions.length === 0) return;
        const latest = sessions[0];
        dispatch({ type: "SET_SESSION_ID", payload: latest.id });
        sessionIdRef.current = latest.id;
        return api.getSessionMessages(latest.id);
      })
      .then((msgs) => {
        if (!msgs || msgs.length === 0) return;
        setMessages(
          msgs.map((m) => ({
            id: ++msgId,
            role: m.role === "user" ? "user" : "agent",
            text: m.content,
          })),
        );
      })
      .catch((err) => {
        console.warn("Could not load session history (non-fatal):", err);
      });
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.serverOnline]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  function newConversation() {
    setMessages([]);
    sessionIdRef.current = undefined;
    dispatch({ type: "SET_SESSION_ID", payload: null });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
    textareaRef.current?.focus();
  }

  const sendMessage = useCallback(async () => {
    const text = input.trim();
    if (!text || busy || !state.serverOnline) return;

    setInput("");
    // Reset textarea height
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }
    setBusy(true);

    const userMsg: Message = { id: ++msgId, role: "user", text };
    const agentMsg: Message = { id: ++msgId, role: "agent", text: "", streaming: true };
    setMessages((prev) => [...prev, userMsg, agentMsg]);

    try {
      api.setToken(state.sessionToken);
      for await (const event of api.chatStream(text, sessionIdRef.current, state.sessionToken ?? undefined)) {
        const ev = event as ChatEvent;

        if (ev.type === "text" && (ev.content ?? ev.token)) {
          const tok = ev.content ?? ev.token ?? "";
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, text: last.text + tok }];
          });

        } else if (ev.type === "tool_call" && ev.tool) {
          const card: ContextCardType = {
            id: nextCardId(),
            tool: ev.tool,
            data: (ev.result as Record<string, unknown>) ?? {},
            timestamp_ms: Date.now(),
          };
          dispatch({ type: "PUSH_CONTEXT_CARD", payload: card }); // keep for voice compat
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, cards: [...(last.cards ?? []), card] }];
          });

        } else if (ev.type === "error" || ev.error) {
          const errMsg = ev.error ?? "Unknown error from agent";
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, text: `Error: ${errMsg}`, streaming: false, error: true }];
          });

        } else if (ev.done && ev.session_id) {
          sessionIdRef.current = ev.session_id;
          dispatch({ type: "SET_SESSION_ID", payload: ev.session_id });
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), {
              ...last,
              ...(ev.model_role ? { modelRole: ev.model_role } : {}),
              ...(ev.usage && ev.usage.completion_tokens > 0 ? { tokenUsage: ev.usage } : {}),
            }];
          });
        }
      }
    } catch (e) {
      setMessages((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent") return prev;
        return [...prev.slice(0, -1), { ...last, text: `Error: ${String(e)}`, streaming: false, error: true }];
      });
    } finally {
      setMessages((prev) => {
        const last = prev[prev.length - 1];
        if (!last || last.role !== "agent") return prev;
        // Only clear streaming flag if not already cleared by error handler
        if (!last.streaming) return prev;
        return [...prev.slice(0, -1), { ...last, streaming: false }];
      });
      setBusy(false);
      textareaRef.current?.focus();
    }
  }, [input, busy, state.serverOnline, state.sessionToken, dispatch]);

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      sendMessage();
    }
  }

  function onInput(e: React.ChangeEvent<HTMLTextAreaElement>) {
    setInput(e.target.value);
    const el = e.target;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 120)}px`;
  }

  return (
    <div style={styles.root}>
      {/* Header */}
      <div style={styles.header}>
        <span style={styles.headerTitle}>Chat</span>
        <Button
          variant="ghost"
          size="sm"
          onPress={newConversation}
          aria-label="New conversation"
        >
          New chat
        </Button>
      </div>

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
            <p style={{
              ...styles.bubbleText,
              ...(msg.error ? styles.bubbleError : {}),
            }}>
              {msg.text || (msg.streaming ? <span style={styles.thinkingDots}>●●●</span> : "")}
            </p>

            {/* Inline tool call result cards */}
            {msg.role === "agent" && (msg.cards?.length ?? 0) > 0 && (
              <div style={styles.cardList}>
                {msg.cards!.map((card) => (
                  <ContextCard key={card.id} card={card} />
                ))}
              </div>
            )}

            {/* Model role badge + token count */}
            {msg.role === "agent" && msg.modelRole && !msg.streaming && (
              <span style={styles.modelRoleBadge}>
                {msg.modelRole}
                {msg.tokenUsage && msg.tokenUsage.completion_tokens > 0 && (
                  <> · {msg.tokenUsage.completion_tokens} tokens</>
                )}
              </span>
            )}
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
          onChange={onInput}
          onKeyDown={onKeyDown}
          placeholder="Message Pond… (⌘↵ to send)"
          disabled={!state.serverOnline || busy}
          aria-label="Message input"
        />
        <Button
          variant="primary"
          isDisabled={!input.trim() || !state.serverOnline || busy}
          onPress={sendMessage}
          aria-label="Send message"
        >
          <ArrowUp size={16} />
        </Button>
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
  header: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    flexShrink: 0,
  },
  headerTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
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
  bubbleError: {
    borderColor: "var(--color-destructive)",
    color: "var(--color-destructive)",
  },
  thinkingDots: {
    color: "var(--color-text-tertiary)",
    animation: "pulse 1.4s ease infinite",
  },
  cardList: {
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-2)",
    maxWidth: "360px",
    width: "100%",
  },
  modelRoleBadge: {
    fontSize: "10px",
    color: "var(--color-text-tertiary)",
    fontFamily: "var(--font-mono)",
    paddingTop: "2px",
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
};
