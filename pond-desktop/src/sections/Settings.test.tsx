import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Settings, diffSettings, settingsValueEquals, foldServerState } from "./Settings";
import { api } from "../api/PondApiClient";

// ── Mocks ─────────────────────────────────────────────────────

const SERVER_SETTINGS = {
  assistant_name: "Pond",
  user_name: "Jerry",
  prompt_style: "balanced",
  agent_memory_inject: false,
  weather_enabled: false,
  chat_provider: "llamafile",
  chat_model: "llama3.2",
  llm_provider: "llamafile",
  llm_temperature: 0.7,
  llm_max_tokens: 1024,
  agent_goose_mode: "auto",
  agent_max_turns: 10,
  agent_memory_limit: 5,
  weather_latitude: 0,
  weather_longitude: 0,
  retention_event_log_days: 30,
  retention_sensor_days: 7,
  retention_session_messages_keep: 100,
  retention_events_by_category: { motion: 14, doorbell: 30 },
  voice_wake_word: "goose",
  voice_wake_word_transcriptions: [] as string[],
};

/** A fresh deep copy, so no test can share a nested object with another. */
function serverSettings(overrides: Record<string, unknown> = {}) {
  return { ...structuredClone(SERVER_SETTINGS), ...overrides };
}

vi.mock("../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    // The real endpoint echoes the full merged object back.
    updateSettings: vi.fn(),
    resetWakeWordCalibration: vi.fn().mockResolvedValue(undefined),
    listModels: vi.fn().mockResolvedValue([]),
    getActiveRoles: vi.fn().mockResolvedValue({ chat: null, think: null, task: null, asr: null, tts: null }),
  },
}));

vi.mock("../state/AppContext", () => ({
  useAppState: () => ({ serverUrl: "http://127.0.0.1:4000" }),
  useAppDispatch: () => vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockResolvedValue(undefined),
}));

// The real panel lazy-loads this and it opens a microphone. The calibration
// tests below only care about what the Calibrate click PUTs.
vi.mock("../components/WakeWordCalibration", () => ({
  WakeWordCalibration: () => <div>calibration running</div>,
}));

// ── Helpers ───────────────────────────────────────────────────

async function renderSettings() {
  render(<Settings />);
  await waitFor(() => {
    if (!screen.queryByText("Settings")) throw new Error("not loaded");
  });
}

async function navigateTo(label: string) {
  fireEvent.click(screen.getByRole("button", { name: label }));
  await waitFor(() => {
    if (!screen.queryByText("← Back")) throw new Error("detail not loaded");
  });
}

function enableDevMode() {
  const btn = screen.queryByText("Developer mode");
  if (btn) fireEvent.click(btn);
  // If "Dev mode on" is already showing (localStorage persisted from prior test), devMode is already on
}

// ── Tests ─────────────────────────────────────────────────────

