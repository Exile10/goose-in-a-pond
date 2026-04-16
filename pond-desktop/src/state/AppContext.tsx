import React, {
  createContext,
  useContext,
  useReducer,
  useEffect,
  useRef,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
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

  // Track whether a partial agent message has been started in the transcript
  const agentMessageStarted = useRef(false);

  useEffect(() => {
    const unlisten: Array<() => void> = [];

    // Guard: Tauri IPC may not be available in non-Tauri environments (browser dev, tests)
    const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!isTauri) return;

    // Server online/offline status
    listen<boolean>("server-status", (e) => {
      if (e.payload) {
        dispatch({ type: "SERVER_ONLINE" });
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

    // Recording lifecycle
    listen("recording-started", () => {
      agentMessageStarted.current = false;
      dispatch({ type: "SET_VOICE_STATE", payload: "recording" });
    }).then((u) => unlisten.push(u));

    listen("recording-aborted", () => {
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    }).then((u) => unlisten.push(u));

    // Transcript (user text after ASR)
    listen<string>("transcript", (e) => {
      const msg: TranscriptMessage = {
        id: nextTranscriptId(),
        role: "user",
        text: e.payload,
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
      agentMessageStarted.current = true;
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
