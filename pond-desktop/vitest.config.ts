import { defineConfig } from "vitest/config";

// Two projects, because the two halves of this app run in different worlds.
// The renderer needs happy-dom and the React setup file; the main process is
// plain Node and must not get either -- a main-process module that only works
// because a DOM happened to be present is a module that will fail in Electron.
export default defineConfig({
  test: {
    projects: [
      {
        test: {
          name: "renderer",
          environment: "happy-dom",
          include: ["src/**/*.test.{ts,tsx}"],
          setupFiles: ["src/test-setup.ts"],
        },
      },
      {
        test: {
          name: "main",
          environment: "node",
          include: ["electron/**/*.test.ts"],
        },
      },
    ],
  },
});
