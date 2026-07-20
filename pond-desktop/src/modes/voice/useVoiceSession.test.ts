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
//  5. voice-session-ended with code 0 returns to idle cleanly.
// ────────────────────────────────────────────────────────────

import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import React from "react";

// ── Shared mutable stores for the Tauri mock ────────────────────────────────

// event name -> array of registered handler functions
const _listeners: Record<string, Array<(e: { payload: unknown }) => void>> = {};

// Stored invoke mock
let _invoke: Mock;

// Emit a fake Tauri event to all registered listeners for that event name.
function emitTauriEvent(name: string, payload: unknown): void {
  const handlers = _listeners[name] ?? [];
  for (const h of handlers) {
    h({ payload });
  }
}

// ── Mock @tauri-apps/api/core ────────────────────────────────────────────────

vi.mock("@tauri-apps/api/core", () => ({
  get invoke() { return _invoke; },
}));

// ── Mock @tauri-apps/api/event ───────────────────────────────────────────────

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((name: string, handler: (e: { payload: unknown }) => void) => {
    if (!_listeners[name]) _listeners[name] = [];
    _listeners[name].push(handler);
    // Return a Promise that resolves to an unlisten function
    return Promise.resolve(() => {
      _listeners[name] = (_listeners[name] ?? []).filter((h) => h !== handler);
    });
  }),
}));

// ── Mock AppContext ──────────────────────────────────────────────────────────

const _dispatched: Array<{ type: string; payload?: unknown }> = [];
const _dispatch = vi.fn((action: { type: string; payload?: unknown }) => {
  _dispatched.push(action);
});

