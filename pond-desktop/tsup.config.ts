import { defineConfig } from "tsup";

// The main process and preload only. The renderer stays on Vite, untouched:
// its output is embedded into pond-server with include_dir! and served over
// HTTP on the Jetson, so a bundler with opinions about it is a liability.
//
// CJS because a sandboxed preload must be CommonJS, and `electron` is external
// because it is provided by the runtime, not bundled.
export default defineConfig({
  entry: ["electron/main/index.ts", "electron/preload/index.ts"],
  outDir: "dist-electron",
  format: "cjs",
  platform: "node",
  target: "node20",
  external: ["electron"],
  clean: true,
  sourcemap: true,
  // package.json declares "type": "module", so a .js file here would be loaded
  // as ESM and every `require` in the bundle would throw. The extension is
  // what settles it, not the format flag.
  outExtension: () => ({ js: ".cjs" }),
});
