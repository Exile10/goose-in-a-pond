import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listModels: vi.fn(),
    synthesizeSpeech: vi.fn(),
    applyTtsSettings: vi.fn(),
    getDownloadProgress: vi.fn(),
  },
}));

import { StepPersonality } from "./StepPersonality";
import { OnboardingProvider } from "../OnboardingContext";
import { api } from "../../../api/PondApiClient";

function renderStep() {
  return render(
    <OnboardingProvider>
      <StepPersonality />
    </OnboardingProvider>,
  );
}

/**
 * The voice buttons the picker is offering.
 *
 * Scoped to the picker's own group: the conversation-style cards above are
 * also radios, and a bare `getAllByRole("radio")` counts those too.
 */
function voiceOptions(): HTMLButtonElement[] {
  const group = document.querySelector(".vpick__voices");
  return group ? (Array.from(group.querySelectorAll("button")) as HTMLButtonElement[]) : [];
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getSettings).mockResolvedValue({} as never);
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  vi.mocked(api.synthesizeSpeech).mockResolvedValue(new ArrayBuffer(8) as never);
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: [] } as never);
  // The server echoes back the tier it actually put in force. Normally that is
  // the one it was asked for; see the substitution test below for when it isn't.
  vi.mocked(api.applyTtsSettings).mockResolvedValue({
    voice: "af_bella",
    speed: 1,
    quality: "q8f16",
    downloaded_voice: true,
    downloaded_weights: false,
    engine_reloaded: false,
    installed_voices: ["af_heart", "af_bella"],
  } as never);
  vi.mocked(api.listModels).mockResolvedValue([
    { id: "1", provider: "tts_kokoro", name: "af_heart", is_active: true, downloaded: true },
    { id: "2", provider: "tts_kokoro", name: "af_bella", is_active: false, downloaded: false },
    { id: "3", provider: "tts_kokoro", name: "bm_george", is_active: false, downloaded: false },
    { id: "4", provider: "gguf", name: "some-llm", is_active: false, downloaded: true },
  ] as never);
});

// Onboarding used to ship four hardcoded voice cards whose ids — "amy",
// "kathleen", "libritts" — named nothing that existed, so every new install
// wrote a `voice_tts_voice` the resolver could never match. It now uses the
// same picker as the Models page, driven by the real catalogue.
describe("Onboarding voice step", () => {
  it("offers real catalogued voices, not a hardcoded list", async () => {
    renderStep();

    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));
    const names = voiceOptions().map((b) => b.textContent ?? "");
    expect(names.some((n) => n.includes("Heart"))).toBe(true);
    expect(names.some((n) => n.includes("Amy"))).toBe(false);
    expect(names.some((n) => n.includes("some-llm"))).toBe(false);
  });

  it("applies the chosen voice to the running engine and speaks it", async () => {
    renderStep();
    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));

    const bella = voiceOptions().find((b) => b.textContent?.includes("Bella"))!;
    fireEvent.click(bella);

    await waitFor(() =>
      expect(vi.mocked(api.applyTtsSettings)).toHaveBeenCalledWith({
        voice: "af_bella",
        quality: "q8f16",
      }),
    );
    await waitFor(() => expect(vi.mocked(api.synthesizeSpeech)).toHaveBeenCalled());
  });

  // The rate was collected by a slider and then dropped on the floor — the
  // wizard never wrote it anywhere.
  it("persists the pace it collects", async () => {
    renderStep();
    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));

    fireEvent.change(screen.getByLabelText("Speaking pace"), { target: { value: "130" } });

    await waitFor(
      () => expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ voice_tts_speed: 1.3 }),
      { timeout: 2000 },
    );
  });

  // Twenty American voices is a decision nobody asked to make before hearing
  // the assistant speak once. Setup shows the three best-graded per accent.
  it("offers at most three voices per accent, best-graded first", async () => {
    vi.mocked(api.listModels).mockResolvedValue([
      // Deliberately out of grade order, and more than three.
      { id: "1", provider: "tts_kokoro", name: "am_adam", is_active: false, downloaded: true },   // F+
      { id: "2", provider: "tts_kokoro", name: "af_sky", is_active: false, downloaded: true },    // C-
      { id: "3", provider: "tts_kokoro", name: "af_heart", is_active: true, downloaded: true },   // A
      { id: "4", provider: "tts_kokoro", name: "af_bella", is_active: false, downloaded: true },  // A-
      { id: "5", provider: "tts_kokoro", name: "af_nicole", is_active: false, downloaded: true }, // B-
      { id: "6", provider: "tts_kokoro", name: "af_jessica", is_active: false, downloaded: true },// D
    ] as never);
    renderStep();

    await waitFor(() => expect(voiceOptions().length).toBe(3));
    const names = voiceOptions().map((b) => b.textContent ?? "");
    expect(names[0]).toContain("Heart");
    expect(names[1]).toContain("Bella");
    expect(names[2]).toContain("Nicole");
    expect(names.some((n) => n.includes("Adam"))).toBe(false);
  });

  // Setup commits to the smallest tier, and every preview must come from it —
  // judging a voice on one tier and living with another is the trap.
  it("uses the compact tier and applies it with every sample", async () => {
    renderStep();
    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));

    await waitFor(() =>
      expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ voice_tts_quality: "q8f16" }),
    );

    fireEvent.click(voiceOptions().find((b) => b.textContent?.includes("Bella"))!);
    await waitFor(() =>
      expect(vi.mocked(api.applyTtsSettings)).toHaveBeenCalledWith({
        voice: "af_bella",
        quality: "q8f16",
      }),
    );
  });

  // Some tiers cannot produce audio on some machines: asked for q8f16, a Jetson
  // answers q4f16, because q8f16 synthesises pure silence there. Setup has to
  // store what is running rather than what it requested, or the household ends
  // up with a saved tier the engine is not using — and every later preview
  // would be judged on the wrong one.
  it("stores the tier the server actually used, not the one it asked for", async () => {
    vi.mocked(api.applyTtsSettings).mockResolvedValue({
      voice: "af_heart",
      speed: 1,
      quality: "q4f16",
      downloaded_voice: false,
      downloaded_weights: true,
      engine_reloaded: true,
      installed_voices: ["af_heart"],
    } as never);
    renderStep();

    await waitFor(() =>
      expect(vi.mocked(api.updateSettings)).toHaveBeenCalledWith({ voice_tts_quality: "q4f16" }),
    );

    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));
    fireEvent.click(voiceOptions().find((b) => b.textContent?.includes("Bella"))!);
    await waitFor(() =>
      expect(vi.mocked(api.applyTtsSettings)).toHaveBeenCalledWith({
        voice: "af_bella",
        quality: "q4f16",
      }),
    );
  });

  it("marks the voices that will need downloading", async () => {
    renderStep();
    await waitFor(() => expect(voiceOptions().length).toBeGreaterThan(0));

    const heart = voiceOptions().find((b) => b.textContent?.includes("Heart"))!;
    const bella = voiceOptions().find((b) => b.textContent?.includes("Bella"))!;
    expect(heart.getAttribute("title")).not.toContain("downloads when selected");
    expect(bella.getAttribute("title")).toContain("downloads when selected");
  });

  /// Offline, the step must still let someone through rather than blocking
  /// setup on a catalogue it could not reach.
  it("still renders when the catalogue is unreachable", async () => {
    vi.mocked(api.listModels).mockRejectedValue(new Error("offline"));
    renderStep();

    await waitFor(() => expect(screen.getByLabelText("Speaking pace")).toBeTruthy());
  });
});