vi.mock("../../state/AppContext", () => ({
  useAppDispatch: () => _dispatch,
  useAppState: () => ({
    serverOnline: true,
    serverUrl: "http://127.0.0.1:4000",
    sessionToken: "tok",
    sessionId: null,
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

    // Contract: wait -> idle
    act(() => { emitTauriEvent("voice-state", "wait"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("idle");

    // Contract: listen -> recording
    act(() => { emitTauriEvent("voice-state", "listen"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("recording");

    // Contract: thinking -> thinking
    act(() => { emitTauriEvent("voice-state", "thinking"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("thinking");

    // Contract: speak -> speaking
    act(() => { emitTauriEvent("voice-state", "speak"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("speaking");

    // Legacy compat: idle -> idle
    act(() => { emitTauriEvent("voice-state", "idle"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("idle");

    // Legacy compat: transcribing -> thinking
    act(() => { emitTauriEvent("voice-state", "transcribing"); });
    expect(dispatchedOfType("SET_VOICE_STATE").at(-1)?.payload).toBe("thinking");

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

    expect(_invoke).toHaveBeenCalledWith("start_voice_session");
    expect(sessionId).toBe("abc-123");
    const sessionIdActions = dispatchedOfType("SET_SESSION_ID");
    expect(sessionIdActions.length).toBeGreaterThan(0);
    expect(sessionIdActions[0].payload).toBe("abc-123");
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

  it("startSession handles invoke rejection by dispatching error and auto-clearing", async () => {
    vi.useFakeTimers();
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockRejectedValue(new Error("spawn failed"));

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    await act(async () => { await result.current.startSession().catch(() => {}); });

    expect(dispatchedOfType("SET_VOICE_ERROR").length).toBeGreaterThan(0);
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "error")).toBe(true);

    // After 4000ms the error should auto-clear
    act(() => { vi.advanceTimersByTime(4100); });
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(true);
    expect(dispatchedOfType("SET_VOICE_ERROR").some((a) => a.payload === null)).toBe(true);

    vi.useRealTimers();
    cleanup();
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
    act(() => { emitTauriEvent("voice-transcript", { text: "hello world" }); });

    const appendActions = dispatchedOfType("APPEND_TRANSCRIPT");
    // Exactly 2: one user message + one agent seed message
    expect(appendActions).toHaveLength(2);
    expect(appendActions[0].payload).toMatchObject({ role: "user", text: "hello world" });
    expect(appendActions[1].payload).toMatchObject({ role: "agent", text: "" });
  });

  it("voice-token dispatches APPEND_AGENT_TOKEN EXACTLY ONCE per token", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitTauriEvent("voice-token", { content: "hello" }); });
    act(() => { emitTauriEvent("voice-token", { content: " world" }); });

    const tokenActions = dispatchedOfType("APPEND_AGENT_TOKEN");
    expect(tokenActions).toHaveLength(2);
    expect(tokenActions[0].payload).toMatchObject({ token: "hello", done: false });
    expect(tokenActions[1].payload).toMatchObject({ token: " world", done: false });
  });

  it("voice-tool-call dispatches PUSH_CONTEXT_CARD EXACTLY ONCE", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitTauriEvent("voice-tool-call", { tool: "giap__weather", id: "call-1" }); });

    const cardActions = dispatchedOfType("PUSH_CONTEXT_CARD");
    expect(cardActions).toHaveLength(1);
    expect(cardActions[0].payload).toMatchObject({
      tool: "giap__weather",
      callId: "call-1",
    });
  });

  it("voice-tool-result dispatches PUSH_CONTEXT_CARD EXACTLY ONCE", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => {
      emitTauriEvent("voice-tool-result", {
        tool: "giap__weather",
        id: "call-1",
        content: "Temperature: 22C",
      });
    });

    const cardActions = dispatchedOfType("PUSH_CONTEXT_CARD");
    expect(cardActions).toHaveLength(1);
    expect(cardActions[0].payload).toMatchObject({
      tool: "giap__weather",
      callId: "call-1",
      data: { id: "call-1", result: "Temperature: 22C" },
    });
  });

  it("voice-done dispatches APPEND_AGENT_TOKEN(done) + SET_SESSION_ID + SET_VOICE_STATE EXACTLY ONCE each", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitTauriEvent("voice-done", { session_id: "sess-abc" }); });

    expect(dispatchedOfType("APPEND_AGENT_TOKEN")).toHaveLength(1);
    expect(dispatchedOfType("APPEND_AGENT_TOKEN")[0].payload).toMatchObject({ done: true });
    expect(dispatchedOfType("SET_SESSION_ID")).toHaveLength(1);
    expect(dispatchedOfType("SET_SESSION_ID")[0].payload).toBe("sess-abc");
    expect(dispatchedOfType("SET_VOICE_STATE")).toHaveLength(1);
    expect(dispatchedOfType("SET_VOICE_STATE")[0].payload).toBe("idle");
  });

  it("voice-error dispatches SET_VOICE_ERROR + SET_VOICE_STATE(error) EXACTLY ONCE each", async () => {
    vi.useFakeTimers();
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitTauriEvent("voice-error", { message: "mic not found" }); });

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

  it("voice-session-ended with code 0 transitions to idle without error", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    // Simulate session active
    act(() => { emitTauriEvent("voice-ready", { session_id: "s1" }); });
    expect(result.current.sessionActive).toBe(true);

    clearDispatched();
    act(() => { emitTauriEvent("voice-session-ended", { code: 0, reason: "stdin_eof" }); });

    expect(result.current.sessionActive).toBe(false);
    expect(result.current.connecting).toBe(false);
    // No error dispatched
    expect(dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null)).toHaveLength(0);
    // Returns to idle
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(true);
  });

  it("voice-session-ended with non-zero code surfaces an error", async () => {
    vi.useFakeTimers();
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    act(() => { emitTauriEvent("voice-ready", { session_id: "s1" }); });
    expect(result.current.sessionActive).toBe(true);

    clearDispatched();
    act(() => { emitTauriEvent("voice-session-ended", { code: 1, reason: "error" }); });

    expect(result.current.sessionActive).toBe(false);
    const errorActions = dispatchedOfType("SET_VOICE_ERROR").filter((a) => a.payload !== null);
    expect(errorActions).toHaveLength(1);
    expect(errorActions[0].payload).toMatch(/code 1/);
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "error")).toBe(true);

    // Auto-clears after 4s
    act(() => { vi.advanceTimersByTime(4100); });
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(true);

    vi.useRealTimers();
    cleanup();
  });

  it("voice-ready sets sessionActive true and dispatches SET_SESSION_ID", async () => {
    const useVoiceSession = await getHook();
    _invoke = vi.fn().mockResolvedValue("s1");

    const { result } = renderHook(() => useVoiceSession(), { wrapper });
    await act(async () => { await Promise.resolve(); });

    clearDispatched();
    act(() => { emitTauriEvent("voice-ready", { session_id: "ready-sess" }); });

    expect(result.current.sessionActive).toBe(true);
    expect(result.current.connecting).toBe(false);
    expect(dispatchedOfType("SET_SESSION_ID")[0].payload).toBe("ready-sess");
    expect(dispatchedOfType("SET_VOICE_STATE").some((a) => a.payload === "idle")).toBe(true);
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
    act(() => { emitTauriEvent("voice-ready", { session_id: "seq-session" }); });
    act(() => { emitTauriEvent("voice-state", "wait"); });
    act(() => { emitTauriEvent("voice-state", "listen"); });
    act(() => { emitTauriEvent("voice-transcript", { text: "what is the weather" }); });
    act(() => { emitTauriEvent("voice-state", "thinking"); });
    act(() => { emitTauriEvent("voice-token", { content: "The" }); });
    act(() => { emitTauriEvent("voice-token", { content: " weather" }); });
    act(() => { emitTauriEvent("voice-tool-call", { tool: "giap__weather", id: "c1" }); });
    act(() => { emitTauriEvent("voice-tool-result", { tool: "giap__weather", id: "c1", content: "22C" }); });
    act(() => { emitTauriEvent("voice-state", "speak"); });
    act(() => { emitTauriEvent("voice-done", { session_id: "seq-session" }); });
    act(() => { emitTauriEvent("voice-state", "wait"); });

    // Verify state transitions
    const stateActions = dispatchedOfType("SET_VOICE_STATE").map((a) => a.payload);
    expect(stateActions).toContain("idle"); // from voice-ready
    expect(stateActions).toContain("recording"); // from voice-state listen
    expect(stateActions).toContain("thinking"); // from voice-state thinking
    expect(stateActions).toContain("speaking"); // from voice-state speak

    // Transcript was appended once (user + agent seed = 2 dispatches from one event)
    const transcriptActions = dispatchedOfType("APPEND_TRANSCRIPT");
    expect(transcriptActions).toHaveLength(2);
    expect(transcriptActions[0].payload).toMatchObject({ role: "user" });

    // Two tokens dispatched exactly twice
    const tokenActions = dispatchedOfType("APPEND_AGENT_TOKEN").filter(
      (a) => (a.payload as { done: boolean }).done === false,
    );
    expect(tokenActions).toHaveLength(2);

    // Two context cards (tool_call + tool_result)
    expect(dispatchedOfType("PUSH_CONTEXT_CARD")).toHaveLength(2);

    // turn_complete: APPEND_AGENT_TOKEN(done) + SET_SESSION_ID + SET_VOICE_STATE(idle)
    const doneActions = dispatchedOfType("APPEND_AGENT_TOKEN").filter(
      (a) => (a.payload as { done: boolean }).done === true,
    );
    expect(doneActions).toHaveLength(1);
  });
});
