import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getAudioDevices: vi.fn(),
    testAudioDevices: vi.fn(),
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import type { AudioDeviceStatus } from "../api/types";
import { AudioDevicesPanel } from "./AudioDevicesPanel";

function status(over: Partial<AudioDeviceStatus> = {}): AudioDeviceStatus {
  const mic = { id: "MacBook Air Microphone", label: "MacBook Air Microphone", is_default: true, kind: "hardware" as const };
  const usb = { id: "USB mic", label: "USB mic", is_default: false, kind: "hardware" as const };
  const speakers = { id: "MacBook Air Speakers", label: "MacBook Air Speakers", is_default: true, kind: "hardware" as const };
  return {
    input: {
      direction: "input",
      device: mic,
      outcome: { result: "hearing", level_dbfs: -50 },
      basis: "system_default",
      pinned: null,
      devices: [
        { ...mic, outcome: { result: "hearing", level_dbfs: -50 } },
        { ...usb, outcome: { result: "untested" } },
      ],
    },
    output: {
      direction: "output",
      device: speakers,
      outcome: { result: "accepting" },
      basis: "system_default",
      pinned: null,
      devices: [{ ...speakers, outcome: { result: "accepting" } }],
    },
    updated_at_ms: Date.now(),
    source: "voice",
    pid: 7,
    ...over,
  };
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getAudioDevices).mockResolvedValue(status());
  vi.mocked(api.testAudioDevices).mockResolvedValue(status());
  vi.mocked(api.getSettings).mockResolvedValue({ audio_input_device: "", audio_test_chime: false });
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
});

describe("AudioDevicesPanel", () => {
  it("says what is in use and why, for both directions", async () => {
    render(<AudioDevicesPanel variant="catalogue" />);
    expect(
      await screen.findByText("MacBook Air Microphone, the system default, hearing sound."),
    ).toBeTruthy();
    expect(screen.getByText("MacBook Air Speakers, the system default, taking audio.")).toBeTruthy();
    expect(screen.getByText(/by the voice session/)).toBeTruthy();
    // The catalogue has its own rows for the pins.
    expect(screen.queryByLabelText("Microphone to use")).toBeNull();
  });

  it("tests again silently, and plays a sound only when asked", async () => {
    render(<AudioDevicesPanel variant="catalogue" />);
    await screen.findByText(/hearing sound/);

    fireEvent.click(screen.getByRole("button", { name: /Test again/ }));
    await screen.findByText("Tested every device just now.");
    expect(api.testAudioDevices).toHaveBeenLastCalledWith(false);

    fireEvent.click(screen.getByRole("button", { name: /Play a test sound/ }));
    await screen.findByText("Played a test sound on MacBook Air Speakers.");
    expect(api.testAudioDevices).toHaveBeenLastCalledWith(true);
  });

  it("pins the device the household picks, and Automatic unpins it", async () => {
    render(<AudioDevicesPanel variant="hub" withPickers />);
    const picker = (await screen.findByLabelText("Microphone to use")) as HTMLSelectElement;
    await waitFor(() => expect(picker.disabled).toBe(false));

    fireEvent.change(picker, { target: { value: "USB mic" } });
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({ audio_input_device: "USB mic" }),
    );
    expect(screen.getByText("Saved. Testing it now.")).toBeTruthy();

    fireEvent.change(picker, { target: { value: "" } });
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenLastCalledWith({ audio_input_device: "" }),
    );
  });

  it("marks a silent microphone as a problem in words", async () => {
    const s = status();
    s.input.outcome = { result: "silent" };
    s.input.basis = "best_effort";
    vi.mocked(api.getAudioDevices).mockResolvedValue(s);
    render(<AudioDevicesPanel variant="catalogue" />);
    const line = await screen.findByText(/every microphone hears only silence/);
    expect(line.getAttribute("data-problem")).toBe("true");
  });

  it("explains an unreachable pond instead of showing nothing", async () => {
    vi.mocked(api.getAudioDevices).mockRejectedValue(new Error("offline"));
    render(<AudioDevicesPanel variant="catalogue" />);
    expect(await screen.findByText(/not answering/)).toBeTruthy();
  });
});
