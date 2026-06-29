import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Settings } from "./Settings";
import { api } from "../api/PondApiClient";

// ── Mocks ─────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn().mockResolvedValue({
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
      voice_wake_word: "goose",
      voice_wake_word_transcriptions: [],
    }),
    updateSettings: vi.fn().mockResolvedValue({}),
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

  it("Save button calls api.updateSettings with current state", async () => {
    await renderSettings();
    await navigateTo("Account");
    fireEvent.click(screen.getByText("Save"));
    await waitFor(() => {
      if (vi.mocked(api.updateSettings).mock.calls.length === 0) {
        throw new Error("updateSettings not called yet");
      }
    });
    expect(vi.mocked(api.updateSettings)).toHaveBeenCalledTimes(1);
    const arg = vi.mocked(api.updateSettings).mock.calls[0][0];
    expect(arg).toMatchObject({ assistant_name: "Pond", user_name: "Jerry" });
  });
});
