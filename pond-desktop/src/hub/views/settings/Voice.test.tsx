import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listModels: vi.fn(),
    synthesizeSpeech: vi.fn(),
    resetWakeWordCalibration: vi.fn().mockResolvedValue(undefined),
  },
}));

import { VoiceDetail } from "./Voice";
import { api } from "../../../api/PondApiClient";
import { PREVIEW_STATEMENTS } from "../../../voice/voiceCatalogue";

/** The fields this view reads, at their shipped defaults. */
function voiceSettings(overrides: Record<string, unknown> = {}) {
  return {
    voice_wake_word: "goose",
    voice_wake_word_transcriptions: [] as string[],
    active_whisper_model: "ggml-base.bin",
    voice_tts_voice: "af_heart",
    voice_tts_speed: 1.0,
    voice_tts_quality: "q8",
    ...overrides,
  };
}

async function renderVoice() {
  render(<VoiceDetail go={() => {}} />);
  await screen.findByText("Sound while it thinks");
}

/** The tone row's switch, found via its own row rather than by index. */
function toneSwitch(): HTMLButtonElement {
  const label = screen.getByText("Sound while it thinks");
  const row = label.closest(".srow");
  if (!row) throw new Error("tone row not found");
  const btn = row.querySelector("button.htoggle");
  if (!btn) throw new Error("tone row has no switch");
  return btn as HTMLButtonElement;
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  vi.mocked(api.listModels).mockResolvedValue([
    { id: "1", provider: "tts_kokoro", name: "af_heart", is_active: true, downloaded: true },
    { id: "2", provider: "tts_kokoro", name: "bm_george", is_active: false, downloaded: true },
    { id: "3", provider: "gguf", name: "some-llm", is_active: false, downloaded: true },
  ] as never);
  vi.mocked(api.synthesizeSpeech).mockResolvedValue(new ArrayBuffer(8) as never);
});

/** The pace slider — the picker's range input, by its accessible name. */
function paceSlider(): HTMLInputElement {
  return screen.getByLabelText("Speaking pace") as HTMLInputElement;
}

/** The voice buttons currently offered (one accent at a time). */
function voiceOptions(): HTMLButtonElement[] {
  return screen.getAllByRole("radio") as HTMLButtonElement[];
}

// The tone was deleted outright in 0136f8c5 and restored behind this setting.
// The Hub is the second surface for it, and the one whose Toggle keeps its own
// internal state — so these assert what is DRAWN after the async settings load,
// not merely that a control exists.
describe("Hub voice settings — thinking tone", () => {
  it("draws ON when the key is absent", async () => {
    // The field defaults ON in Rust, so a settings row written before this
    // setting existed carries no such key.
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    expect(toneSwitch().getAttribute("aria-pressed")).toBe("true");
  });

  it("draws OFF when the stored setting is off", async () => {
    // The regression this guards: Toggle seeds from its prop once via useState
    // and never re-reads it, while settings arrive a render after mount. Without
    // the remount key in Voice.tsx this renders ON and the switch lies about a
    // setting the household already changed.
    vi.mocked(api.getSettings).mockResolvedValue(
      voiceSettings({ voice_thinking_tone_enabled: false }) as never,
    );
    await renderVoice();

    await waitFor(() => {
      expect(toneSwitch().getAttribute("aria-pressed")).toBe("false");
    });
  });

  it("writes voice_thinking_tone_enabled and nothing else", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    fireEvent.click(toneSwitch());

    await waitFor(() => {
      expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({
        voice_thinking_tone_enabled: false,
      });
    });
  });
});

