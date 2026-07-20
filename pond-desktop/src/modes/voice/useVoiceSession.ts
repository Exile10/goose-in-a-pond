// ────────────────────────────────────────────────────────────
// useVoiceSession — persistent child-process voice session hook
//
// Architecture A: the Tauri shell spawns `pond-server chat --input whisper
// --json-events --session-id <uuid>` as a persistent child process that
// exclusively owns mic + speaker (wake word, VAD, ASR, TTS, barge-in all
// in-child).  The shell parses child stdout NDJSON and re-emits the Tauri
// events consumed here.
//
// EVENT OWNERSHIP RULE (enforced here, referenced in AppContext.tsx):
//   voice-ready, voice-state, voice-transcript, voice-token, voice-tool-call,
//   voice-tool-result, voice-done, voice-error, voice-session-ended are the
//   EXCLUSIVE domain of this hook when a child-process session is active.
//   AppContext.tsx listens only for the legacy per-turn events (transcript,
//   response-token, tts-start, tts-end, …) that belong to the old
//   per-turn HTTP pipeline (WebVoiceBackend / TauriVoiceBackend).  Those
//   two event families NEVER overlap: the old events are emitted by the
//   Rust audio_cmd.rs pipeline; the new voice-* events are emitted by the
//   NDJSON reader thread in the Tauri shell's chat_process.rs.
//   NO other component may register listeners for voice-* events.
// ────────────────────────────────────────────────────────────

import { useEffect, useRef, useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAppDispatch } from "../../state/AppContext";
import { nextTranscriptId, nextCardId } from "../../state/reducer";

// ── Contract state-string mapping (contract section 4) ──────────────────────
// Server child emits: "wait" | "listen" | "thinking" | "speak"
// Frontend VoiceState: "idle" | "wait" | "recording" | "thinking" | "speaking" | "error"
// Legacy compat keys kept: "idle" stays "idle", "transcribing" maps to "thinking"

const CHILD_STATE_MAP: Record<string, "idle" | "wait" | "recording" | "thinking" | "speaking" | "error"> = {
  // Contract-defined server states
  wait:      "idle",      // child is waiting for wake word -> show idle orb
  listen:    "recording", // child is recording user speech -> show recording orb
  thinking:  "thinking",  // child is running inference -> show thinking orb
  speak:     "speaking",  // child is playing TTS -> show speaking orb
  // Legacy compat keys (kept so any old emitter path doesn't break the UI)
  idle:         "idle",
  transcribing: "thinking",
  recording:    "recording",
  speaking:     "speaking",
  error:        "error",
};

// ── Public API ───────────────────────────────────────────────────────────────

export interface VoiceSessionAPI {
  /** True once voice-ready fires and until voice-session-ended fires. */
  sessionActive: boolean;
  /** True while the shell has started spawning but before voice-ready fires. */
  connecting: boolean;
  /** Start the persistent child-process session. Returns the session uuid. */
  startSession(): Promise<string | null>;
  /** Close child stdin to request clean exit; kills after 3s if still alive. */
  stopSession(): Promise<void>;
  /** Clear transcript and context cards. */
  clearConversation(): void;
}

// ── Hook ─────────────────────────────────────────────────────────────────────

