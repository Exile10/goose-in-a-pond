import type { ModelEngine, ModelEntry } from "../api/types";

/** The provider the server stores for a model category. A GGUF file runs on llama.cpp and a
 *  `.litertlm` file on LiteRT-LM, but both are this device's own engine, and activating either
 *  records `chat_provider = "local"`; the server still reads `gguf`, which older settings saved, as
 *  `local` too. Compare providers through this, never a category with a stored provider. */
export function providerOf(categoryOrProvider: string): string {
  return categoryOrProvider === "gguf" || categoryOrProvider === "litert"
    ? "local"
    : categoryOrProvider;
}

/** The engine of a conversation category, for a server that does not send `engine` itself. */
const ENGINE_BY_CATEGORY: Record<string, ModelEngine> = {
  gguf: { id: "llama_cpp", label: "llama.cpp", file_format: ".gguf", in_process: true },
  litert: { id: "litert_lm", label: "LiteRT-LM", file_format: ".litertlm", in_process: true },
  ollama: { id: "ollama", label: "Ollama", file_format: null, in_process: false },
  llamafile: { id: "llamafile", label: "llamafile", file_format: ".llamafile", in_process: false },
};

/** What runs a conversation model; null for speech, voice and embedding rows. */
export function engineOf(
  m: Pick<ModelEntry, "engine" | "category" | "provider">,
): ModelEngine | null {
  if (m.engine) return m.engine;
  return ENGINE_BY_CATEGORY[m.category ?? m.provider] ?? ENGINE_BY_CATEGORY[m.provider] ?? null;
}

export type EngineGroupKey = "llama_cpp" | "litert_lm" | "other";

export interface EngineGroup {
  key: EngineGroupKey;
  label: string;
  /** The extension it loads; null for the group of engines that load different things. */
  format: string | null;
  /** One plain sentence that explains the term where it first appears. */
  blurb: string;
}

/** The sections conversation models are read under, in order. Ollama and llamafile share one: both
 *  run outside the pond's own engines. */
export const ENGINE_GROUPS: readonly EngineGroup[] = [
  {
    key: "llama_cpp",
    label: "llama.cpp",
    format: ".gguf",
    blurb: "llama.cpp runs .gguf files and can read pictures with an add-on.",
  },
  {
    key: "litert_lm",
    label: "LiteRT-LM",
    format: ".litertlm",
    blurb: "LiteRT-LM runs .litertlm files on the GPU, text only.",
  },
  {
    key: "other",
    label: "Other engines",
    format: null,
    blurb: "Ollama is your own Ollama server; llamafile runs a model packed into one file.",
  },
];

export function engineGroupKey(engine: Pick<ModelEngine, "id"> | null): EngineGroupKey {
  if (engine?.id === "llama_cpp") return "llama_cpp";
  if (engine?.id === "litert_lm") return "litert_lm";
  return "other";
}

export interface EngineSection<T> extends EngineGroup {
  models: T[];
}

/** Models split by the engine that runs them, in `ENGINE_GROUPS` order; empty sections dropped. */
export function groupByEngine<T extends Pick<ModelEntry, "engine" | "category" | "provider">>(
  models: T[],
): EngineSection<T>[] {
  return ENGINE_GROUPS.map((group) => ({
    ...group,
    models: models.filter((m) => engineGroupKey(engineOf(m)) === group.key),
  })).filter((section) => section.models.length > 0);
}
