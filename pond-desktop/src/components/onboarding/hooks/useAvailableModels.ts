// ────────────────────────────────────────────────────────────
// useAvailableModels — Fetches live model list for the Model step
// Returns LLM models grouped by provider, plus ASR and TTS lists.
// ────────────────────────────────────────────────────────────

import { useState, useEffect } from "react";
import { api } from "../../../api/PondApiClient";
import type { ModelEntry } from "../../../api/types";

interface ProviderModels {
  provider: string;
  models: ModelEntry[];
}

interface AvailableModelsState {
  /** LLM models grouped by provider (llamafile, ollama, gguf) */
  grouped: ProviderModels[];
  /** Whisper ASR models */
  asrModels: ModelEntry[];
  /** Piper TTS models */
  ttsModels: ModelEntry[];
  loading: boolean;
  error: string | null;
}

export function useAvailableModels(): AvailableModelsState {
  const [state, setState] = useState<AvailableModelsState>({
    grouped: [],
    asrModels: [],
    ttsModels: [],
    loading: true,
    error: null,
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const models = await api.listModels();
        if (cancelled) return;

        const llmGroups = new Map<string, ModelEntry[]>();
        const asr: ModelEntry[] = [];
        const tts: ModelEntry[] = [];

        for (const m of models) {
          const cat = m.category || "unknown";
          if (cat === "whisper") {
            asr.push(m);
          } else if (cat === "tts") {
            tts.push(m);
          } else {
            if (!llmGroups.has(cat)) llmGroups.set(cat, []);
            llmGroups.get(cat)!.push(m);
          }
        }

        setState({
          grouped: Array.from(llmGroups.entries()).map(([provider, models]) => ({
            provider,
            models,
          })),
          asrModels: asr,
          ttsModels: tts,
          loading: false,
          error: null,
        });
      } catch (err) {
        if (!cancelled) {
          setState({
            grouped: [],
            asrModels: [],
            ttsModels: [],
            loading: false,
            error: err instanceof Error ? err.message : "Failed to load models",
          });
        }
      }
    })();
    return () => { cancelled = true; };
  }, []);

  return state;
}
