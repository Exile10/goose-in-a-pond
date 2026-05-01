/**
 * useWebVoice — Browser-native voice pipeline hook.
 *
 * Provides the same voice capabilities as the Tauri IPC pipeline
 * (mic capture, transcription, chat streaming, TTS playback, wake word
 * detection, barge-in) using only Web APIs + HTTP calls to pond-server.
 *
 * Used by VoiceMode.tsx when running in a browser context (not Tauri).
 * The Tauri pipeline remains the primary, optimised path; this hook
 * is a graceful fallback for remote access (e.g., phone accessing
 * `http://jetson.local:4000`).
 *
 * @module useWebVoice
 */

import { useRef, useCallback, useEffect } from "react";
import { api } from "../api/PondApiClient";
import { filterThinking } from "../lib/thinkFilter";
import {
  nextTranscriptId,
  type AppAction,
  type TranscriptMessage,
  type AppState,
} from "../state/reducer";
import {
  encodeWav,
  calculateRms,
  downsampleTo16k,
  getAudioContext,
  closeAudioContext,
  playPingTone,
  playThinkingTone,
  splitSentences,
  stripMarkdown,
  normalizeForSpeech,
  isWhisperArtifact,
  checkDismissal,
  getToolAnnouncement,
  getQuip,
  createVadState,
  advanceVad,
  DEFAULT_VAD_CONFIG,
  type VadConfig,
} from "./webAudioUtils";

// ── Public API types ──────────────────────────────────────────────

export interface WebVoiceActions {
  /** Open the mic via getUserMedia and begin recording. */
  startRecording(): Promise<void>;
  /** Stop recording and return the captured WAV blob. */
  stopRecording(): Promise<Blob | null>;
  /** VAD-aware recording: auto-starts, waits for speech, auto-stops. */
  recordWithVad(): Promise<Blob | null>;
  /**
   * Full pipeline: transcribe WAV -> chat stream -> TTS playback.
   * @param stripWakeWord  If provided, strip this phrase from the transcript
   *                       before sending to the LLM (used for one-breath wake+command).
   */
  runVoicePipeline(wavBlob: Blob, stripWakeWord?: string): Promise<void>;
  /** Start background wake word detection. */
  startWakeListener(wakeWord: string, variants: string[]): void;
  /** Stop background wake word detection. */
  stopWakeListener(): void;
  /** Play a two-tone activation chime. */
  playPing(): void;
  /** Cancel in-flight pipeline (barge-in). */
  cancelPipeline(): void;
  /** Abort recording without sending. */
  abortRecording(): void;
}

export interface WebVoiceState {
  /** Current RMS audio level (0..1) for visualisations. */
  audioLevel: number;
  /** Whether the wake word listener is active. */
  isListening: boolean;
}

// ── Enhanced thought filter (all tag formats) ────────────────────

const THINK_OPEN = [/<think>/i, /<thought>/i, /<\|channel>thought/i, /<\|tool_call>/i];
const THINK_CLOSE = [/<\/think>/i, /<\/thought>/i, /<channel\|>/i, /<\/tool_call>/i, /<tool_call\|>/i];
const ORPHAN_SENTINELS = /<eos>|<\|eos\|>|<end_of_turn>/gi;

/**
 * Enhanced thought filter matching Tauri's ThoughtFilter behavior.
 * Handles: <think>, <thought>, <|channel>thought...<channel|>,
 * <|tool_call>...<tool_call|>, and orphan sentinels.
 */
function filterThinkingFull(token: string, inBlock: boolean): [string, boolean] {
  let text = token;
  let inside = inBlock;

  // Strip orphan sentinels
  text = text.replace(ORPHAN_SENTINELS, "");

  let result = "";
  let i = 0;
  while (i < text.length) {
    if (!inside) {
      // Check for opening tags
      let matched = false;
      for (const re of THINK_OPEN) {
        const sub = text.slice(i);
        const m = sub.match(re);
        if (m && m.index === 0) {
          inside = true;
          i += m[0].length;
          matched = true;
          break;
        }
      }
      if (!matched) {
        result += text[i];
        i++;
      }
    } else {
      // Inside a block — look for closing tags
      let matched = false;
      for (const re of THINK_CLOSE) {
        const sub = text.slice(i);
        const m = sub.match(re);
        if (m && m.index === 0) {
          inside = false;
          i += m[0].length;
          matched = true;
          break;
        }
      }
      if (!matched) {
        i++; // skip character inside block
      }
    }
  }

  return [result, inside];
}

