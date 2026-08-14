import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { SettingsCatalogueView, summariseRetitle } from "./SettingsCatalogue";
import { api } from "../api/PondApiClient";

// ── Mocks ─────────────────────────────────────────────────────

const SERVER_SETTINGS = {
  user_name: "Jerry",
  assistant_name: "Goose",
  timezone: "Africa/Nairobi",
  mic_enabled: true,
  cameras_enabled: true,
  vision_enabled: false,
  memory_extraction_enabled: true,
  unprompted_speech_enabled: false,
  network_mode: "open",
  quiet_hours_start: "22:00",
  quiet_hours_end: "07:00",
  unprompted_speech_categories: "alert",
  weather_latitude: -1.286,
  weather_longitude: 36.817,
  matter_ws_url: "ws://127.0.0.1:5580/ws",
  chat_model: "gemma-4-E4B",
  agent_max_turns: 50,
  tool_call_validation: true,
};

const MODELS = [
  { id: "1", provider: "gguf", name: "gemma-4-E4B", is_active: true, downloaded: true },
  { id: "2", provider: "gguf", name: "qwen3-1.7b", is_active: false, downloaded: true },
  { id: "3", provider: "whisper", name: "base", is_active: true, downloaded: true },
];

function serverSettings(overrides: Record<string, unknown> = {}) {
  return { ...structuredClone(SERVER_SETTINGS), ...overrides };
}

vi.mock("../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listModels: vi.fn(),
    retitleSessions: vi.fn(),
  },
}));

const mockApi = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  updateSettings: ReturnType<typeof vi.fn>;
  listModels: ReturnType<typeof vi.fn>;
  retitleSessions: ReturnType<typeof vi.fn>;
};

/** A re-titling reply with the boring fields filled in. */
function retitleReply(over: Record<string, unknown> = {}) {
  return {
    renamed: [],
    renamed_count: 0,
    considered: 0,
    capped: false,
    unusable: 0,
    failed: 0,
    skipped: { user_named: 0, still_current: 0, too_short: 0, unknown_provenance: 0 },
    ...over,
  };
}

/** Render and wait for the first paint after settings load. */
async function renderPage(overrides: Record<string, unknown> = {}) {
  mockApi.getSettings.mockResolvedValue(serverSettings(overrides));
  mockApi.listModels.mockResolvedValue(MODELS);
  mockApi.updateSettings.mockImplementation(async (patch: Record<string, unknown>) =>
    serverSettings({ ...overrides, ...patch }));
  const view = render(<SettingsCatalogueView />);
  await screen.findByText("Who lives here");
  return view;
}

/** The row that owns a given control, found by its accessible name. */
function rowFor(label: string): HTMLElement {
  const control = screen.getByLabelText(label);
  const row = control.closest(".scat__row");
  if (!row) throw new Error(`no row for "${label}"`);
  return row as HTMLElement;
}

beforeEach(() => vi.clearAllMocks());
afterEach(cleanup);

