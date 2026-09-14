// ────────────────────────────────────────────────────────────
// useVoiceSession — unit tests
//
// Verifies the persistent child-process voice session hook against
// the Architecture A contract (voice-synthesis.json).
//
// Key assertions:
//  1. Every voice-* event maps to EXACTLY ONE reducer dispatch
//     (explicit double-dispatch regression test).
//  2. The state-string mapping table is correct.
//  3. start/stop session lifecycle (invoke calls + state transitions).
//  4. voice-error and voice-session-ended with non-zero code surface errors.
//  5. voice-session-ended with code 0 / clean reason returns to idle cleanly.
//  6. Listener teardown race: listeners resolved after unmount are unregistered.
//  7. Double-mount (StrictMode) results in a live session (start/stop/start).
//  8. Stale session_id filtering on voice-session-ended.
//  9. Reason-based exit classification (code null + reason "crashed" = abnormal).
// 10. voice-tool-result dispatches UPDATE_CONTEXT_CARD (not PUSH_CONTEXT_CARD).
// 11. voice-token events are batched via rAF accumulator.
// ────────────────────────────────────────────────────────────

import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import React from "react";

// ── Shared mutable stores for the shell-bridge mock ─────────────────────────

// event name -> array of registered handler functions
const _listeners: Record<string, Array<(payload: unknown) => void>> = {};

// Stored invoke mock
let _invoke: Mock;

// Emit a fake shell event to all registered listeners for that event name.
// The bridge hands handlers the payload directly rather than an envelope.
function emitShellEvent(name: string, payload: unknown): void {
  for (const h of _listeners[name] ?? []) h(payload);
}

// ── Mock requestAnimationFrame / cancelAnimationFrame ──────────────────────
// In tests, rAF batching must be flushed synchronously via fake timers or
// by calling the rAF callback directly.  We replace rAF with an immediate
// synchronous call so batching tests are deterministic without fake timers.

const _rafCallbacks: Map<number, FrameRequestCallback> = new Map();
let _rafNextHandle = 1;

vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback): number => {
  const handle = _rafNextHandle++;
  _rafCallbacks.set(handle, cb);
  // Do NOT call synchronously here — caller must flush via flushRaf()
  return handle;
});

vi.stubGlobal("cancelAnimationFrame", (handle: number): void => {
  _rafCallbacks.delete(handle);
});

function flushRaf(): void {
  const now = performance.now();
  for (const [handle, cb] of Array.from(_rafCallbacks.entries())) {
    _rafCallbacks.delete(handle);
    cb(now);
  }
}

// ── Mock the shell bridge ────────────────────────────────────────────────────
//
// listen() is synchronous and returns its unsubscribe function directly, so
// the mock has no promise machinery. The deferred-resolution switch that used
// to live here existed solely to exercise a teardown race that Tauri's async
// registration made possible; see the leak test below for what replaced it.

vi.mock("../../shell", () => ({
  get invoke() { return _invoke; },
  isDesktopShell: () => true,
  listen: vi.fn((name: string, handler: (payload: unknown) => void) => {
    if (!_listeners[name]) _listeners[name] = [];
    _listeners[name].push(handler);
    return () => {
      _listeners[name] = (_listeners[name] ?? []).filter((h) => h !== handler);
    };
  }),
}));

// ── Mock AppContext ──────────────────────────────────────────────────────────

const _dispatched: Array<{ type: string; payload?: unknown }> = [];
const _dispatch = vi.fn((action: { type: string; payload?: unknown }) => {
  _dispatched.push(action);
});

// The conversation the chat view is on. `startSession` passes it to the child
// so voice continues that conversation instead of starting a new one.
let _appSessionId: string | null = null;

vi.mock("../../state/AppContext", () => ({
  useAppDispatch: () => _dispatch,
  useAppState: () => ({
    serverOnline: true,
    serverUrl: "http://127.0.0.1:4000",
    sessionToken: "tok",
    get sessionId() {
      return _appSessionId;
    },
    voiceState: "idle",
    voiceError: null,
    transcript: [],
    contextCards: [],
  }),
}));