// ── Internal types ────────────────────────────────────────────────

interface RecordingContext {
  stream: MediaStream;
  processor: ScriptProcessorNode;
  source: MediaStreamAudioSourceNode;
  analyser: AnalyserNode;
  audioContext: AudioContext;
  chunks: Float32Array[];
  sampleRate: number;
}

// ── Hook ──────────────────────────────────────────────────────────

/**
 * Browser-native voice pipeline hook.
 *
 * Mirrors the Tauri audio commands (`record_with_vad`, `run_voice_pipeline`,
 * `start_wake_listener`, etc.) using Web Audio API + fetch to pond-server.
 *
 * @param dispatch  React dispatch from AppContext
 * @param state     Current AppState (for sessionId, sessionToken, serverUrl)
 * @param onAudioLevel  Callback fired with RMS level (0..1) during recording
 */
export function useWebVoice(
  dispatch: React.Dispatch<AppAction>,
  state: AppState,
  onAudioLevel?: (level: number) => void,
): WebVoiceActions {
  // ── Refs ──────────────────────────────────────────────────────

  const recordingRef = useRef<RecordingContext | null>(null);
  const cancelledRef = useRef(false);
  const pipelineActiveRef = useRef(false);
  const abortControllerRef = useRef<AbortController | null>(null);
  const ttsSourceRef = useRef<AudioBufferSourceNode | null>(null);
  const stopThinkingRef = useRef<(() => void) | null>(null);
  const wakeIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const wakeStreamRef = useRef<MediaStream | null>(null);
  const wakeActiveRef = useRef(false);

  // Keep latest state accessible without stale closures
  const stateRef = useRef(state);
  stateRef.current = state;

  // ── Cleanup on unmount ────────────────────────────────────────

  useEffect(() => {
    return () => {
      stopRecordingInternal();
      stopWakeListenerInternal();
      closeAudioContext();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Internal: open mic ────────────────────────────────────────

  async function openMic(): Promise<RecordingContext> {
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        sampleRate: { ideal: 16000 },
        channelCount: { exact: 1 },
        echoCancellation: true,
        noiseSuppression: true,
      },
    });

    // Use a dedicated AudioContext for recording at native rate
    const audioContext = new AudioContext();
    const source = audioContext.createMediaStreamSource(stream);
    const analyser = audioContext.createAnalyser();
    analyser.fftSize = 2048;
    source.connect(analyser);

    // ScriptProcessorNode for raw PCM capture.
    // AudioWorklet would be preferred but adds complexity for a fallback path.
    const processor = audioContext.createScriptProcessor(4096, 1, 1);
    const chunks: Float32Array[] = [];

    processor.onaudioprocess = (event) => {
      const input = event.inputBuffer.getChannelData(0);
      chunks.push(new Float32Array(input));
    };

    source.connect(processor);
    processor.connect(audioContext.destination);

    return {
      stream,
      processor,
      source,
      analyser,
      audioContext,
      chunks,
      sampleRate: audioContext.sampleRate,
    };
  }

  /** Stop and tear down the active recording context. */
  function stopRecordingInternal(): void {
    const ctx = recordingRef.current;
    if (!ctx) return;

    try { ctx.processor.disconnect(); } catch { /* ignore */ }
    try { ctx.source.disconnect(); } catch { /* ignore */ }
    ctx.stream.getTracks().forEach((t) => t.stop());
    if (ctx.audioContext.state !== "closed") {
      ctx.audioContext.close().catch(() => {});
    }
    recordingRef.current = null;
  }

  function stopWakeListenerInternal(): void {
    wakeActiveRef.current = false;
    if (wakeIntervalRef.current) {
      clearInterval(wakeIntervalRef.current);
      wakeIntervalRef.current = null;
    }
    if (wakeStreamRef.current) {
      wakeStreamRef.current.getTracks().forEach((t) => t.stop());
      wakeStreamRef.current = null;
    }
  }

  /** Collect recorded chunks into a single WAV ArrayBuffer. */
  function collectWav(ctx: RecordingContext): ArrayBuffer {
    const totalLength = ctx.chunks.reduce((sum, c) => sum + c.length, 0);
    const merged = new Float32Array(totalLength);
    let offset = 0;
    for (const chunk of ctx.chunks) {
      merged.set(chunk, offset);
      offset += chunk.length;
    }
    const downsampled = downsampleTo16k(merged, ctx.sampleRate);
    return encodeWav(downsampled, 16000);
  }

  // ── Public: startRecording ────────────────────────────────────

  const startRecording = useCallback(async () => {
    stopRecordingInternal();
    cancelledRef.current = false;

    try {
      const ctx = await openMic();
      recordingRef.current = ctx;

      // Pump audio level to the callback for the AudioWaves visualisation
      const timeDomainData = new Float32Array(ctx.analyser.fftSize);
      const levelPump = setInterval(() => {
        if (!recordingRef.current) {
          clearInterval(levelPump);
          return;
        }
        ctx.analyser.getFloatTimeDomainData(timeDomainData);
        const rms = calculateRms(timeDomainData);
        onAudioLevel?.(rms);
      }, 30);

      // Store interval so stopRecording can clear it
      (ctx as RecordingContext & { _levelPump?: ReturnType<typeof setInterval> })._levelPump = levelPump;
    } catch (err) {
      const msg = String(err);
      const isMicDenied =
        msg.toLowerCase().includes("permission") ||
        msg.toLowerCase().includes("notallowederror") ||
        msg.toLowerCase().includes("denied");

      dispatch({
        type: "SET_VOICE_ERROR",
        payload: isMicDenied
          ? "Microphone access denied. Please allow mic access in your browser settings."
          : `Mic error: ${msg}`,
      });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dispatch, onAudioLevel]);

  // ── Public: stopRecording ─────────────────────────────────────

  const stopRecording = useCallback(async (): Promise<Blob | null> => {
    const ctx = recordingRef.current;
    if (!ctx) return null;

    // Clear level pump
    const extended = ctx as RecordingContext & { _levelPump?: ReturnType<typeof setInterval> };
    if (extended._levelPump) clearInterval(extended._levelPump);
    onAudioLevel?.(0);

    const wavBuffer = collectWav(ctx);
    stopRecordingInternal();

    return new Blob([wavBuffer], { type: "audio/wav" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onAudioLevel]);

  // ── Public: recordWithVad ─────────────────────────────────────

  const recordWithVad = useCallback(async (): Promise<Blob | null> => {
    stopRecordingInternal();
    cancelledRef.current = false;

    let ctx: RecordingContext;
    try {
      ctx = await openMic();
      recordingRef.current = ctx;
    } catch (err) {
      const msg = String(err);
      dispatch({
        type: "SET_VOICE_ERROR",
        payload: msg.toLowerCase().includes("permission") || msg.toLowerCase().includes("denied")
          ? "Microphone access denied. Please allow mic access in your browser settings."
          : `Mic error: ${msg}`,
      });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
      return null;
    }

    const vadConfig: VadConfig = { ...DEFAULT_VAD_CONFIG };
    const vadState = createVadState();
    const timeDomainData = new Float32Array(ctx.analyser.fftSize);
    const startTime = Date.now();

    return new Promise<Blob | null>((resolve) => {
      const levelPump = setInterval(() => {
        if (cancelledRef.current || !recordingRef.current) {
          clearInterval(levelPump);
          onAudioLevel?.(0);
          stopRecordingInternal();
          resolve(null);
          return;
        }

        ctx.analyser.getFloatTimeDomainData(timeDomainData);
        const rms = calculateRms(timeDomainData);
        onAudioLevel?.(rms);

        const now = Date.now();
        const elapsed = now - startTime;

        // Hard timeout
        if (elapsed >= vadConfig.maxDurationMs) {
          clearInterval(levelPump);
          onAudioLevel?.(0);
          const wavBuffer = collectWav(ctx);
          stopRecordingInternal();
          resolve(new Blob([wavBuffer], { type: "audio/wav" }));
          return;
        }

        // VAD state machine
        const shouldStop = advanceVad(vadState, rms, now, vadConfig);
        if (shouldStop) {
          clearInterval(levelPump);
          onAudioLevel?.(0);
          const wavBuffer = collectWav(ctx);
          stopRecordingInternal();
          resolve(new Blob([wavBuffer], { type: "audio/wav" }));
          return;
        }
      }, 30); // ~33Hz analysis rate

      // Store for cleanup
      const extended = ctx as RecordingContext & { _levelPump?: ReturnType<typeof setInterval> };
      extended._levelPump = levelPump;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dispatch, onAudioLevel]);

  // ── Public: runVoicePipeline ──────────────────────────────────

  const runVoicePipeline = useCallback(async (wavBlob: Blob, stripWakeWord?: string) => {
    cancelledRef.current = false;
    pipelineActiveRef.current = true;
    const controller = new AbortController();
    abortControllerRef.current = controller;

    try {
      // ── Step 1: Transcribe ──────────────────────────────────
      const wavBuffer = await wavBlob.arrayBuffer();
      const transcription = await api.transcribe(wavBuffer);

      if (cancelledRef.current) return;

      let text = transcription.text?.trim() ?? "";
      if (!text || isWhisperArtifact(text)) {
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        return;
      }

      // ── Step 1a: Strip wake word from one-breath captures ───
      if (stripWakeWord) {
        const lower = text.toLowerCase();
        const wakeLower = stripWakeWord.toLowerCase();
        const idx = lower.indexOf(wakeLower);
        if (idx !== -1) {
          text = text.slice(idx + wakeLower.length).trim();
        }
        // If nothing left after stripping, user only said the wake word.
        // Fall back to VAD recording for the actual command.
        if (!text) {
          dispatch({ type: "SET_VOICE_STATE", payload: "recording" });
          const cmdBlob = await recordWithVad();
          if (cmdBlob) {
            dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
            // Recursive call WITHOUT stripWakeWord — this is a clean command recording
            return runVoicePipeline(cmdBlob);
          }
          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
          return;
        }
      }

      // ── Step 1b: Dismissal check ────────────────────────────
      const dismissal = checkDismissal(text);
      if (dismissal.dismissed) {
        const farewell = dismissal.isExit
          ? "Goodbye! I'll be here whenever you need me."
          : "Until next time. Just say my name when you need me.";
        dispatch({ type: "SET_VOICE_STATE", payload: "speaking" });
        await playTtsSentence(farewell, controller.signal);
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
        return;
      }

      // Dispatch user transcript
      const userMsg: TranscriptMessage = {
        id: nextTranscriptId(),
        role: "user",
        text,
        timestamp: Date.now(),
      };
      dispatch({ type: "APPEND_TRANSCRIPT", payload: userMsg });
      dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });

      // ── Step 1c: Concurrent quip TTS ────────────────────────
      // Speaks a short quip to fill silence while LLM processes.
      let quipDone = false;
      const quipPromise = (async () => {
        try {
          await playTtsSentence(getQuip(), controller.signal);
        } catch { /* ignore */ }
        quipDone = true;
      })();

      // ── Step 1d: Start thinking tone ────────────────────────
      const stopThinking = playThinkingTone();
      stopThinkingRef.current = stopThinking;

      // Seed empty agent message for token streaming
      const agentMsg: TranscriptMessage = {
        id: nextTranscriptId(),
        role: "agent",
        text: "",
        timestamp: Date.now(),
      };
      dispatch({ type: "APPEND_TRANSCRIPT", payload: agentMsg });

      // ── Step 2: Chat stream via fetch + ReadableStream ──────
      const sessionId = stateRef.current.sessionId ?? undefined;
      let fullText = "";
      let thinkInBlock = false;
      let firstSentenceSent = false;

      const chatHeaders: Record<string, string> = { "Content-Type": "application/json" };
      const token = stateRef.current.sessionToken;
      if (token) chatHeaders["Authorization"] = `Bearer ${token}`;

      const serverUrl = stateRef.current.serverUrl ?? "http://127.0.0.1:4000";
      const chatRes = await fetch(
        `${serverUrl}/api/v1/chat/stream`,
        {
          method: "POST",
          headers: chatHeaders,
          body: JSON.stringify({
            message: text,
            session_id: sessionId,
            voice_mode: true,
          }),
          signal: controller.signal,
        },
      );

      if (!chatRes.ok || !chatRes.body) {
        throw new Error(`Chat request failed: ${chatRes.status} ${chatRes.statusText}`);
      }

      const reader = chatRes.body.getReader();
      const decoder = new TextDecoder();
      let sseBuffer = "";

      // TTS sentence accumulator
      let ttsBuf = "";
      const ttsQueue: string[] = [];
      let ttsPlaying = false;

      const playNextTts = async () => {
        if (cancelledRef.current) return;
        if (ttsQueue.length === 0) {
          ttsPlaying = false;
          return;
        }
        ttsPlaying = true;
        const raw = ttsQueue.shift()!;
        // Strip markdown and normalize symbols for natural TTS
        const cleaned = normalizeForSpeech(stripMarkdown(raw));
        if (cleaned.trim()) {
          await playTtsSentence(cleaned, controller.signal);
        }
        playNextTts();
      };

      const enqueueTts = (sentence: string) => {
        // On first real sentence, stop quip + thinking tone
        if (!firstSentenceSent) {
          firstSentenceSent = true;
          stopThinking();
          stopThinkingRef.current = null;
          // Cancel quip if still playing
          if (!quipDone && ttsSourceRef.current) {
            try { ttsSourceRef.current.stop(); } catch { /* ignore */ }
          }
        }
        ttsQueue.push(sentence);
        if (!ttsPlaying) {
          dispatch({ type: "SET_VOICE_STATE", payload: "speaking" });
          playNextTts();
        }
      };

      try {
        while (true) {
          if (cancelledRef.current) break;

          const { value, done } = await reader.read();
          if (done) break;

          sseBuffer += decoder.decode(value, { stream: true });
          const lines = sseBuffer.split("\n");
          sseBuffer = lines.pop() ?? "";

          for (const line of lines) {
            const trimmed = line.trim();
            if (!trimmed || trimmed === "data: [DONE]") continue;
            const data = trimmed.startsWith("data: ") ? trimmed.slice(6) : trimmed;

            let event: Record<string, unknown>;
            try {
              event = JSON.parse(data);
            } catch {
              continue;
            }

            if (event.error) {
              dispatch({ type: "SET_VOICE_ERROR", payload: String(event.error) });
              dispatch({ type: "SET_VOICE_STATE", payload: "error" });
              return;
            }

            // ── Text tokens ──
            if (event.type === "text") {
              const rawToken = (event.content ?? event.token ?? "") as string;

              // Strip ALL thinking tag formats (Gemma4, Qwen3, DeepSeek, etc.)
              const [visible, newInBlock] = filterThinkingFull(rawToken, thinkInBlock);
              thinkInBlock = newInBlock;

              if (visible) {
                fullText += visible;
                dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: visible, done: false } });

                ttsBuf += visible;
                const sentences = splitSentences(ttsBuf);
                if (sentences.length > 1) {
                  for (let i = 0; i < sentences.length - 1; i++) {
                    enqueueTts(sentences[i]);
                  }
                  ttsBuf = sentences[sentences.length - 1];
                }
              }
            }

            // ── Tool calls: flush buffer + speak announcement ──
            if (event.type === "tool_call") {
              const toolName = (event.tool as string) ?? "tool";
              // Flush pending TTS buffer
              if (ttsBuf.trim()) {
                enqueueTts(ttsBuf.trim());
                ttsBuf = "";
              }
              // Speak tool announcement
              enqueueTts(getToolAnnouncement(toolName));

              const card = {
                id: Date.now(),
                tool: toolName,
                data: (event.arguments as Record<string, unknown>) ?? {},
                timestamp_ms: Date.now(),
              };
              dispatch({ type: "PUSH_CONTEXT_CARD", payload: card });
            }

            if (event.type === "tool_result") {
              const card = {
                id: Date.now(),
                tool: (event.tool as string) ?? "tool",
                data: (event.result as Record<string, unknown>) ?? {},
                timestamp_ms: Date.now(),
              };
              dispatch({ type: "PUSH_CONTEXT_CARD", payload: card });
            }

            if (event.done === true) {
              dispatch({ type: "APPEND_AGENT_TOKEN", payload: { token: "", done: true } });

              if (event.session_id && typeof event.session_id === "string") {
                dispatch({ type: "SET_SESSION_ID", payload: event.session_id as string });
              }

              if (event.model_name && event.model_role && event.usage) {
                dispatch({
                  type: "SET_LAST_RESPONSE_META",
                  payload: {
                    modelName: event.model_name as string,
                    modelRole: event.model_role as string,
                    completionTokens: (event.usage as { completion_tokens: number }).completion_tokens ?? 0,
                  },
                });
              }
            }
          }
        }
      } finally {
        reader.releaseLock();
      }

      // ── Step 3: Flush remaining TTS buffer ──────────────────
      if (ttsBuf.trim() && !cancelledRef.current) {
        enqueueTts(ttsBuf.trim());
      }

      // Wait for TTS queue to drain
      await new Promise<void>((resolve) => {
        const check = setInterval(() => {
          if (!ttsPlaying || cancelledRef.current) {
            clearInterval(check);
            resolve();
          }
        }, 100);
      });

      // Ensure thinking tone is stopped
      if (stopThinkingRef.current) {
        stopThinkingRef.current();
        stopThinkingRef.current = null;
      }

      if (!cancelledRef.current) {
        dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
      }
    } catch (err) {
      if (cancelledRef.current) return;
      if ((err as Error).name === "AbortError") return;

      console.error("Web voice pipeline error:", err);
      dispatch({ type: "SET_VOICE_ERROR", payload: String(err) });
      dispatch({ type: "SET_VOICE_STATE", payload: "error" });
    } finally {
      pipelineActiveRef.current = false;
      abortControllerRef.current = null;
      if (stopThinkingRef.current) {
        stopThinkingRef.current();
        stopThinkingRef.current = null;
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dispatch]);

  // ── Internal: play a single TTS sentence ──────────────────────

  async function playTtsSentence(text: string, signal: AbortSignal): Promise<void> {
    if (cancelledRef.current || signal.aborted) return;

    try {
      const headers: Record<string, string> = { "Content-Type": "application/json" };
      const token = stateRef.current.sessionToken;
      if (token) headers["Authorization"] = `Bearer ${token}`;

      const serverUrl = stateRef.current.serverUrl ?? "http://127.0.0.1:4000";
      const res = await fetch(
        `${serverUrl}/api/v1/tts`,
        {
          method: "POST",
          headers,
          body: JSON.stringify({ text }),
          signal,
        },
      );

      if (!res.ok) {
        console.warn("TTS request failed:", res.status);
        return;
      }

      const audioData = await res.arrayBuffer();
      if (cancelledRef.current || signal.aborted) return;

      const ctx = getAudioContext();
      const audioBuffer = await ctx.decodeAudioData(audioData);
      if (cancelledRef.current || signal.aborted) return;

      return new Promise<void>((resolve) => {
        const source = ctx.createBufferSource();
        source.buffer = audioBuffer;
        source.connect(ctx.destination);
        ttsSourceRef.current = source;

        source.onended = () => {
          ttsSourceRef.current = null;
          resolve();
        };

        source.start(0);
      });
    } catch (err) {
      if ((err as Error).name === "AbortError") return;
      console.warn("TTS playback error:", err);
    }
  }

  // ── Public: startWakeListener ─────────────────────────────────

  const startWakeListener = useCallback((wakeWord: string, variants: string[]) => {
    stopWakeListenerInternal();
    wakeActiveRef.current = true;

    // Normalise wake word and variants for matching
    const normalised = [wakeWord.toLowerCase().trim()];
    for (const v of variants) {
      const n = v.toLowerCase().trim();
      if (n && !normalised.includes(n)) normalised.push(n);
    }

    // Strategy: Use VAD-gated periodic transcription.
    // 1. Open a persistent mic stream for RMS monitoring
    // 2. When speech is detected, capture a 2-second window
    // 3. Transcribe and check for wake word match
    // This is less efficient than Tauri's always-on detector but functional.

    let wakeRecording = false;
    let wakeContext: AudioContext | null = null;
    let wakeAnalyser: AnalyserNode | null = null;
    let wakeSource: MediaStreamAudioSourceNode | null = null;
    let wakeProcessor: ScriptProcessorNode | null = null;
    let wakeChunks: Float32Array[] = [];
    let wakeSampleRate = 16000;
    let speechStart: number | null = null;
    let onsetFrames = 0; // hysteresis: require 2 consecutive frames above threshold
    const WAKE_SPEECH_RMS = 0.008; // match Tauri's lowered threshold (commit 0361dba)
    const WAKE_SILENCE_RMS = 0.004;
    const WAKE_ONSET_FRAMES = 2; // 2 × 30ms = 60ms sustained speech required
    const WAKE_TAIL_MS = 360; // 12 × 30ms = 360ms silence to end speech
    const WAKE_POST_TRIGGER_MS = 2000; // match Tauri's 2s continuation window

    navigator.mediaDevices
      .getUserMedia({
        audio: {
          sampleRate: { ideal: 16000 },
          channelCount: { exact: 1 },
          echoCancellation: true,
          noiseSuppression: true,
        },
      })
      .then((stream) => {
        if (!wakeActiveRef.current) {
          stream.getTracks().forEach((t) => t.stop());
          return;
        }

        wakeStreamRef.current = stream;
        wakeContext = new AudioContext();
        wakeSampleRate = wakeContext.sampleRate;
        wakeSource = wakeContext.createMediaStreamSource(stream);
        wakeAnalyser = wakeContext.createAnalyser();
        wakeAnalyser.fftSize = 2048;
        wakeSource.connect(wakeAnalyser);

        // Processor for capturing raw audio during wake word detection
        wakeProcessor = wakeContext.createScriptProcessor(4096, 1, 1);
        wakeProcessor.onaudioprocess = (event) => {
          if (wakeRecording) {
            wakeChunks.push(new Float32Array(event.inputBuffer.getChannelData(0)));
          }
        };
        wakeSource.connect(wakeProcessor);
        wakeProcessor.connect(wakeContext.destination);

        const timeDomainData = new Float32Array(wakeAnalyser.fftSize);

        // Poll at ~30Hz for speech activity
        wakeIntervalRef.current = setInterval(async () => {
          if (!wakeActiveRef.current || !wakeAnalyser) return;

          wakeAnalyser.getFloatTimeDomainData(timeDomainData);
          const rms = calculateRms(timeDomainData);

          // Pass level up for visualisation in wait state
          onAudioLevel?.(rms * 0.3); // attenuate for subtle breathing effect

          if (!wakeRecording) {
            // Hysteresis: require WAKE_ONSET_FRAMES consecutive frames above threshold
            if (rms >= WAKE_SPEECH_RMS) {
              onsetFrames++;
              if (onsetFrames >= WAKE_ONSET_FRAMES) {
                wakeRecording = true;
                wakeChunks = [];
                speechStart = Date.now();
                onsetFrames = 0;
              }
            } else {
              onsetFrames = 0;
            }
          }

          if (wakeRecording && speechStart) {
            const elapsed = Date.now() - speechStart;

            // End of speech: WAKE_TAIL_MS of silence or POST_TRIGGER_MS elapsed
            const silenceDetected = rms < WAKE_SILENCE_RMS;
            const tailExceeded = silenceDetected && elapsed > WAKE_TAIL_MS;
            if (elapsed >= WAKE_POST_TRIGGER_MS || tailExceeded) {
              wakeRecording = false;
              speechStart = null;

              // Build WAV from captured chunks
              const totalLen = wakeChunks.reduce((s, c) => s + c.length, 0);
              if (totalLen < 800) {
                // Too short -- likely noise, ignore
                wakeChunks = [];
                return;
              }

              const merged = new Float32Array(totalLen);
              let off = 0;
              for (const chunk of wakeChunks) {
                merged.set(chunk, off);
                off += chunk.length;
              }
              wakeChunks = [];

              const downsampled = downsampleTo16k(merged, wakeSampleRate);
              const wavBuffer = encodeWav(downsampled, 16000);

              // Transcribe in background
              try {
                const result = await api.transcribe(wavBuffer);
                const transcript = result.text?.toLowerCase().trim() ?? "";

                if (!transcript || isWhisperArtifact(transcript)) return;
                if (!wakeActiveRef.current) return;

                // Check if transcript contains the wake word
                const matched = normalised.some((w) => transcript.includes(w));
                if (matched) {
                  // ── Barge-in mode: if pipeline is active, cancel it ──
                  if (pipelineActiveRef.current) {
                    cancelPipeline();
                    dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                    // Short delay then re-enter recording for next command
                    setTimeout(() => {
                      if (!wakeActiveRef.current) return;
                      dispatch({ type: "SET_VOICE_STATE", payload: "recording" });
                      recordWithVad().then((blob) => {
                        if (blob) {
                          dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
                          runVoicePipeline(blob);
                        } else {
                          dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                        }
                      });
                    }, 200);
                    return;
                  }

                  // ── Initial activation mode ──
                  playPingTone();
                  dispatch({ type: "SET_VOICE_STATE", payload: "recording" });

                  // Post-trigger continuation: keep recording for 2s more
                  // to capture any command spoken after the wake word.
                  // This mirrors Tauri's POST_TRIGGER_MS loop in audio.rs.
                  wakeRecording = true;
                  wakeChunks = []; // fresh buffer for continuation
                  // Re-add the original downsampled audio as the first chunk
                  wakeChunks.push(downsampled);
                  speechStart = Date.now();
                  const continuationStart = Date.now();
                  let continuationSilenceStart: number | null = null;

                  const continuationCheck = setInterval(() => {
                    if (!wakeActiveRef.current || !wakeAnalyser) {
                      clearInterval(continuationCheck);
                      return;
                    }
                    wakeAnalyser.getFloatTimeDomainData(timeDomainData);
                    const contRms = calculateRms(timeDomainData);
                    const elapsed = Date.now() - continuationStart;

                    // End continuation: 2s elapsed or 600ms silence after speech
                    if (contRms < WAKE_SILENCE_RMS) {
                      if (!continuationSilenceStart) continuationSilenceStart = Date.now();
                      else if (Date.now() - continuationSilenceStart > 600) {
                        clearInterval(continuationCheck);
                        finishWakeCapture();
                        return;
                      }
                    } else {
                      continuationSilenceStart = null;
                    }

                    if (elapsed >= WAKE_POST_TRIGGER_MS) {
                      clearInterval(continuationCheck);
                      finishWakeCapture();
                    }
                  }, 30);

                  function finishWakeCapture() {
                    wakeRecording = false;
                    speechStart = null;

                    // Build combined WAV (original + continuation)
                    const totalLen = wakeChunks.reduce((s, c) => s + c.length, 0);
                    const combined = new Float32Array(totalLen);
                    let cOff = 0;
                    for (const chunk of wakeChunks) {
                      combined.set(chunk, cOff);
                      cOff += chunk.length;
                    }
                    wakeChunks = [];

                    // The combined audio contains wake word + any continuation.
                    // runVoicePipeline will re-transcribe and strip the wake word.
                    const combinedDownsampled = combined.length > 0 && combined[0] !== undefined
                      ? combined // already at 16kHz (downsampled was the seed)
                      : downsampleTo16k(combined, wakeSampleRate);
                    const wavBuf = encodeWav(combinedDownsampled, 16000);
                    const blob = new Blob([wavBuf], { type: "audio/wav" });
                    dispatch({ type: "SET_VOICE_STATE", payload: "thinking" });
                    // Pass the wake word so it gets stripped from the transcript
                    runVoicePipeline(blob, wakeWord);
                  }
                }
              } catch {
                // Transcription failed -- silently ignore in wake mode
              }
            }
          }
        }, 30);
      })
      .catch((err) => {
        console.warn("Wake listener mic access failed:", err);
        dispatch({
          type: "SET_VOICE_ERROR",
          payload: "Mic access required for wake word detection. Please allow mic access.",
        });
        dispatch({ type: "SET_VOICE_STATE", payload: "error" });
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dispatch, onAudioLevel, recordWithVad, runVoicePipeline]);

  // ── Public: stopWakeListener ──────────────────────────────────

  const stopWakeListener = useCallback(() => {
    stopWakeListenerInternal();
  }, []);

  // ── Public: playPing ──────────────────────────────────────────

  const playPing = useCallback(() => {
    playPingTone();
  }, []);

  // ── Public: cancelPipeline ────────────────────────────────────

  const cancelPipeline = useCallback(() => {
    cancelledRef.current = true;
    pipelineActiveRef.current = false;

    // Abort in-flight fetch requests
    if (abortControllerRef.current) {
      abortControllerRef.current.abort();
      abortControllerRef.current = null;
    }

    // Stop thinking tone
    if (stopThinkingRef.current) {
      stopThinkingRef.current();
      stopThinkingRef.current = null;
    }

    // Stop TTS playback immediately
    if (ttsSourceRef.current) {
      try { ttsSourceRef.current.stop(); } catch { /* ignore */ }
      ttsSourceRef.current = null;
    }

    // Stop any active recording
    stopRecordingInternal();
    onAudioLevel?.(0);
  }, [onAudioLevel]);

  // ── Public: abortRecording ────────────────────────────────────

  const abortRecording = useCallback(() => {
    cancelledRef.current = true;
    stopRecordingInternal();
    cancelPipeline();
    onAudioLevel?.(0);
  }, [cancelPipeline, onAudioLevel]);

  // ── Return ────────────────────────────────────────────────────

  return {
    startRecording,
    stopRecording,
    recordWithVad,
    runVoicePipeline,
    startWakeListener,
    stopWakeListener,
    playPing,
    cancelPipeline,
    abortRecording,
  };
}
