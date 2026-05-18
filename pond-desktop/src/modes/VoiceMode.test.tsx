/**
 * VoiceMode unit tests
 *
 * Tests the wake-word-triggered recording flow:
 *   wait → (wake word detected) → recording → thinking → speaking → wait
 *
 * Uses vitest + @testing-library/react.  Tauri IPC (invoke / listen) and
 * the API client are fully mocked so these run in happy-dom without a
 * native binary.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, act } from "@testing-library/react";
import type { AppState } from "../state/reducer";

// ── Hoist mocks so factory closures can reference them safely ─────────────────
const mockInvoke = vi.hoisted(() =>
  vi.fn().mockResolvedValue(undefined)
);
const mockListen = vi.hoisted(() => vi.fn());
const mockGetSettings = vi.hoisted(() => vi.fn());
const mockDispatch = vi.hoisted(() => vi.fn());

// Listener registry — keyed by event name
const _listeners = vi.hoisted(() => ({}) as Record<string, Array<(e: { payload: unknown }) => void>>);

// ── Mocks ────────────────────────────────────────────────────────────────────
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mockInvoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mockListen,
}));

vi.mock("../api/PondApiClient", () => ({
  api: { getSettings: mockGetSettings },
}));

// Mutable state shared between all useAppState() calls in a test
let _voiceState = "idle";
let _transcript: AppState["transcript"] = [];
let _voiceError: string | null = null;

vi.mock("../state/AppContext", () => ({
  useAppState: () => ({
    voiceState: _voiceState,
    voiceError: _voiceError,
    serverOnline: true,
    sessionToken: "tok-abc",
    sessionId: null,
    transcript: _transcript,
    contextCards: [],
    voiceRequestId: 0,
  }),
  useAppDispatch: () => mockDispatch,
}));

// ── Import component after mocks are established ──────────────────────────────
import { VoiceMode } from "./VoiceMode";

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Fire a mocked Tauri event to all registered listeners. */
function fireTauriEvent(name: string, payload: unknown = undefined) {
  (_listeners[name] ?? []).forEach((cb) => cb({ payload }));
}

function renderVoiceMode(opts: {
  voiceState?: AppState["voiceState"];
  transcript?: AppState["transcript"];
  voiceError?: string | null;
} = {}) {
  _voiceState   = opts.voiceState ?? "idle";
  _transcript   = opts.transcript ?? [];
  _voiceError   = opts.voiceError ?? null;
  return render(<VoiceMode />);
}

// ── Setup / teardown ──────────────────────────────────────────────────────────

beforeEach(() => {
  vi.clearAllMocks();
  // Reset listener registry
  Object.keys(_listeners).forEach((k) => { delete _listeners[k]; });
  _voiceState = "idle";
  _transcript = [];
  _voiceError = null;

  // Simulate Tauri environment so isTauri checks inside VoiceMode pass.
  // Without this, all invoke() calls behind `if (isTauri)` guards are skipped.
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    value: {},
    configurable: true,
    writable: true,
  });

  // Default: no wake word configured
  mockGetSettings.mockResolvedValue({
    voice_wake_word: "",
    voice_recording_duration_secs: 30,
  });

  // Set up the listen mock: captures callbacks per event name
  mockListen.mockImplementation((event: string, cb: (e: { payload: unknown }) => void) => {
    if (!_listeners[event]) _listeners[event] = [];
    _listeners[event].push(cb);
    return Promise.resolve(() => {
      _listeners[event] = (_listeners[event] ?? []).filter((x) => x !== cb);
    });
  });
});

afterEach(() => {
  cleanup();
  // Remove the simulated Tauri environment
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  delete (window as any).__TAURI_INTERNALS__;
});

// ── Tests — idle state ────────────────────────────────────────────────────────

describe("VoiceMode — idle (no wake word)", () => {
  it("shows 'Start Listening' button when idle", async () => {
    renderVoiceMode({ voiceState: "idle" });
    await waitFor(() => expect(screen.queryByText("Start Listening")).toBeTruthy());
  });

  it("does NOT invoke start_wake_listener when voice_wake_word is empty", async () => {
    renderVoiceMode({ voiceState: "idle" });
    await waitFor(() => expect(mockGetSettings).toHaveBeenCalled());
    // Give async chain time to complete
    await new Promise((r) => setTimeout(r, 50));
    expect(mockInvoke).not.toHaveBeenCalledWith("start_wake_listener", expect.anything());
  });

  it("shows 'Ready' status label when idle", () => {
    renderVoiceMode({ voiceState: "idle" });
    expect(screen.queryByText("Ready")).toBeTruthy();
  });
});

// ── Tests — wait state ────────────────────────────────────────────────────────

describe("VoiceMode — wait (wake word configured)", () => {
  beforeEach(() => {
    mockGetSettings.mockResolvedValue({
      voice_wake_word: "goose",
      voice_recording_duration_secs: 30,
    });
  });

  it("dispatches SET_VOICE_STATE 'wait' on mount", async () => {
    renderVoiceMode({ voiceState: "idle" });
    await waitFor(() => {
      expect(mockDispatch).toHaveBeenCalledWith({
        type: "SET_VOICE_STATE",
        payload: "wait",
      });
    });
  });

  it("invokes start_wake_listener with the wake word", async () => {
    renderVoiceMode({ voiceState: "idle" });
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("start_wake_listener", {
        wakeWord: "goose",
        variants: null,
      });
    });
  });

  it("shows passive listening indicator (not a 'Listen now' button) when state is 'wait'", () => {
    renderVoiceMode({ voiceState: "wait" });
    // Should NOT show an action button to start listening — it's already listening
    expect(screen.queryByText("Listen now")).toBeFalsy();
    // Should show a "Record now" manual override button
    expect(screen.queryByText("Record now")).toBeTruthy();
  });

  it("shows 'Waiting…' status label when state is 'wait'", () => {
    renderVoiceMode({ voiceState: "wait" });
    expect(screen.queryByText("Waiting…")).toBeTruthy();
  });
});