describe("Settings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.getSettings).mockResolvedValue(serverSettings() as never);
    // Echo the merged result the way the server does: current state + patch.
    vi.mocked(api.updateSettings).mockImplementation(
      async (patch) => serverSettings(patch as Record<string, unknown>) as never,
    );
  });

  afterEach(() => {
    cleanup();
  });

  it("renders list view with main settings rows", async () => {
    await renderSettings();
    expect(screen.getByText("Settings")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Account" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Models" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Voice" })).toBeTruthy();
  });

  it("Account panel shows user_name field", async () => {
    await renderSettings();
    await navigateTo("Account");
    const input = screen.getByPlaceholderText("Friend") as HTMLInputElement;
    expect(input.value).toBe("Jerry");
  });

  it("Voice panel shows voice_wake_word field", async () => {
    await renderSettings();
    await navigateTo("Voice");
    await waitFor(() => {
      if (!screen.queryByPlaceholderText("goose")) throw new Error("not rendered");
    });
    expect(screen.getByPlaceholderText("goose")).toBeTruthy();
  });

  it("Voice panel shows 'Not calibrated' and Calibrate button when transcriptions empty", async () => {
    await renderSettings();
    await navigateTo("Voice");
    await waitFor(() => {
      if (!screen.queryByText("Not calibrated")) throw new Error("not rendered");
    });
    expect(screen.getByText("Not calibrated")).toBeTruthy();
    expect(screen.getByText("Calibrate")).toBeTruthy();
  });

  it("Models panel shows AI Models section and Change button", async () => {
    await renderSettings();
    await navigateTo("Models");
    await waitFor(() => {
      if (!screen.queryByText("AI Models")) throw new Error("not rendered");
    });
    expect(screen.getByText("AI Models")).toBeTruthy();
    const changeBtns = screen.getAllByText("Change…");
    expect(changeBtns.length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("llamafile / llama3.2")).toBeTruthy();
  });

  it("Prompts panel renders 4 prompt_style radio options", async () => {
    await renderSettings();
    await navigateTo("Prompts");
    await waitFor(() => {
      if (!screen.queryByText("Balanced")) throw new Error("not rendered");
    });
    expect(screen.getByText("Balanced")).toBeTruthy();
    expect(screen.getByText("Concise")).toBeTruthy();
    expect(screen.getByText("Technical")).toBeTruthy();
    expect(screen.getByText("Warm")).toBeTruthy();
  });

  it("Memory panel shows 3 agent_goose_mode radio options and memory_limit disabled when inject off", async () => {
    await renderSettings();
    enableDevMode();
    await waitFor(() => {
      if (!screen.queryByText("Advanced")) throw new Error("dev mode not enabled");
    });
    await navigateTo("Memory");
    await waitFor(() => {
      if (!screen.queryByText("Smart (recommended)")) throw new Error("not rendered");
    });
    expect(screen.getByText("Smart (recommended)")).toBeTruthy();
    expect(screen.getByText("Chat only")).toBeTruthy();
    expect(screen.getByText("Proactive")).toBeTruthy();
    const spinbtns = screen.getAllByRole("spinbutton") as HTMLInputElement[];
    const memLimit = spinbtns.find((el) => el.disabled && el.value === "5");
    expect(memLimit).toBeTruthy();
  });

  it("Privacy panel shows data retention fields", async () => {
    await renderSettings();
    enableDevMode();
    await waitFor(() => {
      if (!screen.queryByText("Advanced")) throw new Error("dev mode not enabled");
    });
    await navigateTo("Privacy");
    await waitFor(() => {
      if (!screen.queryByText("Keep event logs for")) throw new Error("not rendered");
    });
    expect(screen.getByText("Keep event logs for")).toBeTruthy();
    const spinbtns = screen.getAllByRole("spinbutton") as HTMLInputElement[];
    expect(spinbtns.length).toBeGreaterThan(0);
  });

});

// ── Save sends a PATCH, not the whole object ──────────────────
//
// `PUT /api/v1/settings` marks every key the request carries as deliberately
// chosen (`is_user_set`), which permanently exempts it from future
// default-adoption migrations. A Save that sends the full loaded object marks
// EVERY setting from a click that changed nothing, and reverts fields another
// surface wrote since the panel loaded. See
// docs/developer/settings-defaults-and-user-intent.md.

