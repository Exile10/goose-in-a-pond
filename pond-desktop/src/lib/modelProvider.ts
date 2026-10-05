/** The provider the server stores for a model category. A GGUF file runs on llama.cpp and a
 *  `.litertlm` file on LiteRT-LM, but both are this device's own engine, and activating either
 *  records `chat_provider = "local"`; the server still reads `gguf`, which older settings saved, as
 *  `local` too. Compare providers through this, never a category with a stored provider. */
export function providerOf(categoryOrProvider: string): string {
  return categoryOrProvider === "gguf" || categoryOrProvider === "litert"
    ? "local"
    : categoryOrProvider;
}
