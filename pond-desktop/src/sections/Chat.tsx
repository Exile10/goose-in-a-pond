import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import { Brain, Check, ChevronDown, Copy, Cpu, History, Loader2, Paperclip, Pencil, PenSquare, PlayCircle, RefreshCw, ThumbsDown, ThumbsUp, Wrench, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { nextCardId } from "../state/reducer";
import type { ContextCard as ContextCardType } from "../state/reducer";
import { ToolCallChip } from "../components/ToolCallChip";
import { SessionDropdown } from "../components/SessionDropdown";
import { ThinkingPlaceholder } from "../components/ThinkingPlaceholder";
import { AttachmentTray } from "../components/AttachmentTray";
import { GooseAvatar } from "../hub/views/chat/GooseAvatar";
import { TypingIndicator } from "../hub/views/chat/TypingIndicator";
import { HubIco, micEl } from "../hub/primitives/HubIco";
import { HP_PATHS } from "../hub/primitives/icons";
import { CONTINUE_TURN_MESSAGE } from "../api/types";
import type { ChatEvent, ContextWarning, ImageAttachment, ModelEntry, SessionMessage, SessionSummary, TurnStats } from "../api/types";
import { TurnStatsFooter } from "../components/TurnStatsFooter";
import { ContextPressureNote } from "../components/ContextPressureNote";
import { SubagentTree, applySubagentProgress } from "../components/SubagentTree";
import type { SubagentRun } from "../components/SubagentTree";
import { filterThinking } from "../lib/thinkFilter";
import { prepareImage, validateAttachmentSet } from "../lib/imageAttach";
import type { PreparedImage } from "../lib/imageAttach";

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
  turnStats?: TurnStats;
  error?: boolean;
  historyToolNames?: string[];
  /** Set when the agent stopped on its turn budget — renders a Continue action. */
  turnLimit?: number;
  /** PAI-4 P7b. Set when the turn's `context_warning` frame said the window is
   *  filling — renders the pressure line and the "Compact now" control. */
  contextWarning?: ContextWarning;
  /** PAI-6 P6. Delegations this turn started, folded from `subagent_progress`
   *  frames. Rendered WHILE streaming, unlike every other note here: a tree
   *  nobody sees until the turn ends is the spinner it replaces. */
  delegations?: SubagentRun[];
  /** Image preview URLs — either a live send's local previewUrl, or a
   *  built `${apiBase}${url}` for images replayed from session history. */
  images?: string[];
  /** The persisted session_messages.id this bubble corresponds to. Absent
   *  for a just-sent live turn until the "done" event backfills it (see
   *  sendMessage) — copy/edit/refresh/like/dislike are disabled until then,
   *  since they all act against this id. */
  backendId?: string;
  /** Agent messages only: current like/dislike vote, mirrors the backend's
   *  `liked` column. `null`/absent = no vote. */
  liked?: boolean | null;
}

