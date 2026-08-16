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
//
// Fix (finding 17+24): "wait" now maps to "wait" so the orb shows the dedicated
// "Listening for wake word" state instead of the generic idle orb.

const CHILD_STATE_MAP: Record<string, "idle" | "wait" | "recording" | "thinking" | "speaking" | "error"> = {
  // Contract-defined server states
  wait:      "wait",       // child is waiting for wake word -> show wake-word orb
  listen:    "recording",  // child is recording user speech -> show recording orb
  thinking:  "thinking",   // child is running inference -> show thinking orb
  speak:     "speaking",   // child is playing TTS -> show speaking orb
  // Legacy compat keys (kept so any old emitter path does not break the UI)
  idle:         "idle",
  transcribing: "thinking",
  recording:    "recording",
  speaking:     "speaking",
  error:        "error",
};

// ── End-reason classification (finding 16+23) ────────────────────────────────
// "stdin_eof" and "dismissed" are clean exits the user/system intended.
// Anything else (crashed, error, unknown) with a nonzero OR null code is
// treated as abnormal.  A signal-killed child on Unix reports code=null
// and reason="crashed"; null alone must not be treated as clean.

function isCleanExit(code: number | null, reason: string): boolean {
  if (reason === "stdin_eof" || reason === "dismissed") return true;
  return false;
}

// ── Startup-failure messages ─────────────────────────────────────────────────
// `failed_to_start` means the child died before emitting `ready` — it never
// became a session. The shell attaches the child's stderr tail as `detail`,
// because that is the ONLY place the cause is written: under `--json-events`
// the child's human-facing banner macro is compiled to a no-op, so stdout
// carries nothing at all when startup fails.
//
// Without this the message was "Voice session exited (code 1, reason: crashed)"
// — true, and useless. The real line was "migration 29 was previously applied
// but is missing in the resolved migrations", i.e. a staged sidecar older than
// the database, which no exit code could ever have suggested.

/** Longest `detail` excerpt to put in a user-facing message. */
const DETAIL_MAX_CHARS = 300;

/**
 * The line worth showing from a stderr tail: the last non-blank one, which is
 * where a fatal error lands. Truncated so a stack trace cannot fill the screen.
 */
export function lastMeaningfulLine(detail: string | null | undefined): string | null {
  if (!detail) return null;
  const lines = detail.split("\n").map((l) => l.trim()).filter(Boolean);
  if (lines.length === 0) return null;
  const last = lines[lines.length - 1];
  return last.length > DETAIL_MAX_CHARS ? `${last.slice(0, DETAIL_MAX_CHARS)}…` : last;
}

/** The user-facing message for an abnormal `voice-session-ended`. */
export function endedErrorMessage(
  code: number | null,
  reason: string,
  detail?: string | null,
): string {
  if (reason !== "failed_to_start") {
    return `Voice session exited (code ${code ?? "none"}, reason: ${reason})`;
  }
  const line = lastMeaningfulLine(detail);
  return line
    ? `Voice mode could not start: ${line}`
    : `Voice mode could not start — the voice process exited during startup (code ${code ?? "none"}) without reporting a reason.`;
}

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
  /** Live mic RMS level (0-1) during wait/recording; 0 otherwise. */
  audioLevel: number;
}

// ── Hook ─────────────────────────────────────────────────────────────────────

