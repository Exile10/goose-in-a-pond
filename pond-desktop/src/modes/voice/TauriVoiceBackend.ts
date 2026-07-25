// ────────────────────────────────────────────────────────────
// TauriVoiceBackend — VoiceBackend implementation for Tauri IPC
//
// Wraps Tauri invoke() commands and listen() event subscriptions.
// All microphone capture, ASR, LLM streaming, TTS playback, and
// wake word detection run in the Rust sidecar; this class bridges
// the event channel to the VoiceBackend callback interface.
//
// EVENT OWNERSHIP NOTE (double-dispatch fix):
//   AppContext.tsx is the SOLE owner of the legacy per-turn pipeline
//   events: transcript, response-token, tool-result, tts-start, tts-end.
//   TauriVoiceBackend used to also register listeners for those same
//   events, causing every event to dispatch into the reducer twice —
//   a verified bug that corrupted the role guard at reducer.ts:201.
//
//   This class now registers ONLY the events it uniquely owns via its
//   callback interface:
//     - audio-level  (onAudioLevel)
//     - wake-word-detected  (onWakeDetected)
//     - wake-word-interrupt  (onWakeInterrupt)
//     - voice-dismissed  (onDismissed)
//     - pipeline-error  (onError — still registered here so callers
//       can handle errors through the backend interface; AppContext
//       also listens to pipeline-error for Canvas mode compatibility)
//
//   The NEW voice-* events (voice-ready, voice-state, voice-transcript,
//   voice-token, voice-tool-call, voice-tool-result, voice-done,
//   voice-error, voice-session-ended) are the EXCLUSIVE domain of
//   useVoiceSession.  This class never registers any voice-* listener.
// ────────────────────────────────────────────────────────────

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  VoiceBackend,
  PipelineOpts,
  VoiceState,
  ToolCallData,
  ResponseMeta,
} from "./VoiceBackend";

/** Tauri event payload types (mirrors Rust-side structs). */
interface PipelineErrorPayload {
  message?: string;
}

export class TauriVoiceBackend implements VoiceBackend {
  // ── Callbacks (set by orchestration hook) ─────────────────
  onAudioLevel: ((level: number) => void) | null = null;
  onStateChange: ((state: VoiceState) => void) | null = null;
  onTranscript: ((text: string) => void) | null = null;
  onAgentToken: ((token: string, done: boolean) => void) | null = null;
  onToolCall: ((data: ToolCallData) => void) | null = null;
  onError: ((msg: string) => void) | null = null;
  onSessionId: ((id: string) => void) | null = null;
  onResponseMeta: ((meta: ResponseMeta) => void) | null = null;
  onWakeDetected: ((wav: Blob) => void) | null = null;
  onWakeInterrupt: (() => void) | null = null;
  onDismissed: ((isExit: boolean) => void) | null = null;

  private readonly serverUrl: string;
  private readonly unlisteners: Promise<() => void>[] = [];

  // Q2-26: transcript computed speculatively during the last recordWithVad()
  // call's silence-confirmation wait, if any, tagged with the exact Blob it
  // belongs to. runPipeline() only reuses it when passed that same Blob by
  // reference — the wake-word path calls runPipeline() with audio captured
  // independently in Rust, and must never pick up a stale transcript from
  // an unrelated recordWithVad() call.
  private speculative: { wav: Blob; transcript: string } | null = null;

  constructor(serverUrl: string) {
    this.serverUrl = serverUrl;
    this.registerListeners();
  }

  // ── Tauri event registration ──────────────────────────────
  //
  // ONLY events exclusively owned by TauriVoiceBackend are registered here.
  // See the EVENT OWNERSHIP NOTE at the top of this file.

  private registerListeners(): void {
    // audio-level: emitted by audio.rs during VAD recording
    this.unlisteners.push(
      listen<number>("audio-level", (e) => {
        this.onAudioLevel?.(e.payload);
      }),
    );

    // pipeline-error: emitted by audio_cmd.rs on pipeline failure.
    // Also listened to by AppContext for Canvas mode; both registrations
    // are intentional — the callback interface notifies the orchestration
    // hook, while AppContext surfaces the error to the global state.
    this.unlisteners.push(
      listen<string | PipelineErrorPayload>("pipeline-error", (e) => {
        const msg =
          typeof e.payload === "string"
            ? e.payload
            : e.payload.message ?? "Pipeline error";
        this.onError?.(msg);
      }),
    );

    // wake-word-detected: emitted by audio.rs with the captured WAV bytes
    this.unlisteners.push(
      listen<number[]>("wake-word-detected", (e) => {
        const bytes = new Uint8Array(e.payload);
        this.onWakeDetected?.(new Blob([bytes], { type: "audio/wav" }));
      }),
    );

    // wake-word-interrupt: barge-in detected during active pipeline
    this.unlisteners.push(
      listen("wake-word-interrupt", () => {
        this.onWakeInterrupt?.();
      }),
    );

    // voice-dismissed: farewell phrase or explicit dismissal from Rust
    this.unlisteners.push(
      listen<boolean>("voice-dismissed", (e) => {
        this.onDismissed?.(e.payload);
      }),
    );
  }

  // ── Recording ─────────────────────────────────────────────

  async recordWithVad(authToken?: string, sessionId?: string): Promise<Blob | null> {
    const result = await invoke<{ wav: number[]; transcript: string | null }>(
      "record_with_vad",
      { authToken: authToken ?? "", sessionId: sessionId ?? null },
    );
    if (!result.wav?.length) return null;
    const blob = new Blob([new Uint8Array(result.wav)], { type: "audio/wav" });
    this.speculative = result.transcript ? { wav: blob, transcript: result.transcript } : null;
    return blob;
  }

  abortRecording(): void {
    invoke("abort_recording").catch(() => {});
  }

  // ── Pipeline ──────────────────────────────────────────────

  async runPipeline(wav: Blob, opts: PipelineOpts): Promise<void> {
    const buffer = await wav.arrayBuffer();
    const wavBytes = Array.from(new Uint8Array(buffer));
    const transcript = this.speculative?.wav === wav ? this.speculative.transcript : null;
    this.speculative = null;
    await invoke("run_voice_pipeline", {
      wavBytes,
      authToken: opts.authToken ?? "",
      sessionId: opts.sessionId ?? "",
      transcript,
    });
  }

  cancelPipeline(): void {
    // The Tauri pipeline uses AudioKillSwitch, set by the wake listener
    // on barge-in. For explicit cancel, aborting the recording is the
    // closest available command.
    invoke("abort_recording").catch(() => {});
  }

  // ── Wake word ─────────────────────────────────────────────

  startWakeListener(word: string, variants: string[]): void {
    invoke("start_wake_listener", {
      wakeWord: word,
      variants: variants.length > 0 ? variants : null,
    }).catch(() => {});
  }

  stopWakeListener(): void {
    invoke("stop_wake_listener").catch(() => {});
  }

  // ── Audio feedback ────────────────────────────────────────

  playPing(): void {
    invoke("play_ping").catch(() => {});
  }

  // ── Lifecycle ─────────────────────────────────────────────

  destroy(): void {
    this.stopWakeListener();
    for (const pending of this.unlisteners) {
      pending.then((unlisten) => unlisten()).catch(() => {});
    }
    this.unlisteners.length = 0;
  }
}