// ── Tests — wake-word-detected → recording ────────────────────────────────────

describe("VoiceMode — wake-word-detected triggers recording", () => {
  beforeEach(() => {
    mockGetSettings.mockResolvedValue({
      voice_wake_word: "goose",
      voice_recording_duration_secs: 30,
    });
  });

  it("does NOT stop wake listener on detection (always-on), starts recording or pipeline", async () => {
    renderVoiceMode({ voiceState: "wait" });

    // Wait for start_wake_listener to be called (wake listener setup complete)
    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("start_wake_listener", {
        wakeWord: "goose",
        variants: null,
      });
    });

    // Wait for the wake-word-detected event listener to be registered
    await waitFor(() => {
      expect(_listeners["wake-word-detected"]?.length).toBeGreaterThan(0);
    });

    // Simulate Rust emitting wake-word-detected
    await act(async () => {
      fireTauriEvent("wake-word-detected");
    });

    // Wake listener is always-on — stop_wake_listener must NOT be called on detection
    expect(mockInvoke).not.toHaveBeenCalledWith("stop_wake_listener");
    // VAD recording or one-breath pipeline must start
    const usesVad = mockInvoke.mock.calls.some((c: unknown[]) => c[0] === "record_with_vad");
    const usesPipeline = mockInvoke.mock.calls.some((c: unknown[]) => c[0] === "run_voice_pipeline");
    expect(usesVad || usesPipeline).toBe(true);
  });

  it("registers wake-word-interrupt listener for barge-in", async () => {
    renderVoiceMode({ voiceState: "wait" });

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("start_wake_listener", {
        wakeWord: "goose",
        variants: null,
      });
    });

    // The interrupt listener should be registered alongside the detection listener
    await waitFor(() => {
      expect(_listeners["wake-word-interrupt"]?.length).toBeGreaterThan(0);
    });
  });
});

// ── Tests — recording UI ──────────────────────────────────────────────────────

describe("VoiceMode — recording state", () => {
  it("shows Send and Cancel buttons when recording", () => {
    renderVoiceMode({ voiceState: "recording" });
    expect(screen.queryByText("Send")).toBeTruthy();
    expect(screen.queryByText("Cancel")).toBeTruthy();
  });

  it("shows 'Listening…' label when recording", () => {
    renderVoiceMode({ voiceState: "recording" });
    expect(screen.queryByText("Listening…")).toBeTruthy();
  });
});

// ── Tests — thinking / speaking ───────────────────────────────────────────────

describe("VoiceMode — thinking state", () => {
  it("shows animated dots and 'Thinking…' label", () => {
    renderVoiceMode({ voiceState: "thinking" });
    // "Thinking…" appears in both the status bar and the action bar row
    expect(screen.queryAllByText("Thinking…").length).toBeGreaterThanOrEqual(1);
    expect(screen.queryByText("●●●")).toBeTruthy();
  });
});

describe("VoiceMode — speaking state", () => {
  it("shows Interrupt button and 'Speaking…' label", () => {
    renderVoiceMode({ voiceState: "speaking" });
    expect(screen.queryByText("Interrupt")).toBeTruthy();
    expect(screen.queryByText("Speaking…")).toBeTruthy();
  });
});

// ── Tests — transcript feed ───────────────────────────────────────────────────

describe("VoiceMode — transcript display", () => {
  it("renders user message in the transcript feed", () => {
    renderVoiceMode({
      voiceState: "idle",
      transcript: [{ id: 1, role: "user", text: "hey goose weather today", timestamp: 0 }],
    });
    expect(screen.queryByText("hey goose weather today")).toBeTruthy();
  });

  it("renders agent reply in the transcript feed", () => {
    renderVoiceMode({
      voiceState: "idle",
      transcript: [
        { id: 1, role: "user", text: "what is 2 + 2", timestamp: 0 },
        { id: 2, role: "agent", text: "2 + 2 equals 4.", timestamp: 0 },
      ],
    });
    expect(screen.queryByText("what is 2 + 2")).toBeTruthy();
    expect(screen.queryByText("2 + 2 equals 4.")).toBeTruthy();
  });
});

// ── Tests — error state ───────────────────────────────────────────────────────

describe("VoiceMode — error state", () => {
  it("shows Dismiss button when in error state", () => {
    renderVoiceMode({ voiceState: "error", voiceError: "mic unavailable" });
    expect(screen.queryByText("Dismiss")).toBeTruthy();
  });

  it("shows the error message as the status label", () => {
    renderVoiceMode({ voiceState: "error", voiceError: "mic unavailable" });
    expect(screen.queryByText("mic unavailable")).toBeTruthy();
  });
});

// ── Tests — teardown / cleanup ────────────────────────────────────────────────

describe("VoiceMode — unmount cleanup", () => {
  it("invokes stop_wake_listener on unmount when wake word was configured", async () => {
    mockGetSettings.mockResolvedValue({
      voice_wake_word: "goose",
      voice_recording_duration_secs: 30,
    });

    const { unmount } = renderVoiceMode({ voiceState: "wait" });

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("start_wake_listener", {
        wakeWord: "goose",
        variants: null,
      });
    });

    // Reset call tracking so we can check only post-unmount calls
    mockInvoke.mockClear();

    unmount();

    await waitFor(() => {
      expect(mockInvoke).toHaveBeenCalledWith("stop_wake_listener");
    });
  });
});
