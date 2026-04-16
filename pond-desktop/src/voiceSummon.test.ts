import { describe, expect, it } from "vitest";
import { resolveSummonDecision } from "./voiceSummon";

describe("voiceSummon", () => {
  it("returns noop when server is offline", () => {
    expect(
      resolveSummonDecision({
        serverHealthy: false,
        isRecording: false,
        isProcessing: false,
      })
    ).toBe("noop");
  });

  it("returns stop-and-send when currently recording", () => {
    expect(
      resolveSummonDecision({
        serverHealthy: true,
        isRecording: true,
        isProcessing: false,
      })
    ).toBe("stop-and-send");
  });

  it("returns noop when already processing", () => {
    expect(
      resolveSummonDecision({
        serverHealthy: true,
        isRecording: false,
        isProcessing: true,
      })
    ).toBe("noop");
  });

  it("returns start-recording when healthy and idle", () => {
    expect(
      resolveSummonDecision({
        serverHealthy: true,
        isRecording: false,
        isProcessing: false,
      })
    ).toBe("start-recording");
  });
});