function sessionMessagesToMessages(raw: SessionMessage[]): Message[] {
  const out: Message[] = [];
  for (const m of raw) {
    if (m.role === "tool") continue;
    const images = m.images?.length
      ? m.images.map((img) => api.sessionAttachmentUrl(m.session_id, img.id))
      : undefined;
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
      // PAI-5 P6. The panel below already renders `thinkingBlocks` and is
      // already gated on `!streaming`, which is exactly right for replayed
      // history. All that was missing was the refill: before this, reasoning
      // existed only for the lifetime of the SSE connection that produced it,
      // so reloading a conversation showed every answer with the thinking that
      // led to it silently gone.
      const thinkingBlocks = m.thinking?.length ? m.thinking : undefined;
      out.push({
        id: ++_msgId,
        role: "agent",
        text: m.content,
        historyToolNames,
        images,
        thinkingBlocks,
        // The persisted id and the vote ride the SAME row as the reasoning.
        // Pushing them as a second entry renders every assistant turn twice on
        // reload, which is what a keep-both merge of these two changes does if
        // nobody looks — `Chat.test.tsx`'s PAI-5 replay tests caught it as
        // "found multiple elements with the text".
        backendId: m.id,
        liked: m.liked ?? null,
      });
    } else {
      out.push({ id: ++_msgId, role: "user", text: m.content, images, backendId: m.id });
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
  const [showTurnStats, setShowTurnStats]         = useState(false);
  const [attachments, setAttachments]             = useState<PreparedImage[]>([]);
  const [attachError, setAttachError]             = useState<string | null>(null);
  // Which user message (by local id) is being edited inline, if any.
  const [editingId, setEditingId]                 = useState<number | null>(null);
  const [editText, setEditText]                   = useState("");
  // Fail-open: an unknown/failed capabilities fetch never disables attaching —
  // it only disables once we've SUCCESSFULLY confirmed the model lacks vision.
  const [visionCapable, setVisionCapable]         = useState(true);
  const [capabilitiesKnown, setCapabilitiesKnown] = useState(false);

  const bottomRef        = useRef<HTMLDivElement>(null);
  const textareaRef      = useRef<HTMLTextAreaElement>(null);
  const modelSelectorRef = useRef<HTMLDivElement>(null);
  const fileInputRef     = useRef<HTMLInputElement>(null);
  const sessionIdRef     = useRef<string | undefined>(state.sessionId ?? undefined);
  const inThinkBlockRef  = useRef(false);

  const attachDisabled = capabilitiesKnown && !visionCapable;
  const attachTitle = attachDisabled
    ? "The active model cannot read images. Switch to a vision-capable model such as gemma-4-E2B-it."
    : "Attach image";

  const clearAttachments = useCallback(() => {
    setAttachments((prev) => {
      prev.forEach((p) => URL.revokeObjectURL(p.previewUrl));
      return [];
    });
    setAttachError(null);
  }, []);

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

  function onAttachClick() {
    fileInputRef.current?.click();
  }

  function onFileInputChange(e: React.ChangeEvent<HTMLInputElement>) {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    void addFiles(files);
  }

  function onPaste(e: React.ClipboardEvent<HTMLTextAreaElement>) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) => f.type.startsWith("image/"));
    if (files.length === 0) return;
    e.preventDefault();
    void addFiles(files);
  }

  // Load vision capability once the server is reachable. Failure degrades to
  // "let the server explain" (fail open) rather than hiding the affordance.
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getModelCapabilities()
      .then((caps) => { setVisionCapable(caps.vision); setCapabilitiesKnown(true); })
      .catch(() => { setCapabilitiesKnown(false); });
  }, [state.serverOnline]);

  // Sync session ref; load history when session changes externally
  useEffect(() => {
    const newId = state.sessionId ?? undefined;
    if (newId === sessionIdRef.current) return;
    const wasExternal = !!newId;
    sessionIdRef.current = newId;
    clearAttachments();
    if (!wasExternal) return;
    api.getSessionMessages(newId!)
      .then((msgs) => setMessages(sessionMessagesToMessages(msgs ?? [])))
      .catch((err) => console.warn("Could not load session history (non-fatal):", err));
  }, [state.sessionId, clearAttachments]);

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

  // Load show_turn_stats once the server is reachable (cold-start safe:
  // re-runs on the offline->online transition like the session loader below).
  useEffect(() => {
    if (!state.serverOnline) return;
    api.getSettings().then((s) => {
      setShowTurnStats(s.show_turn_stats ?? false);
    }).catch(() => {});
  }, [state.serverOnline]);

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
    clearAttachments();
    textareaRef.current?.focus();
  }

  const renameSession = useCallback(async (id: string, title: string) => {
    // Optimistically update the row, then persist. Refresh reconciles with the
    // server's stored/derived title on success or failure.
    setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, title } : s)));
    try {
      await api.renameSession(id, title);
    } catch (err) {
      console.warn("Rename failed (non-fatal):", err);
    } finally {
      refreshSessions();
    }
  }, [refreshSessions]);

  const deleteSession = useCallback(async (id: string) => {
    try {
      await api.deleteSession(id);
    } catch (err) {
      console.warn("Delete failed (non-fatal):", err);
    }
    // If the deleted conversation was the active one, drop back to a blank chat.
    if (sessionIdRef.current === id) {
      newConversation();
    }
    refreshSessions();
  // newConversation is a stable component-scope function; safe to omit.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshSessions]);

  const sendMessage = useCallback(async (directText?: string) => {
    const text = (directText ?? input).trim();
    if ((!text && attachments.length === 0) || busy || !state.serverOnline) return;

    const pendingAttachments = attachments;
    const imagePayload: ImageAttachment[] = pendingAttachments.map((a) => ({ data: a.data, mime_type: a.mime_type }));

    setInput("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    setBusy(true);
    inThinkBlockRef.current = false;

    const userMsg:  Message = {
      id: ++_msgId,
      role: "user",
      text,
      images: pendingAttachments.length > 0 ? pendingAttachments.map((a) => a.previewUrl) : undefined,
    };
    const agentMsg: Message = { id: ++_msgId, role: "agent", text: "", streaming: true };
    setMessages((prev) => [...prev, userMsg, agentMsg]);
    // Clear the pending tray now that the images have been captured into
    // userMsg above — the bubble keeps its own copy of the previewUrl, so we
    // deliberately don't revoke it here (that would blank the just-sent
    // thumbnail); only a later manual removal or new-conversation revokes it.
    setAttachments([]);
    setAttachError(null);

    try {
      api.setToken(state.sessionToken);
      for await (const event of api.chatStream(text, sessionIdRef.current, state.sessionToken ?? undefined, undefined, imagePayload)) {
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
          // The stream never carries the persisted message ids, so copy/edit/
          // refresh/like/dislike (which all act on a real backend id) have
          // nothing to target yet. Fetch the small tail of the session and
          // match by id, not array position — same reasoning as turn_stats
          // below, a session switch mid-fetch must not misattribute this.
          const doneSessionId = ev.session_id;
          const forUser = userMsg.id;
          const forAgent = agentMsg.id;
          void (async () => {
            try {
              const recent = await api.getSessionMessages(doneSessionId, 10);
              const nonTool = recent.filter((m) => m.role !== "tool");
              const lastUser = [...nonTool].reverse().find((m) => m.role === "user");
              const lastAgent = [...nonTool].reverse().find((m) => m.role === "assistant");
              setMessages((prev) => prev.map((m) => {
                if (m.id === forAgent && lastAgent) return { ...m, backendId: lastAgent.id, liked: lastAgent.liked ?? null };
                if (m.id === forUser && lastUser) return { ...m, backendId: lastUser.id };
                return m;
              }));
            } catch {
              // Non-fatal: the turn already rendered; only the action icons
              // stay disabled until the next successful history load.
            }
          })();
        } else if (ev.type === "turn_stats") {
          // Attach by id, not array position — a mid-stream session switch
          // replaces `messages` with another conversation's history, and the
          // stats must never land on one of those messages.
          const stats = ev as unknown as TurnStats;
          setMessages((prev) =>
            prev.map((m) => (m.id === agentMsg.id ? { ...m, turnStats: stats } : m)),
          );
        } else if (ev.type === "turn_limit_reached") {
          // The agent ran out of turns rather than finishing. Mark the message
          // (by id, same reasoning as turn_stats) so it offers a Continue action
          // instead of leaving the backend's "would you like me to continue?"
          // as a question nothing can answer.
          const limit = ev.max_turns ?? 0;
          setMessages((prev) =>
            prev.map((m) => (m.id === agentMsg.id ? { ...m, turnLimit: limit } : m)),
          );
        } else if (ev.type === "subagent_progress") {
          // PAI-6 P6. Attach by id — same reasoning as turn_stats — and fold
          // through the one shared reducer, so this surface and the hub cannot
          // disagree about what a frame means.
          setMessages((prev) =>
            prev.map((m) =>
              m.id === agentMsg.id
                ? { ...m, delegations: applySubagentProgress(m.delegations ?? [], ev) }
                : m,
            ),
          );
        } else if (ev.type === "context_warning") {
          // PAI-4 P7b. The window is filling. Attach by id — same reasoning as
          // turn_stats and turn_limit_reached — so the note lands on this turn
          // and not on whatever message a mid-stream session switch left last.
          const cw = ev as unknown as ContextWarning;
          setMessages((prev) =>
            prev.map((m) => (m.id === agentMsg.id ? { ...m, contextWarning: cw } : m)),
          );
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
  }, [input, attachments, busy, state.serverOnline, state.sessionToken, dispatch, refreshSessions]);

  const copyMessageText = useCallback((text: string) => {
    void navigator.clipboard.writeText(text).catch(() => {});
  }, []);

  // Drop everything from `msgId` onward in the LOCAL list — used by both edit
  // and refresh right before resending, so the stale pair never briefly shows
  // next to the fresh one.
  const truncateLocalFrom = useCallback((msgId: number) => {
    setMessages((prev) => {
      const idx = prev.findIndex((m) => m.id === msgId);
      return idx === -1 ? prev : prev.slice(0, idx);
    });
  }, []);

  // Shared by refresh (same text) and edit-submit (new text): truncate the
  // persisted history from this user message onward, then resend through the
  // normal send path — no separate regenerate endpoint, `/chat/stream`
  // already knows how to append a fresh turn.
  const truncateAndResend = useCallback(async (msg: Message, text: string) => {
    if (!msg.backendId || !sessionIdRef.current || busy) return;
    try {
      await api.deleteMessagesFrom(sessionIdRef.current, msg.backendId);
    } catch (e) {
      console.error("Failed to truncate session before resend:", e);
      return;
    }
    truncateLocalFrom(msg.id);
    void sendMessage(text);
  }, [busy, sendMessage, truncateLocalFrom]);

  const startEditing = useCallback((msg: Message) => {
    setEditingId(msg.id);
    setEditText(msg.text);
  }, []);

  const cancelEditing = useCallback(() => {
    setEditingId(null);
    setEditText("");
  }, []);

  const submitEdit = useCallback((msg: Message) => {
    const trimmed = editText.trim();
    if (!trimmed) return;
    setEditingId(null);
    void truncateAndResend(msg, trimmed);
  }, [editText, truncateAndResend]);

  // Called from the AGENT bubble, but truncateAndResend needs a USER message
  // to delete-from-and-resend — regenerating means "redo the answer to the
  // prompt right before this one," so walk back to find it. Truncating from
  // there removes both the old prompt row and its stale answer; resending
  // the same text creates a fresh pair, keeping exactly one user/answer per
  // turn (there's no lighter "keep the prompt, only replace the answer"
  // primitive — see truncateAndResend's own comment).
  const refreshResponse = useCallback((agentMsg: Message) => {
    const idx = messages.findIndex((m) => m.id === agentMsg.id);
    const precedingUser = idx === -1 ? undefined : [...messages.slice(0, idx)].reverse().find((m) => m.role === "user");
    if (!precedingUser) return;
    void truncateAndResend(precedingUser, precedingUser.text);
  }, [messages, truncateAndResend]);

  // `null` clears a vote — clicking the already-active thumb toggles it off.
  // Optimistic: flips locally first, reverts only if the PUT fails.
  const setFeedback = useCallback((msg: Message, liked: boolean) => {
    if (!msg.backendId || !sessionIdRef.current) return;
    const sessionId = sessionIdRef.current;
    const backendId = msg.backendId;
    const prevLiked = msg.liked ?? null;
    const next = prevLiked === liked ? null : liked;
    setMessages((prev) => prev.map((m) => (m.id === msg.id ? { ...m, liked: next } : m)));
    api.setMessageFeedback(sessionId, backendId, next).catch((e) => {
      console.error("Failed to save feedback:", e);
      setMessages((prev) => prev.map((m) => (m.id === msg.id ? { ...m, liked: prevLiked } : m)));
    });
  }, []);

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
            onRename={renameSession}
            onDelete={deleteSession}
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
                {/* Delegation tree. Deliberately NOT gated on `!msg.streaming`:
                    the whole point is that a turn which is blocked inside a
                    `delegate` tool call stops looking like a hung spinner. */}
                {msg.role === "agent" && msg.delegations && msg.delegations.length > 0 && (
                  <SubagentTree runs={msg.delegations} />
                )}
                {/* Bubble */}
                <div className={`ch-bubble ${msg.role === "user" ? "ch-bubble--user" : `ch-bubble--goose${msg.error ? " ch-bubble--error" : ""}`}`}>
                  {msg.images && msg.images.length > 0 && (
                    <div className="ch-bubble__images">
                      {msg.images.map((src, i) => (
                        <img key={i} src={src} alt={`Attached image ${i + 1}`} className="ch-bubble__image" />
                      ))}
                    </div>
                  )}
                  {msg.role === "user" && editingId === msg.id ? (
                    <div className="ch-bubble__edit">
                      <textarea
                        className="ch-bubble__edit-input"
                        value={editText}
                        onChange={(e) => setEditText(e.target.value)}
                        onKeyDown={(e) => {
                          if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); submitEdit(msg); }
                          if (e.key === "Escape") { e.preventDefault(); cancelEditing(); }
                        }}
                        autoFocus
                        rows={Math.min(8, Math.max(2, editText.split("\n").length))}
                      />
                      <div className="ch-bubble__edit-actions">
                        <button type="button" className="ch-bubble__edit-btn ch-bubble__edit-btn--cancel" onClick={cancelEditing}>
                          <X size={12} aria-hidden /> Cancel
                        </button>
                        <button
                          type="button"
                          className="ch-bubble__edit-btn ch-bubble__edit-btn--save"
                          onClick={() => submitEdit(msg)}
                          disabled={!editText.trim()}
                        >
                          <Check size={12} aria-hidden /> Save &amp; resend
                        </button>
                      </div>
                    </div>
                  ) : (
                    msg.text || (msg.streaming
                      ? msg.status
                        ? <ThinkingPlaceholder status={msg.status} />
                        : <span className="stream-dots"><span /><span /><span /></span>
                      : "")
                  )}
                </div>
                {/* Copy / edit — user messages only */}
                {msg.role === "user" && editingId !== msg.id && (
                  <div className="ch-bubble__actions" role="group" aria-label="Message actions">
                    <button type="button" className="ch-bubble__action" title="Copy" onClick={() => copyMessageText(msg.text)}>
                      <Copy size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className="ch-bubble__action"
                      title="Edit and resend"
                      disabled={!msg.backendId || busy}
                      onClick={() => startEditing(msg)}
                    >
                      <Pencil size={13} aria-hidden />
                    </button>
                  </div>
                )}
                {/* Copy / regenerate / like / dislike — agent messages only; like/dislike feeds (or excludes from) training data */}
                {msg.role === "agent" && !msg.streaming && (
                  <div className="ch-bubble__actions" role="group" aria-label="Message actions">
                    <button type="button" className="ch-bubble__action" title="Copy" onClick={() => copyMessageText(msg.text)}>
                      <Copy size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className="ch-bubble__action"
                      title="Regenerate response"
                      disabled={!msg.backendId || busy}
                      onClick={() => refreshResponse(msg)}
                    >
                      <RefreshCw size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className={`ch-bubble__action${msg.liked === true ? " ch-bubble__action--liked" : ""}`}
                      title="Good response — use for training"
                      disabled={!msg.backendId}
                      onClick={() => setFeedback(msg, true)}
                    >
                      <ThumbsUp size={13} aria-hidden />
                    </button>
                    <button
                      type="button"
                      className={`ch-bubble__action${msg.liked === false ? " ch-bubble__action--disliked" : ""}`}
                      title="Bad response — exclude from training"
                      disabled={!msg.backendId}
                      onClick={() => setFeedback(msg, false)}
                    >
                      <ThumbsDown size={13} aria-hidden />
                    </button>
                  </div>
                )}
                {/* Model role + token meta */}
                {msg.role === "agent" && msg.modelRole && !msg.streaming && (
                  <span className="bubble__meta">
                    {msg.modelRole}
                    {msg.tokenUsage && msg.tokenUsage.completion_tokens > 0 && (
                      <> · {msg.tokenUsage.completion_tokens} tokens</>
                    )}
                  </span>
                )}
                {/* Turn budget exhausted — offer a continuation turn */}
                {msg.role === "agent" && !msg.streaming && msg.turnLimit !== undefined && (
                  <div className="turn-limit">
                    <span className="turn-limit__note">
                      Stopped after {msg.turnLimit} steps.
                    </span>
                    <button
                      className="turn-limit__btn"
                      onClick={() => sendMessage(CONTINUE_TURN_MESSAGE)}
                      disabled={busy}
                    >
                      <PlayCircle size={12} aria-hidden /> Continue
                    </button>
                  </div>
                )}
                {/* Context window filling — pressure line + "Compact now".
                    Deliberately NOT gated on showTurnStats: `show_turn_stats`
                    defaults to false in Rust, so borrowing that gate would ship
                    this invisible on every default install, which is exactly
                    why TurnStatsFooter was rejected as the host. */}
                {msg.role === "agent" && !msg.streaming && msg.contextWarning && (
                  <ContextPressureNote
                    warning={msg.contextWarning}
                    sessionId={sessionIdRef.current ?? null}
                  />
                )}
                {/* Inference stats footer */}
                {msg.role === "agent" && !msg.streaming && showTurnStats && msg.turnStats && (
                  <TurnStatsFooter stats={msg.turnStats} />
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

      {/* Pending image attachments */}
      <AttachmentTray attachments={attachments} onRemove={removeAttachment} />
      {attachError && <p className="attach-error" role="alert">{attachError}</p>}

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
        <button
          className="ch-attach"
          onClick={onAttachClick}
          disabled={!state.serverOnline || busy || attachDisabled}
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
        <textarea
          ref={textareaRef}
          className="chat2__textarea"
          value={input}
          onChange={onInput}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          placeholder="Message Goose…"
          disabled={!state.serverOnline || busy}
          aria-label="Message input"
          rows={1}
        />
        <button
          className="ch-send"
          onClick={() => sendMessage()}
          disabled={(!input.trim() && attachments.length === 0) || !state.serverOnline || busy}
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
