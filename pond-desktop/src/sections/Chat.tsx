import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import { Brain, Check, ChevronDown, Cpu, History, Loader2, PenSquare, Wrench } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { nextCardId } from "../state/reducer";
import type { ContextCard as ContextCardType } from "../state/reducer";
import { ToolCallChip } from "../components/ToolCallChip";
import { SessionDropdown } from "../components/SessionDropdown";
import { ThinkingPlaceholder } from "../components/ThinkingPlaceholder";
import { GooseAvatar } from "../hub/views/chat/GooseAvatar";
import { TypingIndicator } from "../hub/views/chat/TypingIndicator";
import { HubIco, micEl } from "../hub/primitives/HubIco";
import { HP_PATHS } from "../hub/primitives/icons";
import type { ChatEvent, ModelEntry, SessionMessage, SessionSummary } from "../api/types";
import { filterThinking } from "../lib/thinkFilter";

// Module-level counter — shared across session loads and live sends
let _msgId = 0;

const CHIPS = [
  "What can you help me with?",
  "Check the weather",
  "Set a schedule",
  "Show my devices",
  "Manage my models",
];

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
  return `Working on: ${bare.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase())}…`;
}

interface Message {
  id: number;
  role: "user" | "agent";
  text: string;
  streaming?: boolean;
  status?: string;
  cards?: ContextCardType[];
  thinkingBlocks?: string[];
  modelRole?: string;
  tokenUsage?: { prompt_tokens: number; completion_tokens: number };
  error?: boolean;
  historyToolNames?: string[];
}

function sessionMessagesToMessages(raw: SessionMessage[]): Message[] {
  const out: Message[] = [];
  for (const m of raw) {
    if (m.role === "tool") continue;
    if (m.role === "assistant") {
      const hasContent = m.content.trim().length > 0;
      const hasToolCalls = (m.tool_calls?.length ?? 0) > 0;
      if (!hasContent && hasToolCalls) continue;
      const historyToolNames = hasToolCalls
        ? m.tool_calls!.map((tc) => {
            const bare = tc.name.includes("__") ? tc.name.split("__").pop()! : tc.name;
            return bare;
          })
        : undefined;
      out.push({ id: ++_msgId, role: "agent", text: m.content, historyToolNames });
    } else {
      out.push({ id: ++_msgId, role: "user", text: m.content });
    }
  }
  return out;
}

