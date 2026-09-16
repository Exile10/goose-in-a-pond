// Fixed-duration microphone capture, for wake-word calibration.
//
// This is the capture half of WebVoiceBackend.recordWithVad with the VAD taken
// out: calibration records for a fixed window and keeps everything, because
// the point is to hear exactly what the user said, not to decide when they
// stopped.
//
// It replaces four IPC commands and 811 lines of cpal in the Rust shell. Under
// Tauri the mic had to live natively; under Chromium getUserMedia is right
// there, and the WAV no longer has to cross a process boundary -- the old
// stop_recording returned Vec<u8>, which serialised as a JSON array of
// integers, so a 3.5s 16kHz mono recording travelled as roughly 400 KB of
// text and was copied back to bytes on the other side.
//
// It also means calibration works in the browser dev surface, where it used
// to render an error.

import { MIC_CONSTRAINTS, encodeWav, downsampleTo16k, calculateRms } from "./webAudioUtils";

/** How often to report a level, in ms. Matches the VAD frame rate. */
const LEVEL_INTERVAL_MS = 30;

const PROCESSOR_BUFFER = 4096;

export interface FixedRecording {
  /** 16 kHz mono WAV, ready for the calibration endpoint. */
  wav: ArrayBuffer;
  /** Loudest frame observed, so a caller can tell silence from speech. */
  peakRms: number;
}

export interface FixedRecorder {
  /** Resolves when the window closes; rejects if the mic could not be opened. */
  done: Promise<FixedRecording>;
  /** Stop early and discard. `done` rejects with an AbortError. */
  abort(): void;
}

/**
 * Record for `durationMs`, reporting the input level as it goes.
 *
 * The recorder owns its own AudioContext and closes it on the way out, so a
 * cancelled calibration cannot leave the microphone indicator lit.
 */
export function recordFixedDuration(
  durationMs: number,
  onLevel?: (rms: number) => void,
): FixedRecorder {
  let stop: ((reason?: Error) => void) | null = null;
  let aborted = false;

  const done = new Promise<FixedRecording>((resolve, reject) => {
    let ctx: AudioContext | null = null;
    let stream: MediaStream | null = null;
    let levelTimer: ReturnType<typeof setInterval> | null = null;
    let endTimer: ReturnType<typeof setTimeout> | null = null;
    let settled = false;

    const chunks: Float32Array[] = [];
    let peakRms = 0;

    const teardown = () => {
      if (levelTimer !== null) clearInterval(levelTimer);
      if (endTimer !== null) clearTimeout(endTimer);
      stream?.getTracks().forEach((t) => t.stop());
      void ctx?.close().catch(() => {});
    };

    stop = (reason?: Error) => {
      if (settled) return;
      settled = true;
      teardown();
      if (reason) {
        reject(reason);
        return;
      }
      if (ctx === null || chunks.length === 0) {
        resolve({ wav: encodeWav(new Float32Array(0), 16_000), peakRms });
        return;
      }
      const total = chunks.reduce((n, c) => n + c.length, 0);
      const all = new Float32Array(total);
      let at = 0;
      for (const c of chunks) {
        all.set(c, at);
        at += c.length;
      }
      resolve({ wav: encodeWav(downsampleTo16k(all, ctx.sampleRate), 16_000), peakRms });
    };

    navigator.mediaDevices
      .getUserMedia(MIC_CONSTRAINTS)
      .then((s) => {
        if (aborted) {
          s.getTracks().forEach((t) => t.stop());
          return;
        }
        stream = s;
        ctx = new AudioContext();
        const source = ctx.createMediaStreamSource(s);

        const analyser = ctx.createAnalyser();
        analyser.fftSize = 2048;
        source.connect(analyser);

        // ScriptProcessorNode is deprecated, but it is what the rest of this
        // codebase's capture path uses; switching to an AudioWorklet is a
        // separate change that should move every call site at once.
        const processor = ctx.createScriptProcessor(PROCESSOR_BUFFER, 1, 1);
        processor.onaudioprocess = (e) => {
          chunks.push(new Float32Array(e.inputBuffer.getChannelData(0)));
        };
        source.connect(processor);
        processor.connect(ctx.destination);

        const frame = new Float32Array(analyser.fftSize);
        levelTimer = setInterval(() => {
          analyser.getFloatTimeDomainData(frame);
          const rms = calculateRms(frame);
          if (rms > peakRms) peakRms = rms;
          onLevel?.(rms);
        }, LEVEL_INTERVAL_MS);

        endTimer = setTimeout(() => stop?.(), durationMs);
      })
      .catch((err: Error) => {
        teardown();
        if (settled) return;
        settled = true;
        reject(err);
      });
  });

  return {
    done,
    abort() {
      aborted = true;
      stop?.(new DOMException("recording aborted", "AbortError"));
    },
  };
}

/** A message worth showing the user for a getUserMedia failure. */
export function micErrorMessage(err: unknown): string {
  const name = (err as { name?: string })?.name;
  if (name === "NotAllowedError") {
    return "Microphone access was denied. Allow it in System Settings > Privacy & Security > Microphone, then try again.";
  }
  if (name === "NotFoundError") {
    return "No microphone was found. Connect one and try again.";
  }
  if (name === "AbortError") return "Recording cancelled.";
  return `Microphone error: ${(err as Error)?.message ?? String(err)}`;
}
