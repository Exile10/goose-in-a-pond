export type VoiceDecision = "start-recording" | "stop-and-send" | "noop";

export interface VoiceDecisionInput {
  serverHealthy: boolean;
  isRecording: boolean;
  isProcessing: boolean;
}

export function resolveVoiceDecision(input: VoiceDecisionInput): VoiceDecision {
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
