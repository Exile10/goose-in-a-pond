import { resolve } from "path";
import { defineConfig } from "vitest/config";
import { inkAlias } from "../../jarida-ink/vite-alias";

export default defineConfig({
  // Vitest reads this file and not `vite.config.ts`, so anything the app's
  // config resolves has to be repeated here. The Ink entry points come from the
  // package itself rather than being written out twice.
  resolve: {
    alias: inkAlias(resolve(__dirname, "../../jarida-ink")),
  },
  test: {
    environment: "happy-dom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
  },
});
