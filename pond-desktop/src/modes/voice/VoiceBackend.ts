// ────────────────────────────────────────────────────────────
// VoiceBackend — Unified interface for Tauri and Browser voice
//
// Both TauriVoiceBackend and WebVoiceBackend implement this
// interface. The orchestration hook (useVoicePipeline) uses
// the factory to pick the right one at runtime.
// ────────────────────────────────────────────────────────────

export type VoiceState = "idle" | "wait" | "recording" | "thinking" | "speaking" | "error";

export interface PipelineOpts {
  stripWakeWord?: string;
  sessionId?: string;
  authToken?: string;
  serverUrl: string;
  /**
   * `settings.voice_thinking_tone_enabled` — whether the ambient working tone
   * plays between the quip and the first spoken sentence. Omitted means ON:
   * the orchestrator loads settings asynchronously, and a turn that fires
   * before that resolves should sound like the pond normally does.
   */
  thinkingTone?: boolean;
}

export interface ResponseMeta {
  modelName: string;
  modelRole: string;
  completionTokens: number;
}

export interface ToolCallData {
  tool: string;
  data: Record<string, unknown>;
}

/**
 * Abstraction over voice I/O. Implementations handle microphone capture,
 * ASR transcription, LLM chat streaming, TTS playback, and wake word
 * detection through either Tauri IPC or browser Web APIs.
 *
 * The orchestration hook sets callbacks before calling action methods.
 */
export interface VoiceBackend {
  // ── Recording ──────────────────────────────────────────
  /**
   * VAD-aware recording: opens mic, waits for speech, auto-stops on silence.
   * `authToken` and `sessionId` let implementations fire a speculative LLM
   * request during the silence-confirmation wait (Q2-26). Backends that don't
   * use them may ignore both parameters.
   */
  recordWithVad(authToken?: string, sessionId?: string): Promise<Blob | null>;
  /** Cancel in-progress recording without sending. */
  abortRecording(): void;

  // ── Pipeline ───────────────────────────────────────────
  /** Full pipeline: transcribe → chat stream → sentence TTS. */
  runPipeline(wav: Blob, opts: PipelineOpts): Promise<void>;
  /** Barge-in: abort pipeline, stop TTS, stop thinking tone. */
  cancelPipeline(): void;

  // ── Wake word ──────────────────────────────────────────
  startWakeListener(word: string, variants: string[]): void;
  stopWakeListener(): void;

  // ── Audio feedback ─────────────────────────────────────
  playPing(): void;

  // ── Lifecycle ──────────────────────────────────────────
  destroy(): void;

  // ── Event callbacks (set by orchestration hook) ────────
  onAudioLevel: ((level: number) => void) | null;
  onStateChange: ((state: VoiceState) => void) | null;
  onTranscript: ((text: string) => void) | null;
  onAgentToken: ((token: string, done: boolean) => void) | null;
  onToolCall: ((data: ToolCallData) => void) | null;
  onError: ((msg: string) => void) | null;
  onSessionId: ((id: string) => void) | null;
  onResponseMeta: ((meta: ResponseMeta) => void) | null;
  onWakeDetected: ((wav: Blob) => void) | null;
  onWakeInterrupt: (() => void) | null;
  onDismissed: ((isExit: boolean) => void) | null;
}

// ── Factory ──────────────────────────────────────────────────

/**
 * Create the VoiceBackend for the current runtime.
 *
 * Only ever the browser one. Inside Tauri the voice screen runs the persistent
 * child-process session (`useVoiceSession`) instead, so this per-turn HTTP
 * pipeline is the non-Tauri path exclusively. The Tauri implementation of this
 * interface was removed once that became true — it had no reachable caller, and
 * a second copy of the pipeline that nothing exercised was a copy that could
 * only drift.
 *
 * Still lazy-imported: it pulls in the whole Web Audio pipeline, which the
 * desktop build never runs.
 */
export async function createVoiceBackend(_serverUrl: string): Promise<VoiceBackend> {
  const { WebVoiceBackend } = await import("./WebVoiceBackend");
  return new WebVoiceBackend(_serverUrl);
}
