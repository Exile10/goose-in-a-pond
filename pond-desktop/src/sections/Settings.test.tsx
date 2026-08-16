import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
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
    // The Extensions panel loads both of these on mount.
    listExtensions: vi.fn().mockResolvedValue({ extensions: [] }),
    listSecretKeys: vi.fn().mockResolvedValue([]),
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

  // PAI-5 P4. The Rust side classifies `reasoning_effort` UI_WIRED, and that
  // classification is checked by a test that only looks at a list of strings --
  // it cannot tell whether a control exists. This is the part that can.
  it("the thinking-length control writes reasoning_effort and nothing else", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(
      serverSettings({ reasoning_effort: "brief" }) as never,
    );
    await renderSettings();
    enableDevMode();
    await waitFor(() => {
      if (!screen.queryByText("Advanced")) throw new Error("dev mode not enabled");
    });
    await navigateTo("Models");
    await waitFor(() => {
      if (!screen.queryByText("Thinking length")) throw new Error("control not rendered");
    });

    // Located by its option set, not by its current value: another select on
    // this panel could easily share a value, and a control found by accident
    // would assert nothing.
    const select = (screen.getAllByRole("combobox") as HTMLSelectElement[]).find(
      (el) =>
        Array.from(el.options)
          .map((o) => o.value)
          .join(",") === "brief,balanced,thorough",
    );
    if (!select) throw new Error("no brief/balanced/thorough select on the Models panel");
    expect(select.value).toBe("brief");

    fireEvent.change(select, { target: { value: "thorough" } });
    await clickSave();

    expect(sentPatch()).toEqual({ reasoning_effort: "thorough" });
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

  // ── The thinking tone's switch ──────────────────────────────
  //
  // The tone was deleted outright in 0136f8c5 and restored behind this
  // setting. The deletion is the reason these assert the rendered STATE as
  // well as the patch key: a switch wired to nothing looks exactly like a
  // working one, which is how a whole feature went missing inside a commit
  // about copy buttons.

  it("the thinking-tone switch reads ON when the key is absent", async () => {
    // The field defaults ON in Rust, so an old stored settings row has no such
    // key. Rendering that as OFF would tell a household the pond is silent
    // while it is in fact pulsing at them.
    vi.mocked(api.getSettings).mockResolvedValue(
      serverSettings({ voice_thinking_tone_enabled: undefined }) as never,
    );
    await renderSettings();
    await navigateTo("Voice");

    const el = screen.getByRole("switch", { name: "Sound while it thinks" }) as HTMLInputElement;
    expect(el.checked).toBe(true);
  });

  it("the thinking-tone switch reads OFF when the setting is off", async () => {
    // The control for the case above: a switch hardcoded to `true`, or bound to
    // the wrong key, passes that test and fails this one.
    vi.mocked(api.getSettings).mockResolvedValue(
      serverSettings({ voice_thinking_tone_enabled: false }) as never,
    );
    await renderSettings();
    await navigateTo("Voice");

    const el = screen.getByRole("switch", { name: "Sound while it thinks" }) as HTMLInputElement;
    expect(el.checked).toBe(false);
  });

  it("the thinking-tone switch is reachable without opening Advanced", async () => {
    // The rest of the speech settings live behind "Advanced voice settings".
    // This one must not: the household most likely to want the tone off is the
    // least likely to go looking in there.
    //
    // Dev mode is forced off rather than assumed: it persists in localStorage,
    // so an earlier test in this file leaves the advanced sections expanded and
    // the assertion below would pass for the wrong reason.
    localStorage.setItem("settings-dev-mode", "false");
    await renderSettings();
    await navigateTo("Voice");

    expect(screen.queryByText("Speech Synthesis")).toBeNull();
    expect(screen.getByRole("switch", { name: "Sound while it thinks" })).toBeTruthy();
  });

  it("the thinking-tone switch writes voice_thinking_tone_enabled and nothing else", async () => {
    await renderSettings();
    await navigateTo("Voice");

    const el = screen.getByRole("switch", { name: "Sound while it thinks" }) as HTMLInputElement;
    expect(el.checked).toBe(true);

    fireEvent.click(el);
    await clickSave();

    expect(sentPatch()).toEqual({ voice_thinking_tone_enabled: false });
  });
});

