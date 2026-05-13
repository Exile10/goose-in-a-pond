// ────────────────────────────────────────────────────────────
// useVoiceSession — WebSocket-based voice session hook
//
// Replaces the old 3-HTTP-call pipeline (transcribe → chat-SSE → TTS).
// The Rust command `run_voice_session` owns the WebSocket connection;
// this hook owns the React-side lifecycle: VAD recording, Tauri event
// subscriptions, and app state dispatch.
// ────────────────────────────────────────────────────────────

import { useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAppState, useAppDispatch } from "../../state/AppContext";
import { nextTranscriptId, nextCardId } from "../../state/reducer";
import { api } from "../../api/PondApiClient";

export interface VoiceSessionAPI {
  /** Record audio with VAD, then send to the server via WebSocket. */
  startSession(wavBytes?: number[]): Promise<void>;
  /** Send an interrupt signal — stops current TTS playback and server turn. */
  interrupt(): void;
  /** Clear transcript and context cards. */
  clearConversation(): void;
}

export function useVoiceSession(): VoiceSessionAPI {
  const state = useAppState();
  const dispatch = useAppDispatch();

  // Keep latest state accessible in callbacks without stale closures
  const stateRef = useRef(state);
  stateRef.current = state;

  // Track whether an interrupt was requested while a session is running
  const interruptRef = useRef(false);

  // ── Tauri event listeners ──────────────────────────────────────────────────
  useEffect(() => {
    const unlisten: Array<() => void> = [];

    // State changes from the server pipeline
    listen<string>("voice-state", (e) => {
      const serverState = e.payload;
      // Map server states to our VoiceState type
      const stateMap: Record<string, string> = {
        idle:         "idle",
        transcribing: "thinking",
        thinking:     "thinking",
        speaking:     "speaking",
      };
      const mapped = stateMap[serverState] ?? "idle";
      dispatch({ type: "SET_VOICE_STATE", payload: mapped as any });
    }).then((u) => unlisten.push(u));

    // User transcript from ASR
    listen<{ text: string }>("voice-transcript", (e) => {
      const msg = {
        id: nextTranscriptId(),
        role: "user" as const,
        text: e.payload.text,
        timestamp: Date.now(),
      };
      dispatch({ type: "APPEND_TRANSCRIPT", payload: msg });
      // Seed an empty agent message for token streaming
      dispatch({
        type: "APPEND_TRANSCRIPT",
        payload: {
          id: nextTranscriptId(),
          role: "agent" as const,
          text: "",
          timestamp: Date.now(),
        },
      });
    }).then((u) => unlisten.push(u));

    // Streaming response tokens
    listen<{ content: string }>("voice-token", (e) => {
      dispatch({
        type: "APPEND_AGENT_TOKEN",
        payload: { token: e.payload.content, done: false },
      });
    }).then((u) => unlisten.push(u));

    // Tool call events — push a context card
    listen<{ tool: string; id: string }>("voice-tool-call", (e) => {
      dispatch({
        type: "PUSH_CONTEXT_CARD",
        payload: {
          id: nextCardId(),
          tool: e.payload.tool,
          data: { id: e.payload.id },
          timestamp_ms: Date.now(),
        },
      });
    }).then((u) => unlisten.push(u));

    // Tool result events — update the last context card or push a new one
    listen<{ tool: string; id: string; content: string }>("voice-tool-result", (e) => {
      dispatch({
        type: "PUSH_CONTEXT_CARD",
        payload: {
          id: nextCardId(),
          tool: e.payload.tool,
          data: { id: e.payload.id, result: e.payload.content },
          timestamp_ms: Date.now(),
        },
      });
    }).then((u) => unlisten.push(u));

    // Turn complete
    listen<{ session_id?: string }>("voice-done", (e) => {
      dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });
      if (e.payload?.session_id) {
        dispatch({ type: "SET_SESSION_ID", payload: e.payload.session_id });
      }
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    }).then((u) => unlisten.push(u));

    // Error events
    listen<{ message?: string } | string>("voice-error", (e) => {
      const msg =
        typeof e.payload === "string"
          ? e.payload
          : e.payload?.message ?? "Voice session error";
      dispatch({ type: "SET_VOICE_ERROR", payload: msg });
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
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── startSession ──────────────────────────────────────────────────────────

  const startSession = useCallback(
    async (wavBytes?: number[]) => {
      const s = stateRef.current;
      interruptRef.current = false;

      try {
        let audioBytes: number[];

        if (wavBytes && wavBytes.length > 0) {
          // Caller pre-supplied audio (e.g. from wake listener)
          audioBytes = wavBytes;
        } else {
          // Record with VAD
          dispatch({ type: "SET_VOICE_STATE", payload: "recording" });
          const recorded = await invoke<number[]>("record_with_vad");
          if (!recorded?.length || interruptRef.current) {
            dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
            return;
          }
          audioBytes = recorded;
        }

        dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });

        // Load session settings
        let sessionId = s.sessionId ?? undefined;
        let authToken = s.sessionToken ?? "";

        // Refresh wake-word variants setting (non-blocking, best-effort)
        api.getSettings().catch(() => {});

        await invoke("run_voice_session", {
          wavBytes: audioBytes,
          authToken,
          sessionId: sessionId ?? null,
        });
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        dispatch({ type: "SET_VOICE_ERROR", payload: msg });
        dispatch({ type: "SET_VOICE_STATE", payload: "error" });
        setTimeout(() => {
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
          dispatch({ type: "SET_VOICE_ERROR", payload: null });
        }, 4000);
      }
    },
    [], // eslint-disable-line react-hooks/exhaustive-deps
  );

  // ── interrupt ─────────────────────────────────────────────────────────────

  const interrupt = useCallback(() => {
    interruptRef.current = true;
    // Abort any in-progress recording
    invoke("abort_recording").catch(() => {});
    // The kill switch in Rust is set by the wake listener on barge-in.
    // For an explicit interrupt from the UI, abort_recording is the
    // closest proxy — it signals the VAD loop to stop.
    dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── clearConversation ─────────────────────────────────────────────────────

  const clearConversation = useCallback(() => {
    dispatch({ type: "CLEAR_TRANSCRIPT" });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return { startSession, interrupt, clearConversation };
}