// Pace is stored as the multiplier the engine's `speed` tensor takes, while
// the slider works in percent. That conversion is the kind of thing that
// silently ships backwards, so it is asserted in both directions.
describe("Hub voice settings \u2014 speaking pace", () => {
  it("draws the saved pace rather than the default", async () => {
    // The range is uncontrolled (a controlled one fights the drag), so it
    // seeds from `defaultValue` once. Settings land a render after mount \u2014
    // without the remount key the handle sits at 100% while the saved pace is
    // 130%.
    vi.mocked(api.getSettings).mockResolvedValue(
      voiceSettings({ voice_tts_speed: 1.3 }) as never,
    );
    await renderVoice();

    await waitFor(() => expect(paceSlider().value).toBe("130"));
    expect(screen.getByText(/1\.30/)).toBeTruthy();
  });

  it("persists a multiplier, not a percentage", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    fireEvent.change(paceSlider(), { target: { value: "150" } });

    await waitFor(
      () => {
        expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ voice_tts_speed: 1.5 });
      },
      { timeout: 2000 },
    );
  });

  it("speaks a natural statement once the pace settles", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    fireEvent.change(paceSlider(), { target: { value: "80" } });

    await waitFor(() => expect(vi.mocked(api.synthesizeSpeech)).toHaveBeenCalled(), {
      timeout: 2000,
    });
    // Whatever it says must be one of the product's own lines \u2014 not a demo
    // phrase, and not the same one every time.
    const spoken = vi.mocked(api.synthesizeSpeech).mock.calls[0][0];
    expect(PREVIEW_STATEMENTS).toContain(spoken);
  });

  // Hearing one sentence on repeat tells you nothing by the third play.
  //
  // Driven by two voice changes rather than two clicks of the play button:
  // that button toggles, so a second click stops the current sample instead
  // of starting another.
  it("moves through the statements rather than repeating one", async () => {
    vi.mocked(api.listModels).mockResolvedValue([
      { id: "1", provider: "tts_kokoro", name: "af_heart", is_active: true, downloaded: true },
      { id: "2", provider: "tts_kokoro", name: "af_bella", is_active: false, downloaded: true },
    ] as never);
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    await waitFor(() => expect(voiceOptions().length).toBe(2));
    fireEvent.click(voiceOptions().find((b) => b.textContent?.includes("Bella"))!);
    await waitFor(() => expect(vi.mocked(api.synthesizeSpeech)).toHaveBeenCalledTimes(1));
    fireEvent.click(voiceOptions().find((b) => b.textContent?.includes("Heart"))!);
    await waitFor(() => expect(vi.mocked(api.synthesizeSpeech)).toHaveBeenCalledTimes(2));

    const first = vi.mocked(api.synthesizeSpeech).mock.calls[0][0];
    const second = vi.mocked(api.synthesizeSpeech).mock.calls[1][0];
    expect(second).not.toBe(first);
    expect(PREVIEW_STATEMENTS).toContain(second);
  });
});

describe("Hub voice settings \u2014 voice and quality", () => {
  it("offers the installed voices for the selected accent, and ignores non-TTS models", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));
    const names = voiceOptions().map((b) => b.textContent ?? "");
    expect(names.some((n) => n.includes("Heart"))).toBe(true);
    expect(names.some((n) => n.includes("some-llm"))).toBe(false);
  });

  // The grade is the one thing a name and an accent cannot tell you, and it
  // comes from Kokoro's own VOICES.md.
  it("shows Kokoro's published grade for a voice", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));
    const heart = voiceOptions().find((b) => b.textContent?.includes("Heart"));
    expect(heart?.textContent).toContain("A");
    expect(screen.getByText(/grade A/)).toBeTruthy();
  });

  // A saved voice the catalogue has not returned must still be the selected
  // one \u2014 otherwise the picker silently reassigns the household's voice.
  it("keeps showing a saved voice the catalogue does not list", async () => {
    vi.mocked(api.listModels).mockResolvedValue([] as never);
    vi.mocked(api.getSettings).mockResolvedValue(
      voiceSettings({ voice_tts_voice: "am_michael" }) as never,
    );
    await renderVoice();

    await waitFor(() => {
      const on = voiceOptions().find((b) => b.getAttribute("aria-checked") === "true");
      expect(on?.textContent).toContain("Michael");
    });
  });

  it("writes the chosen voice and speaks it", async () => {
    vi.mocked(api.listModels).mockResolvedValue([
      { id: "1", provider: "tts_kokoro", name: "af_heart", is_active: true, downloaded: true },
      { id: "2", provider: "tts_kokoro", name: "af_bella", is_active: false, downloaded: true },
    ] as never);
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    await waitFor(() => expect(voiceOptions().length).toBe(2));
    const bella = voiceOptions().find((b) => b.textContent?.includes("Bella"))!;
    fireEvent.click(bella);

    await waitFor(() => {
      expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ voice_tts_voice: "af_bella" });
    });
    await waitFor(() => expect(vi.mocked(api.synthesizeSpeech)).toHaveBeenCalled());
  });

  it("writes the chosen quality tier", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(voiceSettings() as never);
    await renderVoice();

    fireEvent.change(screen.getByLabelText("Voice quality"), { target: { value: "fp16" } });

    await waitFor(() => {
      expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ voice_tts_quality: "fp16" });
    });
  });

  it("defaults quality to the balanced tier when unset", async () => {
    vi.mocked(api.getSettings).mockResolvedValue(
      voiceSettings({ voice_tts_quality: undefined }) as never,
    );
    await renderVoice();

    const select = screen.getByLabelText("Voice quality") as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe("q8"));
  });
});
