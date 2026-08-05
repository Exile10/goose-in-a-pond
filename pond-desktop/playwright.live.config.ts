import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright config for LIVE tests -- against a real running pond-server.
 *
 * The default `playwright.config.ts` points at the Vite dev server and every
 * test there mocks its API with `page.route()`. That is the right shape for
 * component behaviour and it can never catch an API contract change: the mock
 * keeps returning the old shape long after the server stopped producing it.
 *
 * This config mocks nothing. It drives the dashboard the server actually
 * serves, talking to the server that actually built it. Start it with
 * `scripts/live-test.sh --ui`, which builds the UI, starts a server against a
 * scratch data directory, and points POND_LIVE_URL here.
 *
 * There is deliberately no `webServer` block. The server is owned by
 * live-test.sh, which also does the migration-idempotence restart and the
 * no-bypass auth pass -- neither of which Playwright should be driving.
 */

const baseURL = process.env.POND_LIVE_URL ?? "http://127.0.0.1:4000";

export default defineConfig({
  testDir: "./tests/e2e-live",
  fullyParallel: false,
  workers: 1,
  timeout: 60_000,
  expect: { timeout: 15_000 },
  // No retries. A live test that passes on the second attempt is telling you
  // something about startup ordering, and retrying hides it.
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL,
    headless: true,
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
