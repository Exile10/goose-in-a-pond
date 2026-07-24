import React, {
  createContext,
  useContext,
  useReducer,
  useEffect,
  useRef,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { api } from "../api/PondApiClient";
import { refreshHomeData } from "../hub/state/hubDataStore";
import {
  reducer,
  buildInitialState,
  nextTranscriptId,
  nextCardId,
  type AppState,
  type AppAction,
  type TranscriptMessage,
  type ContextCard,
  type ScheduleToast,
} from "./reducer";
import type { ScheduleRunNotification } from "../api/types";

const StateCtx = createContext<AppState | null>(null);
const DispatchCtx = createContext<React.Dispatch<AppAction> | null>(null);

export function AppContextProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, undefined, buildInitialState);
  const scheduleEsRef = useRef<EventSource | null>(null);

  // Global SSE listener for schedule result events.
  // Opens once when the server comes online and stays open regardless of which
  // section is active, so toasts work on any page.
  useEffect(() => {
    if (!state.serverOnline) return;

    // Close any existing connection before opening a new one.
    if (scheduleEsRef.current) {
      scheduleEsRef.current.close();
      scheduleEsRef.current = null;
    }

    const baseUrl = state.serverUrl || "http://127.0.0.1:4000";
    const url = `${baseUrl}/api/v1/schedules/events`;
    const es = new EventSource(url);
    scheduleEsRef.current = es;

    es.onmessage = (ev) => {
      try {
        const data = JSON.parse(ev.data) as {
          id?: string;
          schedule_id?: string;
          schedule_label?: string;
          status?: string;
          result?: string;
          error?: string;
        };
        const status = data.status;
        if (status !== "completed" && status !== "failed" && status !== "running") return;
        const toast: ScheduleToast = {
          id: data.id || data.schedule_id || String(Date.now()),
          schedule_id: data.schedule_id || data.id || "",
          schedule_label: data.schedule_label || data.schedule_id || "Schedule",
          status: status as "completed" | "failed" | "running",
          result: data.result,
          error: data.error,
          timestamp: Date.now(),
        };
        dispatch({ type: "SCHEDULE_RESULT", payload: toast });

        // Also feed the persistent notifications list
        const notification: ScheduleRunNotification = {
          id: toast.id,
          scheduleId: toast.schedule_id,
          scheduleName: toast.schedule_label,
          status: toast.status,
          result: toast.result ?? null,
          error: toast.error ?? null,
          startedAt: new Date().toISOString(),
          finishedAt: toast.status !== "running" ? new Date().toISOString() : null,
          durationMs: null,
          read: false,
          excerpt: (toast.result ?? toast.error ?? "").slice(0, 80),
          recipe: inferRecipe(toast.schedule_label),
        };

        // ADD_SCHEDULE_RUN deduplicates by id (filter + prepend), so it handles
        // both the normal "running → completed" update and the "missed running event" case.
        dispatch({ type: "ADD_SCHEDULE_RUN", payload: notification });
      } catch {
        // ignore parse errors
      }
    };

    // Fetch existing schedule run history on connect
    api.getAllRecentRuns(5)
      .then((runs) => {
        const notifications: ScheduleRunNotification[] = runs.map((r) => ({
          id: r.id,
          scheduleId: r.schedule_id,
          scheduleName: r.schedule_name,
          status: r.status,
          result: r.result ?? null,
          error: r.error ?? null,
          startedAt: r.started_at,
          finishedAt: r.finished_at ?? null,
          durationMs: r.duration_ms ?? null,
          read: true, // historical runs start as read
          excerpt: (r.result ?? r.error ?? "").slice(0, 80),
          recipe: inferRecipe(r.schedule_name),
        }));
        dispatch({ type: "SET_SCHEDULE_RUNS", payload: notifications });
      })
      .catch(() => { /* schedule runs fetch failed — non-fatal */ });

    return () => {
      es.close();
      scheduleEsRef.current = null;
    };
  }, [state.serverOnline, state.serverUrl]);

  // Hub data (weather, now-playing, devices, ...) is loaded once at module
  // import time, which races the server's boot — cold start with models can
  // take well over a minute. Re-trigger once the server actually reports
  // online so the dashboard doesn't stay stuck on mock data indefinitely.
  useEffect(() => {
    if (!state.serverOnline) return;
    void refreshHomeData();
  }, [state.serverOnline]);

  useEffect(() => {
    const unlisten: Array<() => void> = [];

    // Guard: Tauri IPC may not be available in non-Tauri environments (browser dev, tests)
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!isTauri) {
      // Browser dev / Playwright mode: mark server online immediately so
      // all sections can load. Auth token not needed (loopback bypass).
      dispatch({ type: "SERVER_ONLINE" });
      // Still check onboarding status so the wizard shows for new setups.
      api.getOnboardingStatus()
        .then((status) => {
          if (!status.onboarded) {
            dispatch({ type: "SET_NEEDS_ONBOARDING", payload: true });
          }
        })
        .catch(() => {});
      // hubDataStore fires its own fetch on module import, which can race
      // ahead of this readiness check and lose (silently falling back to
      // mock data with nothing to retry it). Re-fetch now that we know a
      // round-trip to the server actually works.
      void refreshHomeData();
      return;
    }

    // Check onboarding status — if not yet onboarded, show the wizard UI
    // instead of auto-completing silently.
    const ensureOnboarded = async () => {
      try {
        const status = await api.getOnboardingStatus();
        if (!status.onboarded) {
          dispatch({ type: "SET_NEEDS_ONBOARDING", payload: true });
        }
      } catch (err) {
        console.warn("Onboarding check failed (non-fatal):", err);
      }
    };

    // Centralised "server is up — handshake + ensure onboarded" so both the
    // initial-probe path AND the event-listener path share one code body.
    // Re-entrant: dedupe via a flag so a fast Rust emit + a slow polling
    // probe don't double-handshake.
    let onlineHandled = false;
    const handleServerOnline = () => {
      if (onlineHandled) return;
      onlineHandled = true;
      dispatch({ type: "SERVER_ONLINE" });
      // connect() reuses a persisted token / refresh across restarts and only
      // falls back to a fresh pairing-code pair when neither is usable.
      api.connect("pond-desktop")
        .then(async (token) => {
          if (token) {
            dispatch({ type: "SET_SESSION_TOKEN", payload: token });
          } else {
            console.warn("Could not establish a session (pairing rejected).");
          }
          await ensureOnboarded();
          // hubDataStore fires its own fetch on module import, which almost
          // always races ahead of the session token being set above — that
          // first fetch runs unauthenticated, fails, and permanently caches
          // mock fallback data with nothing to retry it afterward. Re-fetch
          // now that the token is actually in place.
          void refreshHomeData();
        })
        .catch((err) => console.warn("Connect failed (non-fatal):", err));
    };

    // Server online/offline status — reactive path.
    listen<boolean>("server-status", (e) => {
      if (e.payload) {
        handleServerOnline();
      } else {
        // Server went offline — reset so a subsequent online event retriggers.
        onlineHandled = false;
        dispatch({ type: "SERVER_OFFLINE" });
      }
    }).then((u) => unlisten.push(u));

    // Active probe path. Tauri events are NOT buffered: if the Rust side
    // emits `server-status: true` before our `listen()` registration above
    // resolves (which is async), the React side never learns the server is
    // up — the dashboard sits blank until the periodic 10-second health
    // tick fires. The user reported this as "first launch shows nothing,
    // close-and-reopen fixes it". Polling `server_health` here on mount
    // closes the race regardless of event-arrival order.
    let probeCancelled = false;
    (async () => {
      // Short, dense polling (every 250 ms for up to 60 s) so the dashboard
      // appears within a quarter-second of the server actually accepting
      // connections — much snappier than waiting for the 10 s tick.
      for (let i = 0; i < 240; i++) {
        if (probeCancelled || onlineHandled) return;
        try {
          const healthy = await invoke<boolean>("server_health");
          if (healthy) {
            handleServerOnline();
            return;
          }
        } catch {
          // server_health command not registered yet — keep polling.
        }
        await new Promise((r) => setTimeout(r, 250));
      }
    })();
    unlisten.push(() => { probeCancelled = true; });

    listen("server-starting", () => {
      dispatch({ type: "SERVER_STARTING" });
    }).then((u) => unlisten.push(u));

    // Global voice activation hotkey
    listen("desktop-summon", () => {
      dispatch({ type: "VOICE_ACTIVATE" });
    }).then((u) => unlisten.push(u));

    // Recording lifecycle — voice state is now managed explicitly by
    // VoiceMode so calibration recordings don't corrupt it.
    // recording-started: no-op (callers set their own state)
    // recording-aborted: VoiceMode handles state transition itself

    // Transcript (user text after ASR)
    // Rust emits TranscriptResult { text: String } → payload is { text: "..." }
    listen<{ text: string }>("transcript", (e) => {
      const msg: TranscriptMessage = {
        id: nextTranscriptId(),
        role: "user",
        text: e.payload.text,
        timestamp: Date.now(),
      };
      dispatch({ type: "APPEND_TRANSCRIPT", payload: msg });
      dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
      // Seed an empty agent message for token streaming
      const agentMsg: TranscriptMessage = {
        id: nextTranscriptId(),
        role: "agent",
        text: "",
        timestamp: Date.now(),
      };
      dispatch({ type: "APPEND_TRANSCRIPT", payload: agentMsg });
    }).then((u) => unlisten.push(u));

    // Streaming response tokens
    listen<{ token: string; done: boolean }>("response-token", (e) => {
      dispatch({ type: "APPEND_AGENT_TOKEN", payload: e.payload });
    }).then((u) => unlisten.push(u));

    // Tool call results — may include MCP-APP UI hints from backend
    listen<{ tool: string; data: Record<string, unknown>; timestamp_ms: number; renderHint?: string }>(
      "tool-result",
      (e) => {
        const card: ContextCard = {
          id: nextCardId(),
          tool: e.payload.tool,
          data: e.payload.data,
          timestamp_ms: e.payload.timestamp_ms,
          ...(e.payload.renderHint ? { renderHint: e.payload.renderHint } : {}),
        };
        dispatch({ type: "PUSH_CONTEXT_CARD", payload: card });
      },
    ).then((u) => unlisten.push(u));

    // TTS playback
    listen("tts-start", () => {
      dispatch({ type: "SET_VOICE_STATE", payload: "speaking" });
    }).then((u) => unlisten.push(u));

    listen("tts-end", () => {
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    }).then((u) => unlisten.push(u));

    // macOS menu bar — View menu items
    listen("canvas-toggle", () => {
      dispatch({ type: "SET_SECTION", payload: "canvas" });
    }).then((u) => unlisten.push(u));

    listen("switch-to-voice", () => {
      dispatch({ type: "SET_MODE", payload: "voice" });
    }).then((u) => unlisten.push(u));

    // Pipeline errors
    listen<string>("pipeline-error", (e) => {
      dispatch({ type: "SET_VOICE_ERROR", payload: e.payload });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
      // Auto-clear error after 4 seconds
      setTimeout(() => {
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        dispatch({ type: "SET_VOICE_ERROR", payload: null });
      }, 4000);
    }).then((u) => unlisten.push(u));

    // Backend-assigned session ID — emitted at end of chat/stream SSE.
    // Ensures the frontend sessionId tracks the canonical backend session.
    listen<{ session_id: string; model_role: string }>("session-created", (e) => {
      dispatch({ type: "SET_SESSION_ID", payload: e.payload.session_id });
    }).then((u) => unlisten.push(u));

    return () => {
      unlisten.forEach((u) => u());
    };
  }, []);

  return (
    <StateCtx.Provider value={state}>
      <DispatchCtx.Provider value={dispatch}>
        {children}
      </DispatchCtx.Provider>
    </StateCtx.Provider>
  );
}

/** Infer a recipe identifier from the schedule name for debrief card routing. */
function inferRecipe(name: string): string | null {
  const lower = name.toLowerCase();
  if (/morning|briefing|daily.*summ/.test(lower)) return "daily-summary";
  if (/weekly.*report|week.*summ/.test(lower)) return "weekly-report";
  if (/compact|consolidat|memory.*clean/.test(lower)) return "compact-memory";
  return null;
}

export function useAppState(): AppState {
  const ctx = useContext(StateCtx);
  if (!ctx) throw new Error("useAppState must be used within AppContextProvider");
  return ctx;
}

export function useAppDispatch(): React.Dispatch<AppAction> {
  const ctx = useContext(DispatchCtx);
  if (!ctx) throw new Error("useAppDispatch must be used within AppContextProvider");
  return ctx;
}