describe("SettingsCatalogue", () => {
  it("shows what the pond may do as a sentence, from live values", async () => {
    await renderPage();
    const sentence = document.querySelector(".scat__sentence")!;
    expect(sentence.textContent).toContain("listens");
    expect(sentence.textContent).toContain("watches nothing");
    expect(sentence.textContent).toContain("speaks only when spoken to");
    expect(sentence.textContent).toContain("any host on the internet");
  });

  it("rewrites the sentence when the reach changes, without saving", async () => {
    await renderPage({ network_mode: "offline" });
    expect(document.querySelector(".scat__sentence")!.textContent)
      .toContain("Nothing leaves this house.");
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  it("does not offer a control that nothing reads", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));

    // `cameras_enabled` persists and renders, but no code reads it — the whole
    // reason this page exists. It must be visible, explained, and inoperable.
    const cameras = screen.getByLabelText("Cameras") as HTMLInputElement;
    expect(cameras.disabled).toBe(true);
    expect(rowFor("Cameras").textContent).toContain("Nothing reads this");

    // Its live neighbour is untouched.
    expect((screen.getByLabelText("Microphone") as HTMLInputElement).disabled).toBe(false);
  });

  it("counts the inert settings per category in the rail", async () => {
    await renderPage();
    const privacy = screen.getByRole("button", { name: /Privacy & Security/ });
    // cameras_enabled + cloud_fallback_enabled
    expect(within(privacy).getByTitle(/2 settings here that nothing reads/)).toBeTruthy();
  });

  it("blocks the save while a field is invalid, and says which", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Automation & Proactivity/ }));

    const quiet = screen.getByLabelText("Quiet from");
    fireEvent.change(quiet, { target: { value: "10pm" } });

    // The message names the fix rather than the rule.
    await screen.findByText(/Use a 24-hour time like 22:00/);
    const save = screen.getByRole("button", { name: /Fix 1 field/ }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.click(save);
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  it("sends only what changed, and adopts the value the server answers with", async () => {
    await renderPage();
    fireEvent.change(screen.getByLabelText("Your name"), { target: { value: "Anyumba" } });

    const save = await screen.findByRole("button", { name: /Save 1 change/ });
    fireEvent.click(save);

    await waitFor(() => expect(mockApi.updateSettings).toHaveBeenCalledTimes(1));
    // A patch, not the whole object.
    expect(mockApi.updateSettings).toHaveBeenCalledWith({ user_name: "Anyumba" });
    // The action keeps its name through the flow — "Save" becomes "Saved" —
    // and the panel settles clean, so the next save does not resend it.
    await screen.findByRole("button", { name: /^Saved$/ });
  });

  it("shows the server's own words when a save is refused", async () => {
    await renderPage();
    mockApi.updateSettings.mockRejectedValueOnce(
      new Error('network_mode "opn" is not one of ["open", "allowlist", "offline"]'));

    fireEvent.change(screen.getByLabelText("Your name"), { target: { value: "X" } });
    fireEvent.click(await screen.findByRole("button", { name: /Save 1 change/ }));

    // Verbatim, not routed through friendlyMessage — the 422 names the field
    // and the accepted values, and that sentence is the entire reason it failed.
    await screen.findByText(/is not one of/);
  });

  it("fills pickers from the model registry", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    const model = screen.getByLabelText("Model") as HTMLSelectElement;
    const values = [...model.options].map((o) => o.value);
    expect(values).toContain("gemma-4-E4B");
    expect(values).toContain("qwen3-1.7b");
    // Whisper models belong to the ASR picker, not this one.
    expect(values).not.toContain("base");
  });

  it("keeps a configured model the registry does not list", async () => {
    await renderPage({ chat_model: "some-model-i-removed" });
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    const model = screen.getByLabelText("Model") as HTMLSelectElement;
    expect(model.value).toBe("some-model-i-removed");
    expect([...model.options].map((o) => o.text)).toContain("some-model-i-removed — not installed");
  });

  it("offers the device's own time zone only when it differs", async () => {
    const systemZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    await renderPage({ timezone: "UTC" });
    const detect = screen.getByRole("button", { name: new RegExp(`Use ${systemZone}`) });
    fireEvent.click(detect);
    await waitFor(() =>
      expect((screen.getByLabelText("Time zone") as HTMLSelectElement).value).toBe(systemZone));
  });

  it("searches across every category, not just the open one", async () => {
    await renderPage();
    // "Cameras" lives under Privacy; the camera pipeline under Vision.
    fireEvent.change(screen.getByLabelText("Search settings"), { target: { value: "camera" } });
    await screen.findByText(/results/);
    expect(screen.getByLabelText("Cameras")).toBeTruthy();
    expect(screen.getByLabelText("Camera address")).toBeTruthy();
  });

  it("renders consequential choices as radios", async () => {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Privacy & Security/ }));
    const group = screen.getByRole("radiogroup", { name: "Network reach" });
    const radios = within(group).getAllByRole("radio");
    expect(radios).toHaveLength(3);
    expect((within(group).getByRole("radio", { name: /Open/ }) as HTMLInputElement).checked).toBe(true);

    fireEvent.click(within(group).getByRole("radio", { name: /Offline/ }));
    // The sentence at the top answers immediately — before any save.
    await waitFor(() =>
      expect(document.querySelector(".scat__sentence")!.textContent)
        .toContain("Nothing leaves this house."));
  });

  it("degrades a picker to free text when the registry cannot be reached", async () => {
    mockApi.getSettings.mockResolvedValue(serverSettings());
    mockApi.listModels.mockRejectedValue(new Error("offline"));
    render(<SettingsCatalogueView />);
    await screen.findByText("Who lives here");
    fireEvent.click(screen.getByRole("button", { name: /^Models/ }));
    // A text box keeps the configured value visible; an empty dropdown hides it.
    const model = await screen.findByLabelText("Model");
    expect(model.tagName).toBe("INPUT");
    expect((model as HTMLInputElement).value).toBe("gemma-4-E4B");
  });

  it("offers a retry when settings cannot be loaded", async () => {
    mockApi.getSettings.mockRejectedValueOnce(new Error("ECONNREFUSED"));
    mockApi.listModels.mockResolvedValue([]);
    render(<SettingsCatalogueView />);
    const retry = await screen.findByRole("button", { name: /Try again/ });

    mockApi.getSettings.mockResolvedValue(serverSettings());
    fireEvent.click(retry);
    await screen.findByText("Who lives here");
  });
});