export function Chat() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  const [messages, setMessages]             = useState<Message[]>([]);
  const [input, setInput]                   = useState("");
  const [busy, setBusy]                     = useState(false);
  const [loadingSession, setLoadingSession] = useState(false);
  const [sessions, setSessions]             = useState<SessionSummary[]>([]);
  const [showSessions, setShowSessions]     = useState(false);
  const [showModelSelector, setShowModelSelector] = useState(false);
  const [availableModels, setAvailableModels]     = useState<ModelEntry[]>([]);
  const [modelSwitching, setModelSwitching]       = useState(false);

  const bottomRef        = useRef<HTMLDivElement>(null);
  const textareaRef      = useRef<HTMLTextAreaElement>(null);
  const modelSelectorRef = useRef<HTMLDivElement>(null);
  const sessionIdRef     = useRef<string | undefined>(state.sessionId ?? undefined);
  const inThinkBlockRef  = useRef(false);

  // Sync session ref; load history when session changes externally
  useEffect(() => {
    const newId = state.sessionId ?? undefined;
    if (newId === sessionIdRef.current) return;
    const wasExternal = !!newId;
    sessionIdRef.current = newId;
    if (!wasExternal) return;
    api.getSessionMessages(newId!)
      .then((msgs) => setMessages(sessionMessagesToMessages(msgs ?? [])))
      .catch((err) => console.warn("Could not load session history (non-fatal):", err));
  }, [state.sessionId]);

  const refreshSessions = useCallback(() => {
    api.listSessions().then(setSessions).catch(() => {});
  }, []);

  const openModelSelector = useCallback(() => {
    setShowModelSelector(true);
    Promise.all([api.listModels(), api.listOllamaModels()])
      .then(([localModels, { models: ollamaModels }]) => {
        const ollamaEntries: ModelEntry[] = (ollamaModels ?? []).map((m) => {
          const sizeMb = m.size ? Math.round(m.size / (1024 * 1024)) : undefined;
          return {
            id: `ollama/${m.name}`,
            provider: "ollama",
            name: m.name,
            display_name: m.name,
            is_active: false,
            ram_estimate_mb: sizeMb,
            size_mb: sizeMb,
            category: "ollama",
            downloaded: true,
          };
        });
        setAvailableModels([...localModels, ...ollamaEntries]);
      })
      .catch(() => {
        api.listModels().then(setAvailableModels).catch(() => {});
      });
  }, []);

  const chatModels = useMemo(() => {
    return availableModels.filter((m) => {
      if (m.downloaded === false) return false;
      const cat = (m.category ?? m.provider ?? "").toLowerCase();
      if (cat === "whisper" || cat.startsWith("tts")) return false;
      return true;
    });
  }, [availableModels]);

  const groupedModels = useMemo(() => {
    const map = new Map<string, ModelEntry[]>();
    for (const m of chatModels) {
      const group = map.get(m.provider) ?? [];
      group.push(m);
      map.set(m.provider, group);
    }
    return Array.from(map.entries());
  }, [chatModels]);

  const handleModelSwitch = useCallback(async (provider: string, name: string) => {
    setModelSwitching(true);
    try {
      await api.activateModel(provider, name, "chat");
      dispatch({ type: "SET_LAST_RESPONSE_META", payload: { modelName: name, modelRole: "chat", completionTokens: 0 } });
    } catch (e) {
      console.warn("Model switch failed:", e);
    } finally {
      setModelSwitching(false);
      setShowModelSelector(false);
    }
  }, [dispatch]);

  // Close model selector on outside click / Escape
  useEffect(() => {
    if (!showModelSelector) return;
    function handleClick(e: MouseEvent) {
      if (modelSelectorRef.current && !modelSelectorRef.current.contains(e.target as Node)) {
        setShowModelSelector(false);
      }
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") setShowModelSelector(false);
    }
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [showModelSelector]);

  // Load most recent session on mount
  useEffect(() => {
    if (!state.serverOnline || messages.length > 0) return;
    setLoadingSession(true);
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
        setMessages(sessionMessagesToMessages(msgs));
      })
      .catch((err) => console.warn("Could not load session history (non-fatal):", err))
      .finally(() => { setLoadingSession(false); refreshSessions(); });
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

  const sendMessage = useCallback(async (directText?: string) => {
    const text = (directText ?? input).trim();
    if (!text || busy || !state.serverOnline) return;

    setInput("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    setBusy(true);
    inThinkBlockRef.current = false;

    const userMsg:  Message = { id: ++_msgId, role: "user",  text };
    const agentMsg: Message = { id: ++_msgId, role: "agent", text: "", streaming: true };
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
        } else if (ev.type === "thinking" && ev.content) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, thinkingBlocks: [...(last.thinkingBlocks ?? []), ev.content as string] }];
          });
        } else if (ev.type === "status" && ev.content) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, status: ev.content }];
          });
        } else if (ev.type === "tool_call" && ev.tool) {
          const card: ContextCardType = {
            id: nextCardId(),
            tool: ev.tool,
            callId: ev.id as string | undefined,
            data: (ev.result as Record<string, unknown>) ?? {},
            timestamp_ms: Date.now(),
          };
          dispatch({ type: "PUSH_CONTEXT_CARD", payload: card });
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, cards: [...(last.cards ?? []), card], status: friendlyToolStatus(ev.tool ?? "") }];
          });
        } else if (ev.type === "tool_result" && ev.id) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent" || !last.cards) return prev;
            const cardData   = ev.ui?.data ?? { result: ev.content };
            const renderHint = ev.ui?.card_type;
            const evId   = ev.id as string;
            const evTool = ev.tool as string | undefined;
            const newCards = last.cards.map((c) =>
              (c.callId && c.callId === evId) || (evTool && c.tool === evTool)
                ? { ...c, data: cardData, ...(renderHint ? { renderHint } : {}) }
                : c,
            );
            return [...prev.slice(0, -1), { ...last, cards: newCards, status: undefined }];
          });
        } else if (ev.type === "review_status" && ev.content) {
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, status: ev.content }];
          });
        } else if ((ev.type === "review_revision" || ev.type === "tool_revision") && ev.content) {
          inThinkBlockRef.current = false;
          setMessages((prev) => {
            const last = prev[prev.length - 1];
            if (!last || last.role !== "agent") return prev;
            return [...prev.slice(0, -1), { ...last, text: ev.content!, status: undefined }];
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
            dispatch({ type: "SET_LAST_RESPONSE_META", payload: { modelName: ev.model_name, modelRole: ev.model_role, completionTokens: ev.usage?.completion_tokens ?? 0 } });
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
        if (!last || last.role !== "agent" || !last.streaming) return prev;
        return [...prev.slice(0, -1), { ...last, streaming: false }];
      });
      setBusy(false);
      textareaRef.current?.focus();
      refreshSessions();
    }
  }, [input, busy, state.serverOnline, state.sessionToken, dispatch, refreshSessions]);

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

  const modelLabel = state.lastResponseMeta?.modelName ?? "local model";

  return (
    <div className="chat2">
      {/* Header */}
      <header className="chat2__head">
        <div className="chat2__id">
          <GooseAvatar size={40} />
          <div>
            <div className="chat2__name">Goose</div>
            <div className="chat2__status">
              <span className="chat2__dot" aria-hidden="true" />
              {state.serverOnline ? "On-device · listening" : "Offline"}
            </div>
          </div>
        </div>
        <div className="chat2__head-right">
          <button
            className="chat2__voice-btn"
            onClick={() => { refreshSessions(); setShowSessions(!showSessions); }}
            aria-label="Session history"
            title="Session history"
            type="button"
          >
            <History size={18} color="var(--pp)" />
          </button>
          <button
            className="chat2__voice-btn"
            onClick={newConversation}
            aria-label="New conversation"
            title="New conversation"
            type="button"
          >
            <PenSquare size={18} color="var(--pp)" />
          </button>
          <button
            className="chat2__voice-btn"
            onClick={() => dispatch({ type: "SET_MODE", payload: "voice" })}
            aria-label="Switch to voice mode"
            title="Voice mode"
            type="button"
          >
            <HubIco d={micEl} size={20} color="var(--pp)" />
          </button>
          <SessionDropdown
            sessions={sessions}
            currentSessionId={sessionIdRef.current ?? null}
            onSelect={(id) => dispatch({ type: "SET_SESSION_ID", payload: id })}
            onNewChat={newConversation}
            isOpen={showSessions}
            onClose={() => setShowSessions(false)}
          />
        </div>
      </header>

      {/* Thread */}
      <div className="chat2__thread" role="log" aria-live="polite" aria-label="Chat conversation">
        {loadingSession && (
          <div className="chat-skeleton" aria-busy="true" aria-label="Loading conversation">
            {([88, 64, 72] as const).map((w, i) => (
              <div key={i} className={`chat-skeleton__row${i % 2 !== 0 ? " chat-skeleton__row--right" : ""}`}>
                <div className="chat-skeleton__line" style={{ width: `${w}%` }} />
                <div className="chat-skeleton__line" style={{ width: `${Math.round(w * 0.65)}%` }} />
              </div>
            ))}
          </div>
        )}

        {!loadingSession && messages.length === 0 && (
          <div className="chat-empty">
            <GooseAvatar size={52} />
            <p className="chat-empty__title">Start a conversation</p>
            <p className="chat-empty__hint">Ask Goose anything or pick a suggestion below.</p>
          </div>
        )}

        {messages.map((msg) => {
          const hasText     = msg.text && msg.text.trim().length > 0;
          const hasCards    = (msg.cards?.length ?? 0) > 0;
          const hasThinking = (msg.thinkingBlocks?.length ?? 0) > 0;
          if (msg.role === "agent" && !hasText && !hasCards && !hasThinking && !msg.streaming) return null;
          return (
            <div key={msg.id} className={`ch-row ${msg.role === "user" ? "ch-row--user" : "ch-row--goose"}`}>
              {msg.role === "agent" && <GooseAvatar />}
              <div className="ch-bubble-wrap">
                {/* Tool call chips */}
                {msg.role === "agent" && msg.cards && msg.cards.length > 0 && !msg.streaming && (
                  <div className="tool-call-chips" role="list" aria-label="Tools used">
                    {msg.cards.map((card) => <ToolCallChip key={card.id} card={card} />)}
                  </div>
                )}
                {/* History tool indicators */}
                {msg.role === "agent" && msg.historyToolNames && msg.historyToolNames.length > 0 && (
                  <div className="tool-call-chips" role="list" aria-label="Tools used">
                    {msg.historyToolNames.map((name) => (
                      <span key={name} className="tool-history-chip">
                        <Wrench size={10} aria-hidden />
                        {name.replace(/_/g, " ")}
                      </span>
                    ))}
                  </div>
                )}
                {/* Thinking block */}
                {msg.role === "agent" && msg.thinkingBlocks && msg.thinkingBlocks.length > 0 && !msg.streaming && (
                  <details className="thinking-block">
                    <summary className="thinking-block__toggle">
                      <Brain size={12} aria-hidden /> Thinking
                    </summary>
                    <div className="thinking-block__content">
                      {msg.thinkingBlocks.map((block, i) => <p key={i}>{block}</p>)}
                    </div>
                  </details>
                )}
                {/* Bubble */}
                <div className={`ch-bubble ${msg.role === "user" ? "ch-bubble--user" : `ch-bubble--goose${msg.error ? " ch-bubble--error" : ""}`}`}>
                  {msg.text || (msg.streaming
                    ? msg.status
                      ? <ThinkingPlaceholder status={msg.status} />
                      : <span className="stream-dots"><span /><span /><span /></span>
                    : "")}
                </div>
                {/* Model role + token meta */}
                {msg.role === "agent" && msg.modelRole && !msg.streaming && (
                  <span className="bubble__meta">
                    {msg.modelRole}
                    {msg.tokenUsage && msg.tokenUsage.completion_tokens > 0 && (
                      <> · {msg.tokenUsage.completion_tokens} tokens</>
                    )}
                  </span>
                )}
              </div>
            </div>
          );
        })}

        {busy && messages[messages.length - 1]?.text === "" && <TypingIndicator />}
        <div ref={bottomRef} />
      </div>

      {/* Suggestion chips — only when thread is empty */}
      {!loadingSession && messages.length === 0 && (
        <div className="chat2__chips" role="group" aria-label="Quick suggestions">
          {CHIPS.map((c) => (
            <button
              key={c}
              className="ch-chip"
              onClick={() => sendMessage(c)}
              disabled={busy || !state.serverOnline}
              type="button"
            >
              {c}
            </button>
          ))}
        </div>
      )}

      {/* Input row */}
      <div className="chat2__input">
        <button
          className="ch-mic"
          onClick={() => dispatch({ type: "SET_MODE", payload: "voice" })}
          aria-label="Switch to voice mode"
          title="Voice input"
          type="button"
        >
          <HubIco d={micEl} size={19} color="#fff" />
        </button>
        <textarea
          ref={textareaRef}
          className="chat2__textarea"
          value={input}
          onChange={onInput}
          onKeyDown={onKeyDown}
          placeholder="Message Goose…"
          disabled={!state.serverOnline || busy}
          aria-label="Message input"
          rows={1}
        />
        <button
          className="ch-send"
          onClick={() => sendMessage()}
          disabled={!input.trim() || !state.serverOnline || busy}
          aria-label="Send message"
          type="button"
        >
          <HubIco d={HP_PATHS.chevR} size={18} color="#fff" sw={2.5} />
        </button>
      </div>

      {/* Hint bar — model selector + keyboard shortcut */}
      <div className="chat2__hint">
        <div ref={modelSelectorRef} className="model-selector-wrap">
          <button
            className={`model-selector-trigger${showModelSelector ? " is-open" : ""}`}
            onClick={() => showModelSelector ? setShowModelSelector(false) : openModelSelector()}
            disabled={modelSwitching}
            aria-label="Select model"
            aria-expanded={showModelSelector}
          >
            <Cpu size={11} />
            <span className="model-selector-trigger__label">
              {modelSwitching ? "Switching…" : modelLabel}
            </span>
            {modelSwitching ? <Loader2 size={10} className="spin" /> : <ChevronDown size={10} />}
          </button>

          {showModelSelector && (
            <div className="model-selector-dropdown">
              <div className="model-selector-dropdown__header">
                <span>Switch Model</span>
              </div>
              <div className="model-selector-dropdown__list">
                {groupedModels.length === 0 && (
                  <div className="model-selector-dropdown__empty">No models available</div>
                )}
                {groupedModels.map(([provider, group]) => (
                  <div key={provider}>
                    <div className="model-selector-dropdown__group-label">
                      {provider.charAt(0).toUpperCase() + provider.slice(1)}
                    </div>
                    {group.map((m) => {
                      const isActive = modelLabel === m.name || modelLabel === (m.display_name ?? m.name);
                      return (
                        <button
                          key={m.id}
                          className={`model-selector-dropdown__item${isActive ? " is-active" : ""}`}
                          onClick={() => handleModelSwitch(m.provider, m.name)}
                          disabled={modelSwitching}
                        >
                          <span className="model-selector-dropdown__item-name">{m.name}</span>
                          <span className="model-selector-dropdown__item-meta">
                            {m.size_mb
                              ? m.size_mb >= 1024
                                ? `${(m.size_mb / 1024).toFixed(1)} GB`
                                : `${m.size_mb} MB`
                              : ""}
                            {isActive && <Check size={12} color="var(--color-accent)" />}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        <span>·</span>
        <span>Cmd + Enter to send</span>
      </div>
    </div>
  );
}