// ── Mock reducer helpers ─────────────────────────────────────────────────────

let _nextId = 0;
vi.mock("../../state/reducer", () => ({
  nextTranscriptId: vi.fn(() => ++_nextId),
  nextCardId: vi.fn(() => ++_nextId),
}));

// ── Helpers ──────────────────────────────────────────────────────────────────

function dispatchedOfType(type: string) {
  return _dispatched.filter((a) => a.type === type);
}

function clearDispatched() {
  _dispatched.length = 0;
}

// ── Import the hook under test (after mocks are in place) ────────────────────

// We import dynamically AFTER vi.mock so the mocks resolve first.
async function getHook() {
  const mod = await import("./useVoiceSession");
  return mod.useVoiceSession;
}

// Minimal wrapper so renderHook can access context (mocked above)
function wrapper({ children }: { children: React.ReactNode }) {
  return React.createElement(React.Fragment, null, children);
}

// ── Tests ────────────────────────────────────────────────────────────────────

describe("useVoiceSession — state-string mapping", () => {
  it("maps contract state strings to VoiceState correctly", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("test-session-id");

    renderHook(() => useVoiceSession(), { wrapper });
    // Let effect run (listeners register)
    await act(async () => { await Promise.resolve(); });

    clearDispatched();

    // Contract: wait -> wait (wake-word listening state, not idle)
    act(() => { emitShellEvent("voice-state", "wait"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("wait");

    // Contract: listen -> recording
    act(() => { emitShellEvent("voice-state", "listen"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("recording");

    // Contract: thinking -> thinking
    act(() => { emitShellEvent("voice-state", "thinking"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("thinking");

    // Contract: speak -> speaking
    act(() => { emitShellEvent("voice-state", "speak"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("speaking");

    // Legacy compat: idle -> idle
    act(() => { emitShellEvent("voice-state", "idle"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("idle");

    // Legacy compat: transcribing -> thinking
    act(() => { emitShellEvent("voice-state", "transcribing"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("thinking");

    cleanup();
  });

  it("voice-ready dispatches SET_VOICE_STATE 'wait' (not 'idle')", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s0");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-ready", { session_id: "s0" }); });

    const stateActions = dispatchedOfType("SET_VOICE_STATE");
    expect(stateActions.length).toBeGreaterThan(0);
    // voice-ready should put the orb in the wake-word wait state
    expect(stateActions.at(-1)?.payload).toBe("wait");

    cleanup();
  });
});

describe("useVoiceSession — start/stop lifecycle", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("startSession invokes start_voice_session and dispatches SET_SESSION_ID", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("abc-123");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    let sessionId: string | null = null;
    await act(async () => {
      sessionId = await result.current.startSession();
    });

    // No chat has happened, so there is nothing to continue — the child mints
    // its own id and returns it.
    expect(_invoke).toHaveBeenCalledWith("start_voice_session", {
      sessionId: null,
    });
    expect(sessionId).toBe("abc-123");
    const sessionIdActions = dispatchedOfType("SET_SESSION_ID");
    expect(sessionIdActions.length).toBeGreaterThan(0);
    expect(sessionIdActions[0].payload).toBe("abc-123");
  });

  it("startSession continues the conversation the chat view is on", async () => {
    // The whole point of the change: speaking after typing must reach the same
    // agent mid-conversation, not a stranger. The child resolves this GIAP
    // session to its Goose session and hydrates it with the history.
    const useVoiceSession = await getHook();
    _appSessionId = "chat-session-7";
    // The child echoes back whichever id it used, which is this one.
    _invoke = vi.fn().mockResolvedValue("chat-session-7");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    let sessionId: string | null = null;
    await act(async () => {
      sessionId = await result.current.startSession();
    });

    expect(_invoke).toHaveBeenCalledWith("start_voice_session", {
      sessionId: "chat-session-7",
    });
    expect(sessionId).toBe("chat-session-7");
    _appSessionId = null;
  });

  it("stopSession invokes stop_voice_session and resets state", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn()
      .mockResolvedValueOnce("sess-1") // start_voice_session
      .mockResolvedValueOnce(undefined); // stop_voice_session

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    await act(async () => { await result.current.startSession(); });

    clearDispatched();
    await act(async () => { await result.current.stopSession(); });

    expect(_invoke).toHaveBeenCalledWith("stop_voice_session");
    const stateActions = dispatchedOfType("SET_VOICE_STATE");
    expect(stateActions.some((a) => a.payload === "idle")).toBe(true);
  });

  it("startSession handles invoke rejection by dispatching a persistent error", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockRejectedValue(new Error("spawn failed"));

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    await act(async () => { await result.current.startSession().catch(() => {}); });

    expect(dispatchedOfType("SET_VOICE_ERROR").length).toBeGreaterThan(0);
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "error")).toBe(true);

    // startSession's own optimistic "idle" dispatch fires once before the
    // invoke rejects — but with no auto-clear timer, nothing should reset
    // voiceState/voiceError back to idle/null afterward.
    const idleCount = dispatchedOfType("SET_VOICE_STATE").filter((a) => a.payload === "idle").length;
    expect(idleCount).toBe(1);
    expect(dispatchedOfType("SET_VOICE_ERROR").some((a) => a.payload === null)).toBe(false);

    cleanup();
  });
});

describe("useVoiceSession — StrictMode double-mount (finding 4+13)", () => {
  // The actual symmetric mount/cleanup is in VoiceMode.tsx's useEffect
  // (which calls session.startSession() and returns session.stopSession()).
  // The hook itself does NOT block startSession calls — there is no startedRef
  // guard in the hook.  We verify that calling startSession twice works
  // (simulating the StrictMode mount#1 -> cleanup -> mount#2 sequence that
  // VoiceMode.tsx drives).

  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("startSession can be called after stopSession (start/stop/start sequence is not blocked)", async () => {
    const useVoiceSession = await getHook();
    let callCount = 0;
    _invoke = vi.fn().mockImplementation((cmd: string) => {
      if (cmd === "start_voice_session") {
        callCount++;
        return Promise.resolve(`sess-${callCount}`);
      }
      if (cmd === "stop_voice_session") return Promise.resolve(undefined);
      return Promise.resolve(undefined);
    });

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    // Simulate mount #1: start
    await act(async () => { await result.current.startSession(); });
    expect(callCount).toBe(1);

    // Simulate StrictMode cleanup: stop
    await act(async () => { await result.current.stopSession(); });

    // Simulate mount #2: start again — must not be blocked by any ref guard
    await act(async () => { await result.current.startSession(); });
    expect(callCount).toBe(2);

    // The second startSession dispatched a new SET_SESSION_ID
    const sessionIds = dispatchedOfType("SET_SESSION_ID");
    expect(sessionIds.length).toBeGreaterThanOrEqual(2);
    expect(sessionIds.at(-1)?.payload).toBe("sess-2");
  });
});

describe("useVoiceSession — listener teardown race (finding 5+18)", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  // What this used to test: an unlisten function resolving AFTER teardown,
  // which Tauri's async listen() made possible and which the hook guarded with
  // a cancelled flag. Synchronous registration makes that unrepresentable, so
  // the bug class is gone rather than merely tested for. The invariant it was
  // protecting -- unmounting leaves nothing registered -- is still worth
  // pinning, and now it can be asserted directly.
  it("leaves no listener registered after unmount", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("sess-race");

    const { unmount } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    const whileMounted = Object.values(_listeners).reduce((n, a) => n + a.length, 0);
    expect(whileMounted).toBeGreaterThan(0); // control: the test can fail

    unmount();
    await act(async () => { await Promise.resolve(); });

    const afterUnmount = Object.values(_listeners).reduce((n, a) => n + a.length, 0);
    expect(afterUnmount).toBe(0);
  });
});

describe("useVoiceSession — double-dispatch regression", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("voice-transcript dispatches APPEND_TRANSCRIPT EXACTLY TWICE (user + agent seed)", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-transcript", { text: "hello world" }); });

    const appendActions = dispatchedOfType("APPEND_TRANSCRIPT");
    // Exactly 2: one user message + one agent seed message
    expect(appendActions).toHaveLength(2);
    expect(appendActions[0].payload).toMatchObject({ role: "user", text: "hello world" });
    expect(appendActions[1].payload).toMatchObject({ role: "agent", text: "" });
  });

  it("voice-token batches via rAF — two tokens emit one APPEND_AGENT_TOKEN per flush", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-token", { content: "hello" }); });
    act(() => { emitShellEvent("voice-token", { content: " world" }); });

    // Before flushing rAF, dispatch should not have fired yet
    expect(dispatchedOfType("APPEND_AGENT_TOKEN")).toHaveLength(0);

    // Flush the rAF
    act(() => { flushRaf(); });

    const tokenActions = dispatchedOfType("APPEND_AGENT_TOKEN");
    // Both tokens coalesced into ONE dispatch with combined text
    expect(tokenActions).toHaveLength(1);
    expect(tokenActions[0].payload).toMatchObject({ token: "hello world", done: false });
  });

  it("voice-done flushes pending tokens before marking done", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    // Send a token then immediately send done without flushing rAF
    act(() => { emitShellEvent("voice-token", { content: "last" }); });
    act(() => { emitShellEvent("voice-done", { session_id: "sess-abc" }); });

    const tokenActions = dispatchedOfType("APPEND_AGENT_TOKEN");
    // Should have: one batch flush of "last" AND one done dispatch
    const batchAction = tokenActions.find((a) => (a.payload as { token: string }).token === "last");
    const doneAction = tokenActions.find((a) => (a.payload as { done: boolean }).done === true);
    expect(batchAction).toBeDefined();
    expect(doneAction).toBeDefined();
  });

  it("voice-tool-call dispatches PUSH_CONTEXT_CARD EXACTLY ONCE", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-tool-call", { tool: "giap__weather", id: "call-1" }); });

    const cardActions = dispatchedOfType("PUSH_CONTEXT_CARD");
    expect(cardActions).toHaveLength(1);
    expect(cardActions[0].payload).toMatchObject({
      tool: "giap__weather",
      callId: "call-1",
    });
  });

  it("voice-tool-result dispatches UPDATE_CONTEXT_CARD (not PUSH_CONTEXT_CARD)", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => {
      emitShellEvent("voice-tool-result", {
        tool: "giap__weather",
        id: "call-1",
        content: "Temperature: 22C",
      });
    });

    // Must NOT push a new card
    expect(dispatchedOfType("PUSH_CONTEXT_CARD")).toHaveLength(0);
    // Must UPDATE the existing card by callId
    const updateActions = dispatchedOfType("UPDATE_CONTEXT_CARD");
    expect(updateActions).toHaveLength(1);
    expect(updateActions[0].payload).toMatchObject({
      callId: "call-1",
      data: { result: "Temperature: 22C" },
    });
  });

  it("voice-done dispatches APPEND_AGENT_TOKEN(done) + SET_SESSION_ID + SET_VOICE_STATE EXACTLY ONCE each", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-done", { session_id: "sess-abc" }); });

    // Flush any rAF
    act(() => { flushRaf(); });

    expect(dispatchedOfType("APPEND_AGENT_TOKEN")).toHaveLength(1);
    expect(dispatchedOfType("APPEND_AGENT_TOKEN")[0].payload).toMatchObject({ done: true });
    expect(dispatchedOfType("SET_SESSION_ID")).toHaveLength(1);
    expect(dispatchedOfType("SET_SESSION_ID")[0].payload).toBe("sess-abc");
    expect(dispatchedOfType("SET_VOICE_STATE")).toHaveLength(1);
    // voice-done now returns to "wait" (wake-word state) not "idle"
    expect(dispatchedOfType("SET_VOICE_STATE")[0].payload).toBe("wait");
  });

  it("voice-error dispatches SET_VOICE_ERROR + SET_VOICE_STATE(error) EXACTLY ONCE each", async () => {
    vi.useFakeTimers();
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-error", { message: "mic not found" }); });

    expect(dispatchedOfType("SET_VOICE_ERROR")).toHaveLength(1);
    expect(dispatchedOfType("SET_VOICE_ERROR")[0].payload).toBe("mic not found");
    expect(dispatchedOfType("SET_VOICE_STATE").filter((a) => a.payload === "error")).toHaveLength(1);

    vi.useRealTimers();
    cleanup();
  });
});

