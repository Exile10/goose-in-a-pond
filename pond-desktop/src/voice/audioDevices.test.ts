import { describe, expect, it } from "vitest";
import type { AudioChoice, AudioDeviceStatus, ProbedAudioDevice } from "../api/types";
import { describeChoice, describeWhen, deviceOptions, isAudioStatus } from "./audioDevices";

function device(id: string, outcome: ProbedAudioDevice["outcome"], is_default = false): ProbedAudioDevice {
  return { id, label: id, is_default, kind: "hardware", outcome };
}

function choice(over: Partial<AudioChoice>): AudioChoice {
  return {
    direction: "input",
    device: null,
    outcome: { result: "untested" },
    basis: "not_tested",
    pinned: null,
    devices: [],
    ...over,
  };
}

const builtIn = device("MacBook Air Microphone", { result: "hearing", level_dbfs: -48 }, true);

describe("describeChoice", () => {
  it("names the default and what its test found", () => {
    const line = describeChoice(
      choice({ device: builtIn, outcome: builtIn.outcome, basis: "system_default", devices: [builtIn] }),
    );
    expect(line).toEqual({
      text: "MacBook Air Microphone, the system default, hearing sound.",
      problem: false,
    });
  });

  it("says why an unplugged pin was passed over", () => {
    const line = describeChoice(
      choice({
        device: builtIn,
        outcome: builtIn.outcome,
        basis: "fallback",
        pinned: "USB mic",
        devices: [builtIn],
      }),
    );
    expect(line.text).toBe(
      "MacBook Air Microphone, hearing sound: your choice, USB mic, is not connected.",
    );
    expect(line.problem).toBe(false);
  });

  it("says why a broken default was passed over", () => {
    const broken = device("Built-in", { result: "failed", reason: "device busy" }, true);
    const usb = device("USB mic", { result: "hearing", level_dbfs: -50 });
    const line = describeChoice(
      choice({ device: usb, outcome: usb.outcome, basis: "fallback", devices: [broken, usb] }),
    );
    expect(line.text).toBe("USB mic, hearing sound: the system default, Built-in, would not open.");
  });

  it("flags a silent microphone and says what to check", () => {
    const silent = device("Headset", { result: "silent" });
    const line = describeChoice(
      choice({ device: silent, outcome: silent.outcome, basis: "pinned", pinned: "Headset", devices: [silent] }),
    );
    expect(line.problem).toBe(true);
    expect(line.text).toContain("mute switch");
  });

  it("is plain about every mic being silent", () => {
    const silent = device("Built-in", { result: "silent" }, true);
    const line = describeChoice(
      choice({ device: silent, outcome: silent.outcome, basis: "best_effort", devices: [silent] }),
    );
    expect(line.problem).toBe(true);
    expect(line.text).toMatch(/every microphone hears only silence/);
  });

  it("treats the mic switch as a choice, not a fault", () => {
    expect(describeChoice(choice({ basis: "mic_off" }))).toEqual({
      text: "Off in Privacy. The pond is not listening.",
      problem: false,
    });
  });

  it("asks for a device when there is none", () => {
    const line = describeChoice(choice({ direction: "output", basis: "no_device" }));
    expect(line.problem).toBe(true);
    expect(line.text).toMatch(/No speaker found/);
  });

  it("does not claim a test that has not run", () => {
    const line = describeChoice(
      choice({ device: builtIn, basis: "not_tested", devices: [builtIn] }),
    );
    expect(line.text).toMatch(/will be tried first/);
    expect(line.problem).toBe(false);
  });
});

describe("deviceOptions", () => {
  it("offers Automatic, then each device with its default and its last result", () => {
    const phone = device("Phone", { result: "untested" });
    const opts = deviceOptions(choice({ devices: [builtIn, phone] }), "");
    expect(opts).toEqual([
      { value: "", label: "Automatic" },
      { value: "MacBook Air Microphone", label: "MacBook Air Microphone (default, hears sound)" },
      { value: "Phone", label: "Phone" },
    ]);
  });

  it("keeps a pin that is not connected, so opening the picker never drops it", () => {
    const opts = deviceOptions(choice({ devices: [builtIn] }), "USB mic");
    expect(opts.at(-1)).toEqual({ value: "USB mic", label: "USB mic (not connected)" });
  });
});

describe("describeWhen and isAudioStatus", () => {
  const status: AudioDeviceStatus = {
    input: choice({}),
    output: choice({ direction: "output" }),
    updated_at_ms: 1_000_000,
    source: "voice",
    pid: 1,
  };

  it("says when and which process checked", () => {
    expect(describeWhen(status, 1_000_000 + 2_000)).toBe("Checked just now by the voice session.");
    expect(describeWhen({ ...status, source: "serve" }, 1_000_000 + 30_000)).toBe(
      "Checked 30 seconds ago by the pond.",
    );
    expect(describeWhen(status, 1_000_000 + 600_000)).toBe("Checked 10 minutes ago by the voice session.");
  });

  it("rejects a body that is not a status", () => {
    expect(isAudioStatus(status)).toBe(true);
    expect(isAudioStatus(undefined)).toBe(false);
    expect(isAudioStatus("<html>")).toBe(false);
    expect(isAudioStatus({ input: {}, output: {} })).toBe(false);
  });
});