export function useVoiceSession(): VoiceSessionAPI {
  const dispatch = useAppDispatch();

  const [sessionActive, setSessionActive] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [audioLevel, setAudioLevel] = useState(0);

  // Track the session id for stale-event filtering (finding 14-consumer).
  // Updated on startSession invoke return and on voice-ready; cleared on ended.
  const activeSessionIdRef = useRef<string | null>(null);

  // All unlisten functions from Tauri listen() registrations.
  // Populated as each promise resolves; see cancelled-flag pattern below.
  const unlistenersRef = useRef<Array<() => void>>([]);

  // Token accumulation buffer for rAF batching (finding 41).
  const pendingTokensRef = useRef<string>("");
  const rafHandleRef = useRef<number | null>(null);

  // ── flashError helper ────────────────────────────────────────────────────
  // Surfaces an error message and stays until the user dismisses it or
  // retries (VoiceMode.tsx's action bar) — no auto-clear timer. All three
  // error sites (voice-error, abnormal voice-session-ended, failed start)
  // use this.

  const flashError = useCallback((msg: string) => {
    dispatch({ type: "SET_VOICE_ERROR", payload: msg });
    dispatch({ type: "SET_VOICE_STATE", payload: "error" });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── flushPendingTokens (finding 41) ──────────────────────────────────────
  // Flush the accumulated token buffer as one APPEND_AGENT_TOKEN dispatch.

  const flushPendingTokens = useCallback(() => {
    rafHandleRef.current = null;
    if (pendingTokensRef.current.length === 0) return;
    const batch = pendingTokensRef.current;
    pendingTokensRef.current = "";
    dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: batch, done: false } });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ── Tauri event listeners ────────────────────────────────────────────────
  // All voice-* event subscriptions live HERE and nowhere else.
  // See EVENT OWNERSHIP RULE at the top of this file.

  useEffect(() => {
    // Finding 5+18: cancelled flag ensures that if the effect cleanup runs before
    // all listen() promises resolve, any unlisten functions that arrive after
    // teardown are called immediately instead of stored (preventing listener leaks
    // on StrictMode double-mount and fast mount/unmount sequences).
    let cancelled = false;
    const pending: Array<Promise<() => void>> = [];

    function register(p: Promise<() => void>): void {
      pending.push(
        p.then((unlisten) => {
          if (cancelled) {
            // Teardown already ran — call immediately to avoid a leaked listener.
            unlisten();
          } else {
            unlistenersRef.current.push(unlisten);
          }
          return unlisten;
        }),
      );
    }

    // voice-ready: child is fully initialised and entering the wait loop.
    // Emitted once per session after models are loaded.
    register(
      listen<{ session_id: string }>("voice-ready", (e) => {
        setConnecting(false);
        setSessionActive(true);
        if (e.payload?.session_id) {
          activeSessionIdRef.current = e.payload.session_id;
          dispatch({ type: "SET_SESSION_ID", payload: e.payload.session_id });
        }
        // Child enters the wake-word wait loop; show the wait orb.
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
      }),
    );

    // voice-state: wait | listen | thinking | speak (contract section 1)
    register(
      listen<string>("voice-state", (e) => {
        const mapped = CHILD_STATE_MAP[e.payload] ?? "idle";
        dispatch({ type: "SET_VOICE_STATE", payload: mapped });
      }),
    );

    // voice-transcript: confirmed user utterance post-ASR
    register(
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

    // voice-token: assistant token delta — batched via rAF (finding 41)
    register(
      listen<{ content: string }>("voice-token", (e) => {
        pendingTokensRef.current += e.payload.content;
        if (rafHandleRef.current === null) {
          rafHandleRef.current = requestAnimationFrame(flushPendingTokens);
        }
      }),
    );

    // voice-tool-call: push a context card for the in-flight tool
    register(
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

    // voice-tool-result: merge the result into the existing tool_call card by
    // matching call id (finding 18+25) instead of always pushing a new one,
    // avoiding duplicate cards. Passing `tool` lets the reducer still surface an
    // orphan result (no matching call card) as its own card rather than dropping
    // it — the normal path has the call card, so this is a safety net only.
    register(
      listen<{ tool: string; id: string; content: string }>("voice-tool-result", (e) => {
        dispatch({
          type: "UPDATE_CONTEXT_CARD",
          payload: {
            callId: e.payload.id,
            tool: e.payload.tool,
            data: { result: e.payload.content },
          },
        });
      }),
    );

    // voice-done: turn complete; flush any buffered tokens first, then mark done.
    register(
      listen<{ session_id?: string }>("voice-done", (e) => {
        // Flush any rAF-buffered tokens before marking done (finding 41).
        if (rafHandleRef.current !== null) {
          cancelAnimationFrame(rafHandleRef.current);
          flushPendingTokens();
        }
        dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });
        if (e.payload?.session_id) {
          dispatch({ type: "SET_SESSION_ID", payload: e.payload.session_id });
        }
        // Child returns to the wait loop after each completed turn.
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
      }),
    );

    // voice-error: non-fatal child error; surface via flashError (persists
    // until the user dismisses or retries).
    register(
      listen<{ message?: string } | string>("voice-error", (e) => {
        const msg =
          typeof e.payload === "string"
            ? e.payload
            : e.payload?.message ?? "Voice session error";
        flashError(msg);
      }),
    );

    // voice-audio-level: live mic RMS during wait/recording, throttled
    // Rust-side. Drives the orb's audio-reactive pulse.
    register(
      listen<{ rms: number }>("voice-audio-level", (e) => {
        setAudioLevel(e.payload.rms);
      }),
    );

    // voice-session-ended: child exited (clean or crash).
    // Finding 14-consumer: ignore events whose session_id does not match the
    // current active session, preventing a stale ended from a previous child
    // from clobbering a newly started session.
    // Finding 16+23: classify by reason, not by code, since a signal-killed
    // child on Unix reports code=null and reason="crashed" — null alone must
    // not be treated as a clean exit.
    // Finding 14-consumer also: call stop_voice_session best-effort so the
    // shell restores the wake listener when the child exits on its own.
    register(
      listen<{ code: number | null; reason: string; session_id?: string; detail?: string | null }>("voice-session-ended", (e) => {
        const incomingId = e.payload?.session_id ?? null;
        // If the event carries a session_id that does not match the active one,
        // drop it — it belongs to a previous child.
        if (incomingId !== null && activeSessionIdRef.current !== null && incomingId !== activeSessionIdRef.current) {
          return;
        }
        setSessionActive(false);
        setConnecting(false);
        setAudioLevel(0);
        activeSessionIdRef.current = null;

        // Best-effort stop so the shell restores the wake listener when the
        // child exits on its own (the shell early-return path was fixed to
        // restore the listener; this closes the loop from the frontend side).
        invoke("stop_voice_session").catch(() => {});

        const code = e.payload?.code ?? null;
        const reason = e.payload?.reason ?? "unknown";
        if (!isCleanExit(code, reason)) {
          // Abnormal exit — surface error; stays until dismissed/retried.
          flashError(endedErrorMessage(code, reason, e.payload?.detail));
        } else {
          // Clean exit (stdin_eof / dismissed) — return to idle.
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        }
      }),
    );

    return () => {
      // Mark cancelled so any still-resolving listen() promises call their
      // unlisten immediately instead of storing it (finding 5+18).
      cancelled = true;
      // Teardown all already-registered listeners.
      unlistenersRef.current.forEach((u) => u());
      unlistenersRef.current = [];
      // Cancel any in-flight rAF and flush buffered tokens (finding 41).
      if (rafHandleRef.current !== null) {
        cancelAnimationFrame(rafHandleRef.current);
        rafHandleRef.current = null;
        // Flush remaining tokens synchronously on teardown.
        if (pendingTokensRef.current.length > 0) {
          dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: pendingTokensRef.current, done: false } });
          pendingTokensRef.current = "";
        }
      }
      setAudioLevel(0);
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
      // Track for stale-ended filtering (finding 14-consumer).
      activeSessionIdRef.current = sessionId ?? null;
      // Session id is set here for optimistic UI; the voice-ready event
      // will confirm it once the child finishes loading models.
      if (sessionId) {
        dispatch({ type: "SET_SESSION_ID", payload: sessionId });
      }
      return sessionId ?? null;
    } catch (err) {
      setConnecting(false);
      const msg = err instanceof Error ? err.message : String(err);
      flashError(msg);
      return null;
    }
  }, [flashError]); // eslint-disable-line react-hooks/exhaustive-deps

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

  return { sessionActive, connecting, startSession, stopSession, clearConversation, audioLevel };
}