describe("useVoiceSession — voice-session-ended handling", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("voice-session-ended with reason stdin_eof transitions to idle without error", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    // Simulate session active with a known session_id
    act(() => { emitShellEvent("voice-ready", { session_id: "s1" }); });
    expect(result.current.sessionActive).toBe(true);

    clearDispatched();
    act(() => {
      emitShellEvent("voice-session-ended", { code: 0, reason: "stdin_eof", session_id: "s1" });
    });

    expect(result.current.sessionActive).toBe(false);
    expect(result.current.connecting).toBe(false);
    // No error dispatched
    expect(dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null)).toHaveLength(0);
    // Returns to idle
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(true);
  });

  it("voice-session-ended with reason dismissed transitions to idle without error", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s2");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    act(() => { emitShellEvent("voice-ready", { session_id: "s2" }); });

    clearDispatched();
    act(() => {
      emitShellEvent("voice-session-ended", { code: 0, reason: "dismissed", session_id: "s2" });
    });

    expect(result.current.sessionActive).toBe(false);
    expect(dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null)).toHaveLength(0);
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(true);
  });

  it("voice-session-ended with non-zero code surfaces a persistent error", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    act(() => { emitShellEvent("voice-ready", { session_id: "s1" }); });
    expect(result.current.sessionActive).toBe(true);

    clearDispatched();
    act(() => {
      emitShellEvent("voice-session-ended", { code: 1, reason: "error", session_id: "s1" });
    });

    expect(result.current.sessionActive).toBe(false);
    const errorActions = dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null);
    expect(errorActions).toHaveLength(1);
    expect(errorActions[0].payload).toMatch(/code 1/);
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "error")).toBe(true);

    // No auto-clear timer anymore — stays in error state until dismissed/retried.
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(false);

    cleanup();
  });

  it("voice-session-ended with code null and reason crashed is abnormal (signal kill)", async () => {
    // Finding 16+23: Unix signal-killed child reports code=null, reason="crashed"
    // This must be treated as abnormal, not clean.
    vi.useFakeTimers();
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s3");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    act(() => { emitShellEvent("voice-ready", { session_id: "s3" }); });

    clearDispatched();
    act(() => {
      emitShellEvent("voice-session-ended", { code: null, reason: "crashed", session_id: "s3" });
    });

    expect(result.current.sessionActive).toBe(false);
    // code=null + reason=crashed must surface an error, not silently go to idle
    const errorActions = dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null);
    expect(errorActions).toHaveLength(1);
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "error")).toBe(true);

    vi.useRealTimers();
    cleanup();
  });

  it("stale session-ended event is ignored when a new session is active (finding 14-consumer)", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn()
      .mockResolvedValueOnce("session-A") // first start
      .mockResolvedValue(undefined); // stop + any subsequent

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    // Start session A
    await act(async () => { await result.current.startSession(); });
    act(() => { emitShellEvent("voice-ready", { session_id: "session-A" }); });
    expect(result.current.sessionActive).toBe(true);

    // Stop session A and immediately start session B
    await act(async () => { await result.current.stopSession(); });
    _invoke = vi.fn()
      .mockResolvedValueOnce("session-B")
      .mockResolvedValue(undefined);
    await act(async () => { await result.current.startSession(); });
    act(() => { emitShellEvent("voice-ready", { session_id: "session-B" }); });
    expect(result.current.sessionActive).toBe(true);

    clearDispatched();

    // Now deliver a stale ended event for session-A (old child finally reaped)
    act(() => {
      emitShellEvent("voice-session-ended", {
        code: 1,
        reason: "crashed",
        session_id: "session-A", // stale — does not match current "session-B"
      });
    });

    // The event should have been ignored — no state changes for session B
    expect(result.current.sessionActive).toBe(true);
    expect(dispatchedOfType("SET_VOICE_STATE")).toHaveLength(0);
    expect(dispatchedOfType("SET_VOICE_ERROR")).toHaveLength(0);
  });

  it("voice-ready sets sessionActive true and dispatches SET_SESSION_ID", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitShellEvent("voice-ready", { session_id: "ready-sess" }); });

    expect(result.current.sessionActive).toBe(true);
    expect(result.current.connecting).toBe(false);
    expect(dispatchedOfType("SET_SESSION_ID")[0].payload).toBe("ready-sess");
    // voice-ready puts the orb in the wake-word wait state
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "wait")).toBe(true);
  });
});