// ── PAI-7 P4 and P6: speaking and acting unprompted ───────────
//
// These five settings were HEADLESS_BY_DESIGN in
// crates/pond-core/src/user_data/domain/settings.rs and reachable only by
// curl. That Rust guard checks a list of STRINGS: it cannot tell whether a
// control exists, only that somebody classified the field. This is the part
// that can.
//
// Every case below asserts the KEY the control PATCHes, not merely that
// something rendered. A row copy-pasted from its neighbour looks identical on
// screen and writes the neighbour's key, and nothing else in this suite — or
// in the Rust suite — would notice. Each case also asserts the control's state
// BEFORE acting, which is the vacuity control: a control that renders but is
// not bound to the setting fails there rather than passing silently.

/** The panel's own settings, with the five at their shipped defaults. */
function unpromptedSettings(overrides: Record<string, unknown> = {}) {
  return serverSettings({
    proactive_review_enabled: false,
    unprompted_speech_enabled: false,
    quiet_hours_start: "22:00",
    quiet_hours_end: "07:00",
    unprompted_speech_categories: "alert",
    ...overrides,
  });
}

describe("Settings unprompted-behaviour controls", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.getSettings).mockResolvedValue(unpromptedSettings() as never);
    vi.mocked(api.updateSettings).mockImplementation(
      async (patch) => unpromptedSettings(patch as Record<string, unknown>) as never,
    );
  });

  afterEach(() => {
    cleanup();
  });

  function sentPatch(): Record<string, unknown> | undefined {
    const calls = vi.mocked(api.updateSettings).mock.calls;
    expect(calls.length).toBeLessThanOrEqual(1);
    return calls[0]?.[0] as Record<string, unknown> | undefined;
  }

  /**
   * The panel's own Save, from the detail header.
   *
   * Not `getByText("Save")`: the API-keys section on this panel renders a Save
   * button per secret, and a bare text query matches those too.
   */
  async function clickSave() {
    const head = document.querySelector(".set__head-actions");
    if (!head) throw new Error("no detail header");
    fireEvent.click(within(head as HTMLElement).getByText("Save"));
    await waitFor(() => {
      if (!screen.queryByText("Saved")) throw new Error("save not finished");
    });
  }

  async function openPanel() {
    await renderSettings();
    await navigateTo("Extensions (MCP)");
    await waitFor(() => {
      if (!screen.queryByText("Speaking and acting unprompted")) {
        throw new Error("unprompted section not rendered");
      }
    });
  }

  it("shows all five controls, and both switches ship off", async () => {
    await openPanel();

    const start = screen.getByLabelText("Quiet hours start") as HTMLInputElement;
    const end = screen.getByLabelText("Quiet hours end") as HTMLInputElement;
    const categories = screen.getByLabelText("Categories it may speak") as HTMLInputElement;
    const speech = screen.getByRole("switch", { name: "Speak without being spoken to" }) as HTMLInputElement;
    const review = screen.getByRole("switch", { name: "Review the house unasked" }) as HTMLInputElement;

    // Each one shows what the server actually holds. A control rendering a
    // hardcoded placeholder would pass a mere presence check.
    expect(start.value).toBe("22:00");
    expect(end.value).toBe("07:00");
    expect(categories.value).toBe("alert");
    expect(speech.checked).toBe(false);
    expect(review.checked).toBe(false);
  });

  // Ordering is the teaching, so it is asserted rather than left to review.
  // `decide_unprompted_speech` refuses inside quiet hours BEFORE it looks at
  // consent, presence or category. A section that put the speech switch above
  // the window would say the opposite — that consent is the outer decision —
  // and nothing else in either suite would object.
  //
  // This is also the strict form of "the five controls are grouped together":
  // it fails if a sixth row is added to the section, not just if one is moved.
  it("puts quiet hours above the switch they outrank", async () => {
    await openPanel();

    const card = screen.getByText("Speaking and acting unprompted").closest(".card");
    if (!card) throw new Error("no card around the unprompted section");
    const names = Array.from(card.querySelectorAll(".row__name")).map((el) => el.textContent);

    expect(names).toEqual([
      "Quiet hours start",
      "Quiet hours end",
      "Speak without being spoken to",
      "Categories it may speak",
      "Review the house unasked",
    ]);
  });

  // The two switches must read `=== true`, so a settings payload that omits
  // them — an older server, or a read that returned a partial object — has to
  // render OFF. `!== false` would render ON and invite a household to believe
  // the pond was already allowed to speak.
  it("renders both switches off when the server omits them entirely", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(serverSettings() as never);
    await openPanel();

    expect(
      (screen.getByRole("switch", { name: "Speak without being spoken to" }) as HTMLInputElement).checked,
      "`unprompted_speech_enabled` must be read `=== true`: an absent key rendered ON tells a household the pond may already speak to them unasked",
    ).toBe(false);
    expect(
      (screen.getByRole("switch", { name: "Review the house unasked" }) as HTMLInputElement).checked,
      "`proactive_review_enabled` must be read `=== true`: an absent key rendered ON tells a household the pond may already reason about them unasked",
    ).toBe(false);
  });

  it("the quiet-hours start field writes quiet_hours_start and nothing else", async () => {
    await openPanel();
    const el = screen.getByLabelText("Quiet hours start") as HTMLInputElement;
    expect(el.value).toBe("22:00");

    fireEvent.change(el, { target: { value: "23:30" } });
    await clickSave();

    expect(sentPatch()).toEqual({ quiet_hours_start: "23:30" });
  });

  it("the quiet-hours end field writes quiet_hours_end and nothing else", async () => {
    await openPanel();
    const el = screen.getByLabelText("Quiet hours end") as HTMLInputElement;
    expect(el.value).toBe("07:00");

    fireEvent.change(el, { target: { value: "06:15" } });
    await clickSave();

    expect(sentPatch()).toEqual({ quiet_hours_end: "06:15" });
  });

  it("the categories field writes unprompted_speech_categories and nothing else", async () => {
    await openPanel();
    const el = screen.getByLabelText("Categories it may speak") as HTMLInputElement;
    expect(el.value).toBe("alert");

    fireEvent.change(el, { target: { value: "alert,reminder" } });
    await clickSave();

    expect(sentPatch()).toEqual({ unprompted_speech_categories: "alert,reminder" });
  });

  it("the speech switch writes unprompted_speech_enabled and nothing else", async () => {
    await openPanel();
    const el = screen.getByRole("switch", { name: "Speak without being spoken to" }) as HTMLInputElement;
    expect(el.checked).toBe(false);

    fireEvent.click(el);
    await clickSave();

    expect(sentPatch()).toEqual({ unprompted_speech_enabled: true });
  });

  it("the review switch writes proactive_review_enabled and nothing else", async () => {
    await openPanel();
    const el = screen.getByRole("switch", { name: "Review the house unasked" }) as HTMLInputElement;
    expect(el.checked).toBe(false);

    fireEvent.click(el);
    await clickSave();

    expect(sentPatch()).toEqual({ proactive_review_enabled: true });
  });

  // Not a duplicate of the five above. Those run one at a time, so each proves
  // only that ITS control names the right key in isolation; two controls both
  // writing `unprompted_speech_enabled` would still pass every one of them.
  // This edits all five in one pass and asserts the whole body.
  it("editing all five sends exactly those five keys", async () => {
    await openPanel();

    fireEvent.change(screen.getByLabelText("Quiet hours start"), { target: { value: "23:30" } });
    fireEvent.change(screen.getByLabelText("Quiet hours end"), { target: { value: "06:15" } });
    fireEvent.change(screen.getByLabelText("Categories it may speak"), { target: { value: "alert,reminder" } });
    fireEvent.click(screen.getByRole("switch", { name: "Speak without being spoken to" }));
    fireEvent.click(screen.getByRole("switch", { name: "Review the house unasked" }));
    await clickSave();

    expect(sentPatch()).toEqual({
      quiet_hours_start: "23:30",
      quiet_hours_end: "06:15",
      unprompted_speech_categories: "alert,reminder",
      unprompted_speech_enabled: true,
      proactive_review_enabled: true,
    });
  });

  // ── Every switch on this panel must be operable ─────────────
  //
  // This began as a tripwire saying only two rows worked, and it was right: a
  // `<Switch>` from @heroui/react whose children are only `Switch.Control` and
  // `Switch.Thumb` renders two spans and stops. The `<input role="switch">`
  // lives in `Switch.Content` (react-aria's `SwitchButton`), which twenty-two
  // rows on this shipped panel did not use -- so they had no input, no
  // accessible name, no click target and no `onChange`. `GuiMode.tsx` mounts
  // this panel, so those were twenty-two settings a user could see and not
  // change, `ext_orchestrator_enabled` among them.
  //
  // They are repaired, and the assertion is inverted to match: rather than
  // listing which rows work -- a list that rots every time somebody adds a
  // setting -- it asserts the PROPERTY. Every switch rendered here is a real
  // switch, and every one has a name a screen reader can say.
  it("every switch on the panel is a real, named control", async () => {
    await openPanel();

    const switches = screen.getAllByRole("switch");
    // Vacuity control. The old shape asserted a two-element list, which would
    // also have passed on a panel that rendered nothing; this one cannot.
    expect(switches.length).toBeGreaterThan(15);

    const unnamed = switches
      .filter((el) => !(el.getAttribute("aria-label") ?? "").trim())
      .map((el) => el.outerHTML.slice(0, 120));
    expect(unnamed).toEqual([]);
  });

  // Thinking is reachable WITHOUT developer mode.
  //
  // `thinking_mode` defaults to "auto", which resolves to ON for any model whose
  // name implies reasoning -- so a household runs a reasoning model, pays 76-156
  // reasoning tokens a turn, sees none of it (`show_thinking` ships false) and,
  // while these rows sat behind the developer pill, had no way to stop it.
  // "Thinking cannot be turned off" was literally true from this panel.
  //
  // openPanel() does not enable dev mode, so finding the control at all is the
  // assertion. The vacuity control is the second half: a genuinely dev-only row
  // must still be hidden, or this would pass on a panel that showed everything.
  it("the thinking controls are reachable without developer mode", async () => {
    // ModelsTab, not the Extensions tab `openPanel` opens -- and explicitly NOT
    // in developer mode, which is the whole assertion. devMode is read from
    // localStorage, which persists across tests in this file, so an earlier one
    // leaving it on would make this pass vacuously.
    localStorage.removeItem("settings-dev-mode");
    await renderSettings();
    await navigateTo("Models");

    expect(screen.getByLabelText("Thinking mode")).toBeTruthy();
    expect(screen.getByText("Thinking length")).toBeTruthy();
    expect(screen.getByRole("switch", { name: "Show thinking steps" })).toBeTruthy();

    // Vacuity control: without it this would pass on a panel that showed
    // everything. "Show turn stats" sat in the same block and is still
    // developer-only, so it must NOT be reachable here. Asserted by ROLE and
    // accessible name -- a text query matches empty section headings.
    expect(screen.queryByRole("switch", { name: "Show turn stats" })).toBeNull();
  });

  it("turning thinking off writes thinking_mode and nothing else", async () => {
    localStorage.removeItem("settings-dev-mode");
    await renderSettings();
    await navigateTo("Models");
    const el = screen.getByLabelText("Thinking mode") as HTMLSelectElement;
    expect(el.value).toBe("auto");

    fireEvent.change(el, { target: { value: "off" } });
    await clickSave();

    expect(sentPatch()).toEqual({ thinking_mode: "off" });
  });

  // The specific row the PAI programme depends on, asserted by key rather than
  // by presence. `proactive_review_enabled` is refused unless delegation is on,
  // so a Settings panel where the review switch works and this one does not is
  // one where the feature can be switched on and can never run.
  it("the delegation switch is operable and writes ext_orchestrator_enabled", async () => {
    await openPanel();
    const el = screen.getByRole("switch", { name: "Delegation to saved roles" }) as HTMLInputElement;
    expect(el.checked).toBe(false);

    fireEvent.click(el);
    await clickSave();

    expect(sentPatch()).toEqual({ ext_orchestrator_enabled: true });
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

// ── The full catalogue, reached from the classic Settings list ──────────────

describe("all-settings catalogue", () => {
  beforeEach(() => {
    vi.mocked(api.getSettings).mockResolvedValue(serverSettings() as never);
  });

  // This file registers cleanup per describe block rather than globally; without
  // it the previous render stays mounted and every getByText finds two.
  afterEach(() => {
    cleanup();
  });

  it("opens from the settings list and comes back", async () => {
    await renderSettings();

    fireEvent.click(screen.getByRole("button", { name: /All settings/ }));

    // The catalogue brings its own header, so the section's twelve topic rows
    // are gone and the categories are in their place.
    await screen.findByText("Everything this pond is, knows, hears, and is allowed to do.");
    expect(screen.getByRole("button", { name: /Privacy & Security/ })).toBeTruthy();
    expect(screen.queryByText("Your home, your assistant, and the models that power it.")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "← Back" }));
    await screen.findByText("Your home, your assistant, and the models that power it.");
  });

  it("leaves the twelve topic screens reachable", async () => {
    // The catalogue is a sibling of the existing panels, not a replacement —
    // this asserts the classic route into one topic still works.
    await renderSettings();
    await navigateTo("Models");
    expect(screen.getByText("← Back")).toBeTruthy();
  });
});
