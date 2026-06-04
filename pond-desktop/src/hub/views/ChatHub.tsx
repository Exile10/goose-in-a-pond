import { useState, useRef, useEffect, useCallback } from "react";
import { api } from "../../api/PondApiClient";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { filterThinking } from "../../lib/thinkFilter";
import type { ChatEvent } from "../../api/types";
import { HubIco, micEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { GooseAvatar } from "./chat/GooseAvatar";
import { TypingIndicator } from "./chat/TypingIndicator";
import { ResultCard } from "./chat/ResultCard";
import type { CardKind } from "./chat/ResultCard";
import "./chat.css";

// ── Types ─────────────────────────────────────────────────────

interface ChatMessage {
  id: string;
  who: "user" | "goose";
  text: string;
  card?: CardKind;
  streaming?: boolean;
}

// ── Constants ─────────────────────────────────────────────────

const SEED: ChatMessage[] = [
  {
    id: "seed-0",
    who: "goose",
    text: "Morning, Jerry. The house is set to Good Morning — lights are easing up and coffee's brewing. Anything you need?",
  },
  { id: "seed-1", who: "user", text: "What’s the weather looking like?" },
  {
    id: "seed-2",
    who: "goose",
    text: "Partly cloudy and mild today — here’s your day:",
    card: "weather",
  },
  { id: "seed-3", who: "user", text: "Is the front door locked?" },
  {
    id: "seed-4",
    who: "goose",
    text: "Yes — the front door is locked. Tap to toggle it from here:",
    card: "lock",
  },
];

const CHIPS = [
  "Set Movie Time",
  "Lock everything",
  "Bedroom to 67°",
  "Show the driveway",
  "New sticky note",
];

// Simple incrementing ID for messages
let _msgId = 0;
function nextMsgId() {
  return String(++_msgId);
}

// Resolve tool call to an inline card kind
function toolToCard(toolName: string): CardKind | undefined {
  if (!toolName) return undefined;
  const lower = toolName.toLowerCase();
  if (lower.includes("weather")) return "weather";
  if (lower.includes("device") || lower.includes("lock")) return "lock";
  if (lower.includes("routine") || lower.includes("recipe") || lower.includes("scene")) return "movie";
  return undefined;
}

// ── Component ─────────────────────────────────────────────────

export function ChatHubView() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  // Use seed as initial messages; cleared when user sends first real message
  const [msgs, setMsgs] = useState<ChatMessage[]>(SEED);
  const [seeded, setSeeded] = useState(true); // true = currently showing seed
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const sessionIdRef = useRef<string | undefined>(
    state.sessionId ?? undefined,
  );
  const inThinkBlockRef = useRef(false);

  // Keep sessionIdRef in sync with global state
  useEffect(() => {
    if (state.sessionId) sessionIdRef.current = state.sessionId;
  }, [state.sessionId]);

  // Auto-scroll to bottom on new messages
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [msgs, busy]);

  const sendMessage = useCallback(
    async (raw?: string) => {
      const t = (raw ?? text).trim();
      if (!t || busy) return;

      // Clear seed on first real send
      if (seeded) {
        setSeeded(false);
      }

      setText("");
      setBusy(true);
      inThinkBlockRef.current = false;

      const userMsg: ChatMessage = { id: nextMsgId(), who: "user", text: t };
      const agentMsg: ChatMessage = {
        id: nextMsgId(),
        who: "goose",
        text: "",
        streaming: true,
      };

      setMsgs((prev) => {
        const base = seeded ? [] : prev;
        return [...base, userMsg, agentMsg];
      });

      try {
        api.setToken(state.sessionToken);
        for await (const event of api.chatStream(
          t,
          sessionIdRef.current,
          state.sessionToken ?? undefined,
        )) {
          const ev = event as ChatEvent;

          if (ev.type === "text" && (ev.content ?? ev.token)) {
            const raw = ev.content ?? ev.token ?? "";
            const [visible, newInBlock] = filterThinking(
              raw,
              inThinkBlockRef.current,
            );
            inThinkBlockRef.current = newInBlock;
            if (visible) {
              setMsgs((prev) => {
                const last = prev[prev.length - 1];
                if (!last || last.who !== "goose") return prev;
                return [
                  ...prev.slice(0, -1),
                  { ...last, text: last.text + visible },
                ];
              });
            }
          } else if (ev.type === "tool_call" && ev.tool) {
            const card = toolToCard(ev.tool);
            if (card) {
              setMsgs((prev) => {
                const last = prev[prev.length - 1];
                if (!last || last.who !== "goose") return prev;
                return [
                  ...prev.slice(0, -1),
                  { ...last, card },
                ];
              });
            }
          } else if (ev.type === "error" || ev.error) {
            const errMsg = ev.error ?? "Something went wrong";
            setMsgs((prev) => {
              const last = prev[prev.length - 1];
              if (!last || last.who !== "goose") return prev;
              return [
                ...prev.slice(0, -1),
                {
                  ...last,
                  text: `Error: ${errMsg}`,
                  streaming: false,
                },
              ];
            });
          } else if (ev.done && ev.session_id) {
            sessionIdRef.current = ev.session_id;
            dispatch({ type: "SET_SESSION_ID", payload: ev.session_id });
          }
        }
      } catch (e) {
        setMsgs((prev) => {
          const last = prev[prev.length - 1];
          if (!last || last.who !== "goose") return prev;
          return [
            ...prev.slice(0, -1),
            {
              ...last,
              text: `Error: ${String(e)}`,
              streaming: false,
            },
          ];
        });
      } finally {
        setMsgs((prev) => {
          const last = prev[prev.length - 1];
          if (!last || last.who !== "goose") return prev;
          return [...prev.slice(0, -1), { ...last, streaming: false }];
        });
        setBusy(false);
        inputRef.current?.focus();
      }
    },
    [text, busy, seeded, state.sessionToken], // state.sessionId intentionally via ref
  );

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  function goToVoice() {
    dispatch({ type: "SET_MODE", payload: "voice" });
  }

  const canSend = text.trim().length > 0 && !busy;

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
              On-device &middot; listening
            </div>
          </div>
        </div>
        <button
          className="chat2__voice-btn"
          onClick={goToVoice}
          aria-label="Switch to voice mode"
          title="Voice mode"
        >
          <HubIco d={micEl} size={20} color="#7C3AED" />
        </button>
      </header>

      {/* Thread */}
      <div
        className="chat2__thread"
        ref={scrollRef}
        role="log"
        aria-label="Chat conversation"
        aria-live="polite"
      >
        {msgs.map((m) => (
          <div key={m.id} className={`ch-row ch-row--${m.who}`}>
            {m.who === "goose" && <GooseAvatar />}
            <div className="ch-bubble-wrap">
              <div className={`ch-bubble ch-bubble--${m.who}`}>
                {m.text || (m.streaming ? " " : "")}
              </div>
              {m.card && !m.streaming && (
                <div className="ch-card">
                  <ResultCard kind={m.card} />
                </div>
              )}
            </div>
          </div>
        ))}
        {busy && msgs[msgs.length - 1]?.text === "" && (
          <TypingIndicator />
        )}
      </div>

      {/* Suggestion chips */}
      <div
        className="chat2__chips"
        role="group"
        aria-label="Quick suggestions"
      >
        {CHIPS.map((c) => (
          <button
            key={c}
            className="ch-chip"
            onClick={() => sendMessage(c)}
            disabled={busy}
            type="button"
          >
            {c}
          </button>
        ))}
      </div>

      {/* Input */}
      <div className="chat2__input">
        <button
          className="ch-mic"
          onClick={goToVoice}
          aria-label="Switch to voice mode"
          title="Voice input"
          type="button"
        >
          <HubIco d={micEl} size={19} color="#fff" />
        </button>
        <input
          ref={inputRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Message Goose or speak a command…"
          disabled={busy}
          aria-label="Message input"
          autoComplete="off"
          spellCheck={false}
        />
        <button
          className="ch-send"
          onClick={() => sendMessage()}
          disabled={!canSend}
          aria-label="Send message"
          type="button"
        >
          <HubIco d={HP_PATHS.chevR} size={18} color="#fff" sw={2.5} />
        </button>
      </div>
    </div>
  );
}