describe("useVoiceSession — audio-level reactivity", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("voice-audio-level updates audioLevel", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    expect(result.current.audioLevel).toBe(0);

    act(() => { emitShellEvent("voice-audio-level", { rms: 0.37 }); });
    expect(result.current.audioLevel).toBe(0.37);
  });

  it("voice-session-ended resets audioLevel to 0", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    act(() => { emitShellEvent("voice-ready", { session_id: "s1" }); });
    act(() => { emitShellEvent("voice-audio-level", { rms: 0.5 }); });
    expect(result.current.audioLevel).toBe(0.5);

    act(() => {
      emitShellEvent("voice-session-ended", { code: null, reason: "stdin_eof", session_id: "s1" });
    });
    expect(result.current.audioLevel).toBe(0);
  });
});

describe("useVoiceSession — flashError persistence", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("overlapping errors both surface and neither auto-clears", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();

    // Fire two errors in quick succession
    act(() => { emitShellEvent("voice-error", { message: "error 1" }); });
    act(() => { emitShellEvent("voice-error", { message: "error 2" }); });

    const errorPayloads = dispatchedOfType("SET_VOICE_ERROR").map((a) => a.payload);
    expect(errorPayloads).toEqual(["error 1", "error 2"]);
    // No timer of any kind clears the state — it stays in error.
    const idleActions = dispatchedOfType("SET_VOICE_STATE").filter((a) => a.payload === "idle");
    expect(idleActions).toHaveLength(0);
  });
});

