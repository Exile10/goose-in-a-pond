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
import { invoke, listen } from "../../shell";
import { useAppDispatch, useAppState } from "../../state/AppContext";
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
  /**
   * True from voice-warmup "warming" until its terminal state — the stretch
   * where the child is alive but the model is still loading and the prompt
   * prefix precompiling. The child speaks "Warming up." at the start and
   * greets by name when done; this flag lets the orb say it visually too.
   */
  warmingUp: boolean;
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

  // The conversation the chat view is on, read at call time.
  //
  // Held in a ref rather than put in `startSession`'s dependency array so the
  // callback keeps a stable identity: it is depended on by effects elsewhere,
  // and re-creating it whenever the session id changed would re-run them.
  //
  // `useAppState()` subscribes this hook to the whole app state, so it now
  // re-renders on every `APPEND_AGENT_TOKEN` batch too. Accepted: it already
  // re-renders at roughly 30 Hz from the local `audioLevel` state while
  // listening, so the token batches are not the thing driving this component.
  const appSessionId = useAppState().sessionId;
  const appSessionIdRef = useRef<string | null>(appSessionId);
  appSessionIdRef.current = appSessionId;

  const [sessionActive, setSessionActive] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [warmingUp, setWarmingUp] = useState(false);
  const [audioLevel, setAudioLevel] = useState(0);

  // Track the session id for stale-event filtering (finding 14-consumer).
  // Updated on startSession invoke return and on voice-ready; cleared on ended.
  const activeSessionIdRef = useRef<string | null>(null);

  // Unsubscribe functions for every voice-* listener this hook owns.
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

  // ── Shell event listeners ────────────────────────────────────────────────
  // All voice-* event subscriptions live HERE and nowhere else.
  // See EVENT OWNERSHIP RULE at the top of this file.

  useEffect(() => {
    // Registration is synchronous, so there is nothing to race: an unlisten
    // cannot arrive after teardown, and no event can be missed between mount
    // and subscription. The cancelled-flag register that used to live here
    // existed only because Tauri's listen() round-tripped into Rust.

    // voice-warmup: warming | ready | skipped | failed. Precedes voice-ready.
    unlistenersRef.current.push(
      listen("voice-warmup", (payload) => {
        setWarmingUp(payload === "warming");
      }),
    );

    // voice-ready: child is fully initialised and entering the wait loop.
    // Emitted once per session after models are loaded.
    unlistenersRef.current.push(
      listen("voice-ready", (payload) => {
        setConnecting(false);
        setWarmingUp(false);
        setSessionActive(true);
        if (payload?.session_id) {
          activeSessionIdRef.current = payload.session_id;
          dispatch({ type: "SET_SESSION_ID", payload: payload.session_id });
        }
        // Child enters the wake-word wait loop; show the wait orb.
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
      }),
    );

    // voice-state: wait | listen | thinking | speak (contract section 1)
    unlistenersRef.current.push(
      listen("voice-state", (payload) => {
        const mapped = CHILD_STATE_MAP[payload] ?? "idle";
        dispatch({ type: "SET_VOICE_STATE", payload: mapped });
      }),
    );

    // voice-transcript: confirmed user utterance post-ASR
    unlistenersRef.current.push(
      listen("voice-transcript", (payload) => {
        const userMsg = {
          id: nextTranscriptId(),
          role: "user" as const,
          text: payload.text,
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
    unlistenersRef.current.push(
      listen("voice-token", (payload) => {
        pendingTokensRef.current += payload.content;
        if (rafHandleRef.current === null) {
          rafHandleRef.current = requestAnimationFrame(flushPendingTokens);
        }
      }),
    );

    // voice-tool-call: push a context card for the in-flight tool
    unlistenersRef.current.push(
      listen("voice-tool-call", (payload) => {
        dispatch({
          type: "PUSH_CONTEXT_CARD",
          payload: {
            id: nextCardId(),
            tool: payload.tool,
            callId: payload.id,
            data: { id: payload.id },
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
    unlistenersRef.current.push(
      listen("voice-tool-result", (payload) => {
        dispatch({
          type: "UPDATE_CONTEXT_CARD",
          payload: {
            callId: payload.id,
            tool: payload.tool,
            data: { result: payload.content },
          },
        });
      }),
    );

    // voice-done: turn complete; flush any buffered tokens first, then mark done.
    unlistenersRef.current.push(
      listen("voice-done", (payload) => {
        // Flush any rAF-buffered tokens before marking done (finding 41).
        if (rafHandleRef.current !== null) {
          cancelAnimationFrame(rafHandleRef.current);
          flushPendingTokens();
        }
        dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });
        if (payload?.session_id) {
          dispatch({ type: "SET_SESSION_ID", payload: payload.session_id });
        }
        // Child returns to the wait loop after each completed turn.
        dispatch({ type: "SET_VOICE_STATE", payload: "wait" });
      }),
    );

    // voice-error: non-fatal child error; surface via flashError (persists
    // until the user dismisses or retries).
    unlistenersRef.current.push(
      listen("voice-error", (payload) => {
        const msg =
          typeof payload === "string"
            ? payload
            : payload?.message ?? "Voice session error";
        flashError(msg);
      }),
    );

    // voice-audio-level: live mic RMS during wait/recording, throttled
    // Rust-side. Drives the orb's audio-reactive pulse.
    unlistenersRef.current.push(
      listen("voice-audio-level", (payload) => {
        setAudioLevel(payload.rms);
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
    unlistenersRef.current.push(
      listen("voice-session-ended", (payload) => {
        const incomingId = payload?.session_id ?? null;
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

        const code = payload?.code ?? null;
        const reason = payload?.reason ?? "unknown";
        if (!isCleanExit(code, reason)) {
          // Abnormal exit — surface error; stays until dismissed/retried.
          flashError(endedErrorMessage(code, reason, payload?.detail));
        } else {
          // Clean exit (stdin_eof / dismissed) — return to idle.
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        }
      }),
    );

    return () => {
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
      //
      // Passing the session the chat view is on makes voice a continuation of
      // that conversation rather than a new one — same history, same Goose
      // engine session, same agent mid-thought. `null` (no chat has happened
      // yet) still starts fresh, and the child returns whichever id it used.
      const sessionId = await invoke("start_voice_session", {
        sessionId: appSessionIdRef.current,
      });
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

  return { sessionActive, connecting,
    warmingUp, startSession, stopSession, clearConversation, audioLevel };
}