describe("Settings save payload", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.getSettings).mockResolvedValue(serverSettings() as never);
    vi.mocked(api.updateSettings).mockImplementation(
      async (patch) => serverSettings(patch as Record<string, unknown>) as never,
    );
  });

  afterEach(() => {
    cleanup();
  });

  /** The patch body of the single PUT, or `undefined` if none was made. */
  function sentPatch(): Record<string, unknown> | undefined {
    const calls = vi.mocked(api.updateSettings).mock.calls;
    expect(calls.length).toBeLessThanOrEqual(1);
    return calls[0]?.[0] as Record<string, unknown> | undefined;
  }

  async function clickSave() {
    fireEvent.click(screen.getByText("Save"));
    await waitFor(() => {
      if (!screen.queryByText("Saved")) throw new Error("save not finished");
    });
  }

  it("a Save with no edits sends no patch at all", async () => {
    await renderSettings();
    await navigateTo("Account");
    await clickSave();

    const patch = sentPatch();
    expect(patch === undefined || Object.keys(patch).length === 0).toBe(true);
  });

  it("a Save of one field sends exactly that field", async () => {
    await renderSettings();
    await navigateTo("Account");
    fireEvent.change(screen.getByPlaceholderText("Friend"), { target: { value: "Ochieng" } });
    await clickSave();

    expect(sentPatch()).toEqual({ user_name: "Ochieng" });
  });

  it("never re-sends the server-owned wake-word transcriptions", async () => {
    // Calibration appends to this field server side. A full-object Save from a
    // panel loaded before calibration would wipe it.
    vi.mocked(api.getSettings).mockResolvedValue(
      serverSettings({ voice_wake_word_transcriptions: ["goose", "guse"] }) as never,
    );
    await renderSettings();
    await navigateTo("Account");
    fireEvent.change(screen.getByPlaceholderText("Friend"), { target: { value: "Ochieng" } });
    await clickSave();

    expect(sentPatch()).not.toHaveProperty("voice_wake_word_transcriptions");
  });

  it("a second Save after the first sends only what changed since", async () => {
    await renderSettings();
    await navigateTo("Account");
    fireEvent.change(screen.getByPlaceholderText("Friend"), { target: { value: "Ochieng" } });
    await clickSave();
    await waitFor(() => {
      if (screen.queryByText("Saved")) throw new Error("still flashing Saved");
    }, { timeout: 3000 });

    fireEvent.change(screen.getByPlaceholderText("Goose"), { target: { value: "Pondy" } });
    await clickSave();

    const calls = vi.mocked(api.updateSettings).mock.calls;
    expect(calls.length).toBe(2);
    expect(calls[0][0]).toEqual({ user_name: "Ochieng" });
    expect(calls[1][0]).toEqual({ assistant_name: "Pondy" });
  });

  // Every case above uses a STRING field, so none of them can see this: the
  // API serialises `f32` through serde_json, which widens to `f64`, and the
  // echo of a float is therefore never the literal that was sent. Save twice
  // and the field must still converge.
  it("a float field converges even though the server echoes a widened value", async () => {
    // What the real endpoint does to every f32 field it echoes.
    const widen = (o: Record<string, unknown>) =>
      typeof o.llm_temperature === "number"
        ? { ...o, llm_temperature: Math.fround(o.llm_temperature) }
        : o;
    vi.mocked(api.getSettings).mockResolvedValue(
      widen(serverSettings()) as never,
    );
    vi.mocked(api.updateSettings).mockImplementation(
      async (patch) => widen(serverSettings(patch as Record<string, unknown>)) as never,
    );

    await renderSettings();
    await navigateTo("Models");
    // The Creativity slider is the only range input on this panel.
    const slider = document.querySelector('input[type="range"]');
    if (!slider) throw new Error("no creativity slider");
    fireEvent.change(slider, { target: { value: "0.8" } });
    await clickSave();

    const first = vi.mocked(api.updateSettings).mock.calls;
    expect(first.length).toBe(1);
    expect(first[0][0]).toEqual({ llm_temperature: 0.8 });
    // The echo really is a different number, or this test proves nothing.
    expect(Math.fround(0.8)).not.toBe(0.8);

    await waitFor(() => {
      if (screen.queryByText("Saved")) throw new Error("still flashing Saved");
    }, { timeout: 3000 });

    await clickSave();
    expect(vi.mocked(api.updateSettings).mock.calls.length).toBe(1);
  });

  it("Calibrate does not re-send an unchanged wake phrase", async () => {
    await renderSettings();
    await navigateTo("Voice");
    fireEvent.click(screen.getByText("Calibrate"));
    await waitFor(() => {
      if (!screen.queryByText("calibration running")) throw new Error("not calibrating");
    });

    // Writing it back would mark `voice_wake_word` as deliberately chosen on a
    // click that changed nothing, ending default adoption for that key.
    expect(vi.mocked(api.updateSettings)).not.toHaveBeenCalled();
    expect(vi.mocked(api.resetWakeWordCalibration)).toHaveBeenCalled();
  });

  it("Calibrate persists a wake phrase the user just edited, and nothing else", async () => {
    await renderSettings();
    await navigateTo("Voice");
    fireEvent.change(screen.getByPlaceholderText("goose"), { target: { value: "hey pond" } });
    fireEvent.click(screen.getByText("Calibrate"));
    await waitFor(() => {
      if (!screen.queryByText("calibration running")) throw new Error("not calibrating");
    });

    expect(sentPatch()).toEqual({ voice_wake_word: "hey pond" });

    // That write moved the panel's baseline too, so the next Save has nothing
    // to say about the field. Without adopting the response, Save would send
    // the phrase a second time and re-mark it.
    await clickSave();
    expect(vi.mocked(api.updateSettings).mock.calls.length).toBe(1);
  });
});