describe("useVoiceSession — full contract event sequence", () => {
  beforeEach(() => {
    clearDispatched();
    for (const key of Object.keys(_listeners)) {
      delete _listeners[key];
    }
    _nextId = 0;
  });

  afterEach(() => {
    cleanup();
  });

  it("replays contract sequence and verifies dispatch ordering", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("seq-session");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    await act(async () => { await result.current.startSession(); });

    clearDispatched();

    // Contract sequence: ready -> state wait -> state listen -> transcript ->
    // state thinking -> tokens -> tool_call/tool_result -> state speak ->
    // turn_complete -> state wait
    act(() => { emitShellEvent("voice-ready", { session_id: "seq-session" }); });
    act(() => { emitShellEvent("voice-state", "wait"); });
    act(() => { emitShellEvent("voice-state", "listen"); });
    act(() => { emitShellEvent("voice-transcript", { text: "what is the weather" }); });
    act(() => { emitShellEvent("voice-state", "thinking"); });
    act(() => { emitShellEvent("voice-token", { content: "The" }); });
    act(() => { emitShellEvent("voice-token", { content: " weather" }); });
    act(() => { emitShellEvent("voice-tool-call", { tool: "giap__weather", id: "c1" }); });
    act(() => { emitShellEvent("voice-tool-result", { tool: "giap__weather", id: "c1", content: "22C" }); });
    act(() => { emitShellEvent("voice-state", "speak"); });
    act(() => { emitShellEvent("voice-done", { session_id: "seq-session" }); });
    // voice-done flushes tokens and dispatches wait state
    act(() => { emitShellEvent("voice-state", "wait"); });

    // Verify state transitions
    const stateActions = dispatchedOfType("SET_VOICE_STATE").map((a) => a.payload);
    expect(stateActions).toContain("wait");     // from voice-ready + voice-state wait + voice-done
    expect(stateActions).toContain("recording"); // from voice-state listen
    expect(stateActions).toContain("thinking");  // from voice-state thinking
    expect(stateActions).toContain("speaking");  // from voice-state speak

    // Transcript was appended once (user + agent seed = 2 dispatches from one event)
    const transcriptActions = dispatchedOfType("APPEND_TRANSCRIPT");
    expect(transcriptActions).toHaveLength(2);
    expect(transcriptActions[0].payload).toMatchObject({ role: "user" });

    // Two tokens dispatched as one batched APPEND_AGENT_TOKEN (rAF coalesced by voice-done flush)
    const tokenActions = dispatchedOfType("APPEND_AGENT_TOKEN").filter(
      (a) => (a.payload as { done: boolean }).done === false,
    );
    // The two tokens "The" + " weather" are flushed together by voice-done
    expect(tokenActions).toHaveLength(1);
    expect((tokenActions[0].payload as { token: string }).token).toBe("The weather");

    // One tool-call card pushed
    expect(dispatchedOfType("PUSH_CONTEXT_CARD")).toHaveLength(1);
    // One tool-result update (not push)
    expect(dispatchedOfType("UPDATE_CONTEXT_CARD")).toHaveLength(1);

    // turn_complete: APPEND_AGENT_TOKEN(done) + SET_SESSION_ID + SET_VOICE_STATE(wait)
    const doneActions = dispatchedOfType("APPEND_AGENT_TOKEN").filter(
      (a) => (a.payload as { done: boolean }).done === true,
    );
    expect(doneActions).toHaveLength(1);
  });
});

