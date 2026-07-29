// ────────────────────────────────────────────────────────────
// WebVoiceBackend — conversational continuation
//
// Covers the two paths where one turn continues into another:
//   1. hands-free — reopen the mic after a reply instead of making the user
//      repeat the wake word;
//   2. a bare wake word ("Hey Goose" with no command) — wait for the command.
//
// Both re-enter the turn body. They used to recurse into `runPipeline`, whose
// `pipelineActive` guard rejects re-entry while a pipeline is running, so the
// follow-up was recorded and then silently dropped. These tests fail against
// that behaviour.
//
// The network/audio methods are stubbed on the instance (TypeScript `private`
// is compile-time only) so no Web Audio or mic mocking is needed.
// ────────────────────────────────────────────────────────────

import { describe, it, expect, vi, beforeEach } from "vitest";
import { WebVoiceBackend } from "./WebVoiceBackend";

// Audio helpers are irrelevant here — neutralise everything that touches the
// speaker or the tone generator, and keep dismissal detection real.
vi.mock("./webAudioUtils", async () => {
  const actual = await vi.importActual<Record<string, unknown>>("./webAudioUtils");
  return {
    ...actual,
    playThinkingTone: () => () => {},
    getQuip: () => "one moment",
    stopTtsPlayback: () => {},
    resetTtsInterrupt: () => {},
  };
});

const OPTS = { serverUrl: "http://test.local" };

/** A backend whose network + speaker calls are stubbed. */
function makeBackend(transcripts: string[]) {
  const backend = new WebVoiceBackend(OPTS.serverUrl);
  const raw = backend as unknown as Record<string, unknown>;

  // Each turn consumes the next transcript.
  const queue = [...transcripts];
  const transcribe = vi.fn(async (): Promise<string | null> => queue.shift() ?? null);
  const streamChat = vi.fn(async (_text: string, ..._rest: unknown[]) => {});
  const playTtsSentence = vi.fn(async (_text: string, ..._rest: unknown[]) => {});

  raw.transcribe = transcribe;
  raw.streamChat = streamChat;
  raw.playTtsSentence = playTtsSentence;

  return { backend, transcribe, streamChat, playTtsSentence };
}

const WAV = new Blob(["audio"], { type: "audio/wav" });

beforeEach(() => vi.clearAllMocks());

describe("hands-free", () => {
  it("reopens the mic after a reply and runs the follow-up turn", async () => {
    const { backend, transcribe, streamChat } = makeBackend([
      "turn on the kitchen light",
      "and the hallway one too",
    ]);
    // One follow-up, then silence ends the conversation.
    const recordWithVad = vi
      .fn()
      .mockResolvedValueOnce(WAV)
      .mockResolvedValueOnce(null);
    backend.recordWithVad = recordWithVad;

    await backend.runPipeline(WAV, { ...OPTS, handsFree: true });

    // Two turns actually reached the model — the follow-up was not dropped.
    expect(streamChat).toHaveBeenCalledTimes(2);
    expect(transcribe).toHaveBeenCalledTimes(2);
    expect(streamChat.mock.calls[1][0]).toBe("and the hallway one too");
  });

  it("ends the conversation when the user stays silent", async () => {
    const { backend, streamChat } = makeBackend(["what time is it"]);
    // No speech in the follow-up window.
    backend.recordWithVad = vi.fn().mockResolvedValue(null);

    const states: string[] = [];
    backend.onStateChange = (s: string) => states.push(s);

    await backend.runPipeline(WAV, { ...OPTS, handsFree: true });

    expect(streamChat).toHaveBeenCalledTimes(1);
    // Silence returns to idle rather than looping.
    expect(states[states.length - 1]).toBe("idle");
  });

  it("bounds the follow-up wait so the mic does not hang open", async () => {
    const { backend } = makeBackend(["hello"]);
    const recordWithVad = vi.fn().mockResolvedValue(null);
    backend.recordWithVad = recordWithVad;

    await backend.runPipeline(WAV, { ...OPTS, handsFree: true });

    // A no-speech timeout must be supplied; without it recordWithVad waits out
    // the full 30s maxDurationMs after every single reply.
    const vadOpts = recordWithVad.mock.calls[0][2];
    expect(vadOpts?.noSpeechTimeoutMs).toBeGreaterThan(0);
    expect(vadOpts?.noSpeechTimeoutMs).toBeLessThanOrEqual(15_000);
  });

  it("does not listen again when hands-free is off", async () => {
    const { backend, streamChat } = makeBackend(["turn off the light"]);
    const recordWithVad = vi.fn().mockResolvedValue(WAV);
    backend.recordWithVad = recordWithVad;

    await backend.runPipeline(WAV, { ...OPTS, handsFree: false });

    expect(streamChat).toHaveBeenCalledTimes(1);
    expect(recordWithVad).not.toHaveBeenCalled();
  });

  it("stops on dismissal instead of reopening the mic", async () => {
    const { backend, streamChat } = makeBackend(["goodbye"]);
    const recordWithVad = vi.fn().mockResolvedValue(WAV);
    backend.recordWithVad = recordWithVad;

    await backend.runPipeline(WAV, { ...OPTS, handsFree: true });

    // "goodbye" is a dismissal: no model call, and no follow-up listening.
    expect(streamChat).not.toHaveBeenCalled();
    expect(recordWithVad).not.toHaveBeenCalled();
  });
});

describe("bare wake word", () => {
  it("processes the command spoken after the wake word", async () => {
    // First utterance is the wake word alone; the command follows separately.
    const { backend, streamChat } = makeBackend([
      "hey goose",
      "set a timer for five minutes",
    ]);
    backend.recordWithVad = vi.fn().mockResolvedValue(WAV);

    await backend.runPipeline(WAV, { ...OPTS, stripWakeWord: "hey goose" });

    // Regression: the recursive turn used to hit the pipelineActive guard and
    // return immediately, so the command was recorded and never answered.
    expect(streamChat).toHaveBeenCalledTimes(1);
    expect(streamChat.mock.calls[0][0]).toBe("set a timer for five minutes");
  });
});