// ── Diff primitives ───────────────────────────────────────────

describe("diffSettings", () => {
  it("reports nothing when the object is untouched", () => {
    const server = serverSettings();
    expect(diffSettings(server, structuredClone(server))).toEqual({});
  });

  it("compares arrays and maps by value, not identity", () => {
    const baseline = serverSettings({
      voice_wake_word_transcriptions: ["goose"],
      retention_events_by_category: { motion: 14, doorbell: 30 },
    });
    // Same values, different object identities, and a different key order.
    const current = serverSettings({
      voice_wake_word_transcriptions: ["goose"],
      retention_events_by_category: { doorbell: 30, motion: 14 },
    });
    expect(diffSettings(baseline, current)).toEqual({});
  });

  it("reports an array or map that really changed", () => {
    const baseline = serverSettings({ voice_wake_word_transcriptions: ["goose"] });
    const current = serverSettings({ voice_wake_word_transcriptions: ["goose", "guse"] });
    expect(diffSettings(baseline, current)).toEqual({
      voice_wake_word_transcriptions: ["goose", "guse"],
    });
  });

  it("distinguishes null, undefined-in-baseline and a real value", () => {
    expect(diffSettings({ tool_model: null }, { tool_model: "qwen" })).toEqual({ tool_model: "qwen" });
    expect(diffSettings({ tool_model: "qwen" }, { tool_model: null })).toEqual({ tool_model: null });
    expect(diffSettings({}, { tool_model: null })).toEqual({ tool_model: null });
    expect(diffSettings({ tool_model: null }, {})).toEqual({});
  });

  it("does not confuse an array with an object", () => {
    expect(settingsValueEquals([], {})).toBe(false);
    expect(settingsValueEquals({ 0: "a" }, ["a"])).toBe(false);
  });
});

describe("foldServerState", () => {
  it("takes the server value for a field the user has not touched", () => {
    const baseline = { user_name: "Jerry", chat_model: "llama3.2" };
    const prev = { ...baseline };
    const server = { user_name: "Jerry", chat_model: "gemma-4" };
    expect(foldServerState(prev, baseline, server)).toEqual({
      user_name: "Jerry",
      chat_model: "gemma-4",
    });
  });

  it("keeps an unsaved local edit when the server disagrees", () => {
    const baseline = { user_name: "Jerry", chat_model: "llama3.2" };
    const prev = { ...baseline, user_name: "Ochieng" };
    const server = { user_name: "Jerry", chat_model: "gemma-4" };
    expect(foldServerState(prev, baseline, server)).toEqual({
      user_name: "Ochieng",
      chat_model: "gemma-4",
    });
  });
});
