// What the pond is using to hear and speak, and why, in the household's words. Pure, so both
// settings surfaces say exactly the same thing and the sentences can be tested.

import type {
  AudioChoice,
  AudioDeviceStatus,
  AudioProbeOutcome,
  ProbedAudioDevice,
} from "../api/types";

export interface ChoiceLine {
  /** One sentence: what is in use, and why. */
  text: string;
  /** Something the household should act on; the sentence says what. */
  problem: boolean;
}

const NOUN = { input: "microphone", output: "speaker" } as const;

function heard(outcome: AudioProbeOutcome): string {
  switch (outcome.result) {
    case "hearing":
      return "hearing sound";
    case "silent":
      return "hearing only silence";
    case "accepting":
      return "taking audio";
    case "stalled":
      return "not responding";
    case "failed":
      return "not opening";
    case "untested":
      return "not tested yet";
  }
}

/** Why a passed-over device lost: by its outcome, or "not connected" when it is not listed. */
function whyNot(device: ProbedAudioDevice | undefined): string {
  if (!device) return "is not connected";
  switch (device.outcome.result) {
    case "silent":
      return "hears only silence";
    case "stalled":
      return "did not respond";
    case "failed":
      return device.outcome.reason.includes("not connected")
        ? "is not connected"
        : "would not open";
    default:
      return "did not pass its test";
  }
}

const SILENCE_HINT = "Check its mute switch, and that the pond may use the microphone.";

export function describeChoice(choice: AudioChoice): ChoiceLine {
  const noun = NOUN[choice.direction];
  const device = choice.device;
  const state = heard(choice.outcome);
  const silent = choice.outcome.result === "silent";
  const hint = silent ? ` ${SILENCE_HINT}` : "";

  switch (choice.basis) {
    case "mic_off":
      return { text: "Off in Privacy. The pond is not listening.", problem: false };
    case "no_device":
      return { text: `No ${noun} found. Connect one and the pond starts using it.`, problem: true };
    case "not_tested":
      return {
        text: device
          ? `${device.label} will be tried first. It is tested when voice starts, or when you test again.`
          : `Not tested yet. It is tested when voice starts, or when you test again.`,
        problem: false,
      };
  }
  if (!device) return { text: `No ${noun} found.`, problem: true };

  switch (choice.basis) {
    case "pinned":
      return { text: `${device.label}, your choice, ${state}.${hint}`, problem: silent };
    case "system_default":
      return { text: `${device.label}, the system default, ${state}.${hint}`, problem: silent };
    case "fallback": {
      const pin = choice.pinned;
      if (pin && pin !== device.id) {
        const pinned = choice.devices.find((d) => d.id === pin);
        const name = pinned?.label ?? pin;
        return {
          text: `${device.label}, ${state}: your choice, ${name}, ${whyNot(pinned)}.${hint}`,
          problem: silent,
        };
      }
      const def = choice.devices.find((d) => d.is_default && d.id !== device.id);
      return {
        text: def
          ? `${device.label}, ${state}: the system default, ${def.label}, ${whyNot(def)}.${hint}`
          : `${device.label}, ${state}.${hint}`,
        problem: silent,
      };
    }
    case "best_effort":
      return {
        text: silent
          ? `${device.label}, but every ${noun} hears only silence. ${SILENCE_HINT}`
          : `${device.label}, though no ${noun} passed its test. Check that it is connected and switched on.`,
        problem: true,
      };
  }
  return { text: `${device.label}, ${state}.`, problem: false };
}

/** "Tested 12 seconds ago by the voice session." Says which process, since only voice hears. */
export function describeWhen(status: AudioDeviceStatus, now = Date.now()): string {
  const secs = Math.max(0, Math.round((now - status.updated_at_ms) / 1000));
  const ago =
    secs < 5
      ? "just now"
      : secs < 90
        ? `${secs} seconds ago`
        : secs < 5400
          ? `${Math.round(secs / 60)} minutes ago`
          : `${Math.round(secs / 3600)} hours ago`;
  const by = status.source === "voice" ? "by the voice session" : "by the pond";
  return `Checked ${ago} ${by}.`;
}

export interface DeviceOption {
  value: string;
  label: string;
}

/** Picker rows: "Automatic" first, then each device with what its last test found. A pin that
 *  is not connected stays listed, or opening the picker would silently drop it. */
export function deviceOptions(choice: AudioChoice | undefined, pinned: string): DeviceOption[] {
  const rows: DeviceOption[] = [{ value: "", label: "Automatic" }];
  const devices = choice?.devices ?? [];
  for (const d of devices) {
    const parts: string[] = [];
    if (d.is_default) parts.push("system default");
    if (d.outcome.result !== "untested") parts.push(heard(d.outcome));
    rows.push({
      value: d.id,
      label: parts.length ? `${d.label} (${parts.join(", ")})` : d.label,
    });
  }
  if (pinned && !devices.some((d) => d.id === pinned)) {
    rows.push({ value: pinned, label: `${pinned} (not connected)` });
  }
  return rows;
}

/** A body that is not a status (an HTML error page, an empty 204) must not crash the panel. */
export function isAudioStatus(v: unknown): v is AudioDeviceStatus {
  const s = v as AudioDeviceStatus | null;
  return Boolean(s && typeof s === "object" && s.input && s.output && Array.isArray(s.input.devices));
}
