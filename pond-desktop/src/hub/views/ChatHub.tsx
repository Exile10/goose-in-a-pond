import { useState, useRef, useEffect, useCallback } from "react";
import { Paperclip } from "lucide-react";
import { api } from "../../api/PondApiClient";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { filterThinking } from "../../lib/thinkFilter";
import { CONTINUE_TURN_MESSAGE } from "../../api/types";
import type { ChatEvent, ContextWarning, ImageAttachment, TurnStats } from "../../api/types";
import { HubIco, micEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import { GooseAvatar } from "./chat/GooseAvatar";
import { TypingIndicator } from "./chat/TypingIndicator";
import { ResultCard } from "./chat/ResultCard";
import type { CardKind } from "./chat/ResultCard";
import { TurnStatsFooter } from "../../components/TurnStatsFooter";
import { ContextPressureNote } from "../../components/ContextPressureNote";
import { SubagentTree, applySubagentProgress } from "../../components/SubagentTree";
import type { SubagentRun } from "../../components/SubagentTree";
import { AttachmentTray } from "../../components/AttachmentTray";
import { prepareImage, validateAttachmentSet } from "../../lib/imageAttach";
import type { PreparedImage } from "../../lib/imageAttach";
import "./chat.css";

// ── Types ─────────────────────────────────────────────────────

interface ChatMessage {
  id: string;
  who: "user" | "goose";
  text: string;
  card?: CardKind;
  streaming?: boolean;
  turnStats?: TurnStats;
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** Set when the server said the context window is filling — renders the
   *  pressure note and the manual compaction control (PAI-4 P7b). */
  contextWarning?: ContextWarning;
  /** PAI-6 P6. Delegations this turn started, folded from `subagent_progress`
   *  frames through the shared reducer. */
  delegations?: SubagentRun[];
  /** Local preview URLs for images attached to a live-sent message. */
  images?: string[];
}

// ── Constants ─────────────────────────────────────────────────

function makeSeed(userName: string): ChatMessage[] {
  const greeting = userName
    ? `Morning, ${userName}. The house is set to Good Morning — lights are easing up and coffee’s brewing. Anything you need?`
    : "Morning! The house is set to Good Morning — lights are easing up and coffee’s brewing. Anything you need?";
  return [
    { id: "seed-0", who: "goose", text: greeting },
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
}

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
  const [msgs, setMsgs] = useState<ChatMessage[]>(() => makeSeed(""));
  const [seeded, setSeeded] = useState(true); // true = currently showing seed

  // Populate the greeting with the real user name once settings are loaded
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getSettings()
      .then((s) => {
        const name = s.user_name?.trim() ?? "";
        setMsgs(makeSeed(name));
        setShowTurnStats(s.show_turn_stats ?? false);
      })
      .catch(() => {});
  }, [state.serverOnline]);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [showTurnStats, setShowTurnStats] = useState(false);
  const [attachments, setAttachments] = useState<PreparedImage[]>([]);
  const [attachError, setAttachError] = useState<string | null>(null);
  // Fail-open: an unknown/failed capabilities fetch never disables attaching —
  // it only disables once we've SUCCESSFULLY confirmed the model lacks vision.
  const [visionCapable, setVisionCapable] = useState(true);
  const [capabilitiesKnown, setCapabilitiesKnown] = useState(false);

  // Load vision capability once the server is reachable.
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  const attachDisabled = capabilitiesKnown && !visionCapable;
  const attachTitle = attachDisabled
    ? "The active model cannot read images. Switch to a vision-capable model such as gemma-4-E2B-it."
    : "Attach image";

  const removeAttachment = useCallback((index: number) => {
    setAttachments((prev) => {
      const target = prev[index];
      if (target) URL.revokeObjectURL(target.previewUrl);
      return prev.filter((_, i) => i !== index);
    });
  }, []);

  const addFiles = useCallback(async (files: File[]) => {
    if (files.length === 0) return;
    const prepared: PreparedImage[] = [];
    let firstError: string | null = null;
    for (const file of files) {
      try {
        prepared.push(await prepareImage(file));
      } catch (e) {
        firstError = e instanceof Error ? e.message : "Could not read that image.";
      }
    }
    if (prepared.length === 0) {
      if (firstError) setAttachError(firstError);
      return;
    }
    const capErr = validateAttachmentSet(attachments, prepared);
    if (capErr) {
      prepared.forEach((p) => URL.revokeObjectURL(p.previewUrl));
      setAttachError(capErr);
      return;
    }
    setAttachError(firstError); // surface a partial-batch MIME rejection, if any
    setAttachments((prev) => [...prev, ...prepared]);
  }, [attachments]);

  const fileInputRef = useRef<HTMLInputElement>(null);

  function onAttachClick() {
    fileInputRef.current?.click();
  }

  function onFileInputChange(e: React.ChangeEvent<HTMLInputElement>) {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    void addFiles(files);
  }

  function onPaste(e: React.ClipboardEvent<HTMLInputElement>) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) => f.type.startsWith("image/"));
    if (files.length === 0) return;
    e.preventDefault();
    void addFiles(files);
  }

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
      if ((!t && attachments.length === 0) || busy) return;

      // Clear seed on first real send
      if (seeded) {
        setSeeded(false);
      }

      const pendingAttachments = attachments;
      const imagePayload: ImageAttachment[] = pendingAttachments.map((a) => ({ data: a.data, mime_type: a.mime_type }));

      setText("");
      setBusy(true);
      inThinkBlockRef.current = false;

      const userMsg: ChatMessage = {
        id: nextMsgId(),
        who: "user",
        text: t,
        images: pendingAttachments.length > 0 ? pendingAttachments.map((a) => a.previewUrl) : undefined,
      };
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
      // Bubble now holds its own copy of previewUrl — don't revoke on send.
      setAttachments([]);
      setAttachError(null);

      try {
        api.setToken(state.sessionToken);
        for await (const event of api.chatStream(
          t,
          sessionIdRef.current,
          state.sessionToken ?? undefined,
          undefined,
          imagePayload,
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
          } else if (ev.type === "turn_stats") {
            // Attach by id, not array position, so a message-list refresh
            // mid-stream can never misdirect the stats.
            const stats = ev as unknown as TurnStats;
            setMsgs((prev) =>
              prev.map((m) => (m.id === agentMsg.id ? { ...m, turnStats: stats } : m)),
            );
          } else if (ev.type === "turn_limit_reached") {
            // The agent ran out of turns rather than finishing — mark the
            // message (by id, same reasoning as turn_stats) so it offers a
            // Continue action.
            const limit = ev.max_turns ?? 0;
            setMsgs((prev) =>
              prev.map((m) => (m.id === agentMsg.id ? { ...m, turnLimit: limit } : m)),
            );
          } else if (ev.type === "subagent_progress") {
            // PAI-6 P6. Attach by id — same reasoning as turn_stats — and fold
            // through the shared reducer rather than a second copy of it.
            setMsgs((prev) =>
              prev.map((m) =>
                m.id === agentMsg.id
                  ? { ...m, delegations: applySubagentProgress(m.delegations ?? [], ev) }
                  : m,
              ),
            );
          } else if (ev.type === "context_warning") {
            // PAI-4 P7b. The window is filling. Attach by id — same reasoning
            // as turn_stats — so the note lands on this turn and not on
            // whatever message a mid-stream history refresh left last.
            const cw = ev as unknown as ContextWarning;
            setMsgs((prev) =>
              prev.map((m) => (m.id === agentMsg.id ? { ...m, contextWarning: cw } : m)),
            );
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
    [text, attachments, busy, seeded, state.sessionToken], // state.sessionId intentionally via ref
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

  const canSend = (text.trim().length > 0 || attachments.length > 0) && !busy;

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
              {/* PAI-6 P6. Live, not gated on `!m.streaming`: a delegating
                  turn is blocked inside one tool call for the whole of its
                  child's run. */}
              {m.who === "goose" && m.delegations && m.delegations.length > 0 && (
                <SubagentTree runs={m.delegations} />
              )}
              <div className={`ch-bubble ch-bubble--${m.who}`}>
                {m.images && m.images.length > 0 && (
                  <div className="ch-bubble__images">
                    {m.images.map((src, i) => (
                      <img key={i} src={src} alt={`Attached image ${i + 1}`} className="ch-bubble__image" />
                    ))}
                  </div>
                )}
                {m.text || (m.streaming ? " " : "")}
              </div>
              {m.card && !m.streaming && (
                <div className="ch-card">
                  <ResultCard kind={m.card} />
                </div>
              )}
              {m.who === "goose" && !m.streaming && m.turnLimit !== undefined && (
                <div className="turn-limit">
                  <span className="turn-limit__note">
                    Stopped after {m.turnLimit} steps.
                  </span>
                  <button
                    className="turn-limit__btn"
                    onClick={() => sendMessage(CONTINUE_TURN_MESSAGE)}
                    disabled={busy}
                  >
                    <HubIco d={HP_PATHS.play} size={12} color="currentColor" /> Continue
                  </button>
                </div>
              )}
              {m.who === "goose" && !m.streaming && m.contextWarning && (
                <ContextPressureNote
                  warning={m.contextWarning}
                  sessionId={sessionIdRef.current ?? null}
                />
              )}
              {m.who === "goose" && !m.streaming && showTurnStats && m.turnStats && (
                <TurnStatsFooter stats={m.turnStats} />
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

      {/* Pending image attachments */}
      <AttachmentTray attachments={attachments} onRemove={removeAttachment} />
      {attachError && <p className="attach-error" role="alert">{attachError}</p>}

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
        <button
          className="ch-attach"
          onClick={onAttachClick}
          disabled={busy || attachDisabled}
          aria-label="Attach image"
          title={attachTitle}
          type="button"
        >
          <Paperclip size={18} />
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept="image/*"
          multiple
          hidden
          onChange={onFileInputChange}
        />
        <input
          ref={inputRef}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={handleKeyDown}
          onPaste={onPaste}
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