// ── Startup failure: the child's own reason must reach the user ──────────────
//
// Regression: a sidecar staged weeks before the database was migrated exited 1
// having emitted ZERO NDJSON lines. The shell called that "crashed" and the UI
// said "Voice session exited (code 1, reason: crashed)" — true, and useless.
// The actual cause was on the child's stderr, which the shell logged at debug.

describe("useVoiceSession — failed_to_start", () => {
  it("surfaces the child's stderr reason instead of an exit code", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s-fail");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    // No voice-ready: the child died during startup.
    clearDispatched();
    act(() => {
      emitShellEvent("voice-session-ended", {
        code: 1,
        reason: "failed_to_start",
        session_id: null,
        detail:
          "  Goose in a Pond 0.1.0 — voice\n" +
          "Error: migration 29 was previously applied but is missing in the resolved migrations",
      });
    });

    const errors = dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null);
    expect(errors).toHaveLength(1);
    const msg = String(errors[0].payload);
    // The cause, not the exit code, is what tells anyone what to do next.
    expect(msg).toContain("migration 29");
    expect(msg).not.toContain("reason: failed_to_start");

    cleanup();
  });

  it("says so plainly when the child died without explaining itself", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s-quiet");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => {
      emitShellEvent("voice-session-ended", {
        code: 1,
        reason: "failed_to_start",
        session_id: null,
        detail: null,
      });
    });

    const errors = dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null);
    expect(errors).toHaveLength(1);
    // Absent detail must not render as a message that trails off into nothing.
    expect(String(errors[0].payload)).toContain("without reporting a reason");

    cleanup();
  });
});
