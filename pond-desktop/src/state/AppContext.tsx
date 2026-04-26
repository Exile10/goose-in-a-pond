import React, {
  createContext,
  useContext,
  useReducer,
  useEffect,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api/PondApiClient";
import {
  reducer,
  buildInitialState,
  nextTranscriptId,
  nextCardId,
  type AppState,
  type AppAction,
  type TranscriptMessage,
  type ContextCard,
} from "./reducer";

const StateCtx = createContext<AppState | null>(null);
const DispatchCtx = createContext<React.Dispatch<AppAction> | null>(null);

export function AppContextProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(reducer, undefined, buildInitialState);

  useEffect(() => {
    const unlisten: Array<() => void> = [];

    // Guard: Tauri IPC may not be available in non-Tauri environments (browser dev, tests)
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!isTauri) {
      // Browser dev / Playwright mode: mark server online immediately so
      // all sections can load. Auth token not needed (loopback bypass).
      dispatch({ type: "SERVER_ONLINE" });
      return;
    }

    // Ensure onboarding is complete so protected routes are accessible.
    // pond-desktop has no onboarding wizard UI, so we auto-complete on first connect.
    const ensureOnboarded = async () => {
      try {
        const status = await api.getOnboardingStatus();
        if (!status.onboarded) {
          await api.completeOnboarding();
        }
      } catch (err) {
        console.warn("Onboarding check failed (non-fatal):", err);
      }
    };

    // Server online/offline status
    listen<boolean>("server-status", (e) => {
      if (e.payload) {
        dispatch({ type: "SERVER_ONLINE" });
        // Always re-handshake when server comes online — the server restarts
        // alongside the app, so any previously stored token is invalid.
        api.handshake("pond-desktop")
          .then(async (res) => {
            api.setToken(res.token);
            dispatch({ type: "SET_SESSION_TOKEN", payload: res.token });
            // Ensure onboarding is complete before any protected route is called
            await ensureOnboarded();
          })
          .catch((err) => console.warn("Handshake failed (non-fatal):", err));
      } else {
        dispatch({ type: "SERVER_OFFLINE" });
      }
    }).then((u) => unlisten.push(u));

    listen("server-starting", () => {
      dispatch({ type: "SERVER_STARTING" });
    }).then((u) => unlisten.push(u));

    // Global voice activation hotkey
    listen("desktop-summon", () => {
      dispatch({ type: "VOICE_ACTIVATE" });
    }).then((u) => unlisten.push(u));

    // Recording lifecycle — voice state is now managed explicitly by
    // VoiceMode/CanvasOverlay so calibration recordings don't corrupt it.
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

    // Tool call results
    listen<{ tool: string; data: Record<string, unknown>; timestamp_ms: number }>(
      "tool-result",
      (e) => {
        const card: ContextCard = {
          id: nextCardId(),
          tool: e.payload.tool,
          data: e.payload.data,
          timestamp_ms: e.payload.timestamp_ms,
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
      dispatch({ type: "SET_MODE", payload: "canvas" });
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
