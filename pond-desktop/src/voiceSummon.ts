export type SummonDecision = "start-recording" | "stop-and-send" | "noop";

export interface SummonDecisionInput {
  serverHealthy: boolean;
  isRecording: boolean;
  isProcessing: boolean;
}

export function resolveSummonDecision(input: SummonDecisionInput): SummonDecision {
  if (!input.serverHealthy) {
    return "noop";
  }

  if (input.isRecording) {
    return "stop-and-send";
  }

  if (input.isProcessing) {
    return "noop";
  }

  return "start-recording";
}