export function useVoiceSession(): VoiceSessionAPI {
  const dispatch = useAppDispatch();

  const [sessionActive, setSessionActive] = useState(false);
  const [connecting, setConnecting] = useState(false);

  // Track whether we have at least one live unlisten function — if listeners
  // are not yet registered, skip teardown to avoid calling undefined functions.
  const unlistenersRef = useRef<Array<() => void>>([]);

  // ── Tauri event listeners ────────────────────────────────────────────────
  // All voice-* event subscriptions live HERE and nowhere else.
  // See EVENT OWNERSHIP RULE at the top of this file.

  useEffect(() => {
    const pending: Array<Promise<() => void>> = [];

    // voice-ready: child is fully initialised and entering the wait loop.
    // Emitted once per session after models are loaded.
    pending.push(
      listen<{ session_id: string }>("voice-ready", (e) => {
        setConnecting(false);
        setSessionActive(true);
        if (e.payload?.session_id) {
          dispatch({ type: "SET_SESSION_ID", payload: e.payload.session_id });
        }
        // Child is in the wake-word wait loop; reflect that in the UI.
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      }),
    );

    // voice-state: wait | listen | thinking | speak (contract section 1)
    pending.push(
      listen<string>("voice-state", (e) => {
        const mapped = CHILD_STATE_MAP[e.payload] ?? "idle";
        dispatch({ type: "SET_VOICE_STATE", payload: mapped });
      }),
    );

    // voice-transcript: confirmed user utterance post-ASR
    pending.push(
      listen<{ text: string }>("voice-transcript", (e) => {
        const userMsg = {
          id: nextTranscriptId(),
          role: "user" as const,
          text: e.payload.text,
          timestamp: Date.now(),
        };
        dispatch({ type: "APPEND_TRANSCRIPT", payload: userMsg });
        // Seed an empty agent message so APPEND_AGENT_TOKEN can target it.
        dispatch({
          type: "APPEND_TRANSCRIPT",
          payload: {
            id: nextTranscriptId(),
            role: "agent" as const,
            text: "",
            timestamp: Date.now(),
          },
        });
      }),
    );

    // voice-token: assistant token delta
    pending.push(
      listen<{ content: string }>("voice-token", (e) => {
        dispatch({
          type: "APPEND_AGENT_TOKEN",
          payload: { token: e.payload.content, done: false },
        });
      }),
    );

    // voice-tool-call: push a context card for the in-flight tool
    pending.push(
      listen<{ tool: string; id: string }>("voice-tool-call", (e) => {
        dispatch({
          type: "PUSH_CONTEXT_CARD",
          payload: {
            id: nextCardId(),
            tool: e.payload.tool,
            callId: e.payload.id,
            data: { id: e.payload.id },
            timestamp_ms: Date.now(),
          },
        });
      }),
    );

    // voice-tool-result: update existing card or add a result card
    pending.push(
      listen<{ tool: string; id: string; content: string }>("voice-tool-result", (e) => {
        dispatch({
          type: "PUSH_CONTEXT_CARD",
          payload: {
            id: nextCardId(),
            tool: e.payload.tool,
            callId: e.payload.id,
            data: { id: e.payload.id, result: e.payload.content },
            timestamp_ms: Date.now(),
          },
        });
      }),
    );

    // voice-done: turn complete; marks end-of-streaming for the agent message
    pending.push(
      listen<{ session_id?: string }>("voice-done", (e) => {
        dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });
        if (e.payload?.session_id) {
          dispatch({ type: "SET_SESSION_ID", payload: e.payload.session_id });
        }
        // Child returns to the wait loop after each completed turn.
        // We stay in "idle" until the next voice-state event arrives.
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      }),
    );

    // voice-error: non-fatal child error; surface to user and auto-clear
    pending.push(
      listen<{ message?: string } | string>("voice-error", (e) => {
        const msg =
          typeof e.payload === "string"
            ? e.payload
            : e.payload?.message ?? "Voice session error";
        dispatch({ type: "SET_VOICE_ERROR", payload: msg });
        dispatch({ type: "SET_VOICE_STATE", payload: "error" });
        // Auto-clear after 4 s so the user can retry.
        setTimeout(() => {
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
          dispatch({ type: "SET_VOICE_ERROR", payload: null });
        }, 4000);
      }),
    );

    // voice-session-ended: child exited (clean or crash).
    // A non-zero exit code means something went wrong; surface an error.
    pending.push(
      listen<{ code: number | null; reason: string }>("voice-session-ended", (e) => {
        setSessionActive(false);
        setConnecting(false);
        const code = e.payload?.code ?? null;
        const reason = e.payload?.reason ?? "unknown";
        if (code !== null && code !== 0) {
          // Abnormal exit — surface error and auto-clear
          const msg = `Voice session exited (code ${code}, reason: ${reason})`;
          dispatch({ type: "SET_VOICE_ERROR", payload: msg });
          dispatch({ type: "SET_VOICE_STATE", payload: "error" });
          setTimeout(() => {
            dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
            dispatch({ type: "SET_VOICE_ERROR", payload: null });
          }, 4000);
        } else {
          // Clean exit (stdin_eof / dismissed) — return to idle
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        }
      }),
    );

    // Store all unlisten functions for teardown.
    Promise.all(pending).then((fns) => {
      unlistenersRef.current = fns;
    });

    return () => {
      // Teardown: unregister all listeners.
      unlistenersRef.current.forEach((u) => u());
      unlistenersRef.current = [];
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── startSession ─────────────────────────────────────────────────────────

  const startSession = useCallback(async (): Promise<string | null> => {
    try {
      setConnecting(true);
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      // start_voice_session: stops the shell wake listener, sets the
      // VoiceChildActive flag, spawns the child, starts the stdout reader.
      // Returns the generated session uuid.
      const sessionId = await invoke<string>("start_voice_session");
      // Session id is set here for optimistic UI; the voice-ready event
      // will confirm it once the child finishes loading models.
      if (sessionId) {
        dispatch({ type: "SET_SESSION_ID", payload: sessionId });
      }
      return sessionId ?? null;
    } catch (err) {
      setConnecting(false);
      const msg = err instanceof Error ? err.message : String(err);
      dispatch({ type: "SET_VOICE_ERROR", payload: msg });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
      setTimeout(() => {
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        dispatch({ type: "SET_VOICE_ERROR", payload: null });
      }, 4000);
      return null;
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── stopSession ──────────────────────────────────────────────────────────

  const stopSession = useCallback(async (): Promise<void> => {
    try {
      // stop_voice_session: closes child stdin (triggers clean exit via
      // stdin_eof) and kills if still alive after ~3s; clears the
      // VoiceChildActive flag; restarts the shell wake listener if it
      // was running.  The voice-session-ended event will fire afterward.
      await invoke("stop_voice_session");
    } catch {
      // Non-fatal — the session-ended event will still arrive and reset state.
    } finally {
      // Optimistically reset state; the voice-session-ended event will confirm.
      setSessionActive(false);
      setConnecting(false);
      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
    }
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── clearConversation ─────────────────────────────────────────────────────

  const clearConversation = useCallback(() => {
    dispatch({ type: "CLEAR_TRANSCRIPT" });
    dispatch({ type: "CLEAR_CONTEXT_CARDS" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  return { sessionActive, connecting, startSession, stopSession, clearConversation };
}