describe("summariseRetitle", () => {
  it("counts what it renamed, and says when there is more to do", () => {
    expect(summariseRetitle(retitleReply({ renamed_count: 1 }))).toBe("Renamed 1 conversation");
    expect(summariseRetitle(retitleReply({ renamed_count: 4 }))).toBe("Renamed 4 conversations");
    expect(summariseRetitle(retitleReply({ renamed_count: 20, capped: true })))
      .toBe("Renamed 20 conversations — press again for more");
  });

  /// The distinction the copy exists for: "nothing needed doing" and "nothing
  /// was allowed" look identical from a count alone, and a person told the
  /// first would press the button again expecting a different answer.
  it("separates nothing-to-do from nothing-allowed", () => {
    expect(summariseRetitle(retitleReply({ considered: 0 })))
      .toBe("No conversations to rename");
    expect(summariseRetitle(retitleReply({
      considered: 3, skipped: { user_named: 3, still_current: 0, too_short: 0, unknown_provenance: 0 },
    }))).toBe("All of these are named by hand");
    expect(summariseRetitle(retitleReply({
      considered: 3, skipped: { user_named: 0, still_current: 3, too_short: 0, unknown_provenance: 0 },
    }))).toBe("Nothing needed a new name");
  });

  it("reports a model that gave nothing usable, and an outright failure", () => {
    expect(summariseRetitle(retitleReply({ considered: 2, unusable: 2 })))
      .toBe("The model gave no usable name");
    expect(summariseRetitle(retitleReply({ considered: 2, failed: 2 })))
      .toBe("Could not rename any of them");
  });

  it("never claims success when nothing was renamed", () => {
    for (const over of [
      { considered: 5, failed: 5 },
      { considered: 5, unusable: 5 },
      { considered: 5, skipped: { user_named: 5, still_current: 0, too_short: 0, unknown_provenance: 0 } },
    ]) {
      expect(summariseRetitle(retitleReply(over))).not.toMatch(/^Renamed/);
    }
  });
});

describe("the rename-now button", () => {
  async function openAutomation() {
    await renderPage();
    fireEvent.click(screen.getByRole("button", { name: /Automation & Proactivity/ }));
  }

  it("renames on demand and reports what happened", async () => {
    mockApi.retitleSessions.mockResolvedValue(retitleReply({
      renamed_count: 2,
      renamed: [
        { session_id: "a", title: "Wake word fires twice" },
        { session_id: "b", title: "Jetson build stamp is lying" },
      ],
      considered: 5,
    }));
    await openAutomation();

    fireEvent.click(screen.getByRole("button", { name: /Rename now/ }));

    await screen.findByText("Renamed 2 conversations");
    expect(mockApi.retitleSessions).toHaveBeenCalledTimes(1);
    // Renaming is not a settings change; it must not dirty the save button.
    expect(mockApi.updateSettings).not.toHaveBeenCalled();
  });

  /// One model call per conversation, so a pass is slow on a small board. The
  /// button has to say so and refuse to be pressed twice.
  it("says it is working and cannot be pressed again mid-run", async () => {
    let release!: (v: unknown) => void;
    mockApi.retitleSessions.mockReturnValue(new Promise((r) => { release = r; }));
    await openAutomation();

    fireEvent.click(screen.getByRole("button", { name: /Rename now/ }));

    const busy = await screen.findByRole("button", { name: /Renaming/ });
    expect((busy as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(busy);
    expect(mockApi.retitleSessions).toHaveBeenCalledTimes(1);

    release(retitleReply({ renamed_count: 1, considered: 1 }));
    await screen.findByText("Renamed 1 conversation");
  });

  it("shows the failure rather than a silent no-op", async () => {
    mockApi.retitleSessions.mockRejectedValue(new Error("No language model is configured"));
    await openAutomation();

    fireEvent.click(screen.getByRole("button", { name: /Rename now/ }));

    await screen.findByText("No language model is configured");
    // Recoverable: the button comes back rather than staying stuck on "Renaming".
    await waitFor(() =>
      expect((screen.getByRole("button", { name: /Rename now/ }) as HTMLButtonElement).disabled)
        .toBe(false));
  });

  /// The toggle governs what happens unattended. A button that silently did
  /// nothing because of a switch elsewhere on the same page is the worse
  /// surprise, so it is offered either way.
  it("is offered even when the automatic pass is switched off", async () => {
    mockApi.retitleSessions.mockResolvedValue(retitleReply());
    await renderPage({ session_titling_enabled: false });
    fireEvent.click(screen.getByRole("button", { name: /Automation & Proactivity/ }));

    const button = screen.getByRole("button", { name: /Rename now/ }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
  });
});
