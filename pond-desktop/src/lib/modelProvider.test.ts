import { describe, it, expect } from "vitest";
import { ENGINE_GROUPS, engineGroupKey, engineOf, groupByEngine, providerOf } from "./modelProvider";
import {
  ENGINES, e2b, e4b, foundOnDisk, litertE2b, litertE4b, llamafile, ollama, voice, whisper,
} from "../sections/models/fixtures";

describe("providerOf", () => {
  it("reads both of this device's own engines as local, and leaves the rest", () => {
    expect(providerOf("gguf")).toBe("local");
    expect(providerOf("litert")).toBe("local");
    expect(providerOf("local")).toBe("local");
    expect(providerOf("ollama")).toBe("ollama");
    expect(providerOf("llamafile")).toBe("llamafile");
  });
});

describe("engineOf", () => {
  it("takes the engine the server named", () => {
    expect(engineOf(e4b())).toEqual(ENGINES.LLAMA_CPP);
    expect(engineOf(litertE4b())).toEqual(ENGINES.LITERT);
  });

  it("derives it from the category when an older server named none", () => {
    expect(engineOf({ provider: "gguf", category: "gguf" })?.label).toBe("llama.cpp");
    expect(engineOf({ provider: "litert" })?.file_format).toBe(".litertlm");
    expect(engineOf({ provider: "ollama" })?.in_process).toBe(false);
    expect(engineOf({ provider: "llamafile" })?.label).toBe("llamafile");
  });

  it("has none for speech, voices and embeddings", () => {
    expect(engineOf(whisper())).toBeNull();
    expect(engineOf(voice())).toBeNull();
    expect(engineOf({ provider: "embedding", category: "embedding" })).toBeNull();
  });
});

describe("the engine sections", () => {
  it("open each with one plain sentence that explains the term", () => {
    expect(ENGINE_GROUPS.map((g) => g.blurb)).toEqual([
      "llama.cpp runs .gguf files and can read pictures with an add-on.",
      "LiteRT-LM runs .litertlm files on the GPU, text only.",
      "Ollama is your own Ollama server; llamafile runs a model packed into one file.",
    ]);
  });

  it("put Ollama and llamafile together, as the engines outside the pond's own", () => {
    expect(engineGroupKey(ENGINES.OLLAMA)).toBe("other");
    expect(engineGroupKey(ENGINES.LLAMAFILE)).toBe("other");
    expect(engineGroupKey({ id: "something_new" })).toBe("other");
    expect(engineGroupKey(null)).toBe("other");
  });

  it("split the models by engine in the order a household meets them, dropping empty ones", () => {
    const sections = groupByEngine([
      ollama(), litertE2b(), foundOnDisk(), llamafile(), e2b(), litertE4b(), e4b(),
    ]);
    expect(sections.map((s) => s.label)).toEqual(["llama.cpp", "LiteRT-LM", "Other engines"]);
    expect(sections[0].models.map((m) => m.name)).toEqual([
      "Llama-3.2-3B-Instruct-Q4_K_M", e2b().name, e4b().name,
    ]);
    expect(sections[2].models.map((m) => m.name)).toEqual(["qwen3:4b", "mistral-7b-instruct"]);
    expect(groupByEngine([e4b()]).map((s) => s.key)).toEqual(["llama_cpp"]);
    expect(groupByEngine([])).toEqual([]);
  });
});
