import { useState, useRef, useEffect, useCallback } from "react";
import { Button } from "@heroui/react";
import { ArrowUp } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { ThinkingPlaceholder } from "../components/ThinkingPlaceholder";
import type { ChatEvent } from "../api/types";
import { filterThinking } from "../lib/thinkFilter";

// Map raw tool names (e.g. "giap__get_current_weather") to a one-line,
// user-friendly status the chat bubble shows while the tool is running.
// Falls back to a humanised version of the bare tool name so unknown tools
// still render something readable instead of "giap__do_thing_v2".
function friendlyToolStatus(rawName: string): string {
    const bare = rawName.includes("__") ? rawName.split("__").pop()! : rawName;
    const map: Record<string, string> = {
        get_current_weather:      "Checking the weather…",
        list_registered_devices:  "Looking up your devices…",
        recall_memories:          "Recalling what I know…",
        save_memory:              "Saving that for later…",
        list_schedules:           "Looking up your schedules…",
        get_recipe:               "Finding that recipe…",
        get_user_profile:         "Looking up your profile…",
        list_skills:              "Checking my skills…",
    };
    if (map[bare]) return map[bare];
    // Generic fallback: turn snake_case into "Title Case" preceded by "Working on".
    const pretty = bare
        .replace(/_/g, " ")
        .replace(/\b\w/g, (c) => c.toUpperCase());
    return `Working on: ${pretty}…`;
}

interface Message {
  id: number;
  role: "user" | "agent";
  text: string;
  streaming?: boolean;
  status?: string;       // current activity description (e.g. "Thinking...", "Using tool...")
  // (cards removed — see tool_call handler below)
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
  const inThinkBlockRef = useRef(false);

  // Keep sessionIdRef in sync with state. When the session id changes
  // *externally* (e.g. the user clicked a Recent item on the Dashboard),
  // fetch that session's messages and replace the bubble list. The
  // mount-time loader below covers the "open Chat for the first time"
  // case; this effect covers every subsequent navigate-to-this-chat.
  useEffect(() => {
    const newId = state.sessionId ?? undefined;
    if (newId === sessionIdRef.current) return;
    const wasExternal = !!newId;
    sessionIdRef.current = newId;
    if (!wasExternal) return;
    api.getSessionMessages(newId!)
      .then((msgs) => {
        setMessages(
          (msgs ?? []).map((m) => ({
            id: ++msgId,
            role: m.role === "user" ? "user" : "agent",
            text: m.content,
          })),
        );
      })
      .catch((err) => {
        console.warn("Could not load session history (non-fatal):", err);
      });
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
          const raw = ev.content ?? ev.token ?? "";
          const [visible, newInBlock] = filterThinking(raw, inThinkBlockRef.current);
          inThinkBlockRef.current = newInBlock;
          if (visible) {
            setMessages((prev) => {
              const last = prev[prev.length - 1];
              if (!last || last.role !== "agent") return prev;
              return [...prev.slice(0, -1), { ...last, text: last.text + visible, status: undefined }];
            });
          }

        } else if (ev.type === "status" && ev.content) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, status: ev.content }];
          });

        } else if (ev.type === "tool_call" && ev.tool) {
          // Surface the tool invocation as a discreet inline status only.
          // We deliberately do NOT push the ContextCard into global state
          // either — anything that reads `state.contextCards` (the voice
          // overlay, the transcript feed) would otherwise re-render the
          // raw "Get Weather / Get User Profile" chip the user explicitly
          // asked us to remove. The reply text still streams through as a
          // `text` event, so the user sees the answer.
          const friendly = friendlyToolStatus(ev.tool);
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, status: friendly }];
          });

        } else if (ev.type === "tool_result") {
          // Tool finished — clear the inline status. Result text arrives
          // separately as `text` events from the model's follow-up reply,
          // so we don't need to render the raw payload here either.
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, status: undefined }];
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
          if (ev.model_name && ev.model_role) {
            dispatch({
              type: "SET_LAST_RESPONSE_META",
              payload: {
                modelName: ev.model_name,
                modelRole: ev.model_role,
                completionTokens: ev.usage?.completion_tokens ?? 0,
              },
            });
          }
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
              {msg.text || (msg.streaming ? (
                <ThinkingPlaceholder status={msg.status} />
              ) : "")}
            </p>

            {/* Inline tool-call ContextCards intentionally NOT rendered in
             * the chat thread. They were leaking the agent's plumbing
             * (raw "Get Recipe" / "Get Weather" chips with `{}` JSON
             * underneath) every time the model invoked a tool. The
             * canvas overlay still receives the same cards via the
             * PUSH_CONTEXT_CARD action above, so voice mode is unaffected. */}

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
