import { expect, test } from "@playwright/test";

/**
 * Live dashboard checks -- no mocks.
 *
 * Everything in `tests/e2e/` mocks its API with `page.route()`. That catches
 * component regressions and can never catch an API contract change: the mock
 * keeps returning the old shape long after the server stopped producing it.
 * These tests talk to the server that actually served the page.
 *
 * Started by `scripts/live-test.sh --ui`. Run alone with:
 *   POND_LIVE_URL=http://127.0.0.1:4000 \
 *     npx playwright test --config=playwright.live.config.ts
 */

test.describe("live dashboard", () => {
  test("the server serves a real UI, not the build.rs placeholder", async ({ page }) => {
    const response = await page.goto("/");
    expect(response?.status()).toBe(200);

    // `crates/pond-api/build.rs` emits a stub carrying this attribute when the
    // UI was never built. A release binary shipping it is a broken install,
    // and it looks like a working server until somebody opens a browser.
    const placeholder = await page.locator("html[data-giap-placeholder]").count();
    expect(
      placeholder,
      "the server is serving the build.rs placeholder -- run `npm run build` in pond-desktop first",
    ).toBe(0);

    await expect(page).toHaveTitle(/Goose In A Pond/i);
  });

  test("the page loads without console errors or failed requests", async ({ page }) => {
    const consoleErrors: string[] = [];
    const failed: string[] = [];

    page.on("console", (m) => {
      if (m.type() === "error") consoleErrors.push(m.text());
    });
    page.on("response", (r) => {
      // 401 is not a failure here: the dashboard is expected to probe routes
      // it may not be authorised for. A 5xx always is.
      if (r.status() >= 500) failed.push(`${r.status()} ${r.url()}`);
    });

    await page.goto("/");
    await page.waitForLoadState("networkidle");

    expect(failed, "server returned 5xx to the dashboard").toEqual([]);
    expect(consoleErrors, "dashboard logged console errors").toEqual([]);
  });

  test("the API the dashboard actually calls answers in the shape it expects", async ({
    request,
  }) => {
    // PondApiClient.listProfiles() reads `.profiles`. This is the ONLY
    // profile-related call the shipped UI makes -- verified by grepping
    // pond-desktop/src for the identity routes and finding nothing. If that
    // changes, this assertion is the cheapest place to notice.
    const res = await request.get("/api/v1/profiles");
    expect(res.status()).toBe(200);
    const body = await res.json();
    expect(
      Array.isArray(body.profiles),
      `listProfiles() expects {profiles: []}, server sent ${JSON.stringify(body).slice(0, 120)}`,
    ).toBe(true);
  });

  test("health is reachable from the browser context, same-origin", async ({ page }) => {
    // In a plain browser the app defaults its API base to window.location.origin
    // (defaultServerUrl() in PondApiClient.ts). This asserts that assumption
    // holds against the single-executable dashboard, not just in Tauri.
    await page.goto("/");
    const status = await page.evaluate(async () => {
      const r = await fetch("/api/v1/health");
      return r.status;
    });
    expect(status).toBe(200);
  });
});

/**
 * PAI-1 identity has NO UI surface.
 *
 * Verified by grep: pond-desktop/src calls exactly one profile route,
 * `GET /api/v1/profiles` (PondApiClient.listProfiles). There is no household
 * member removal, no session-identity binding, and no wake-on-face control
 * anywhere in the shipped app -- so the whole identity feature is reachable
 * only over the REST API.
 *
 * There is deliberately no test here pretending otherwise. When a UI lands,
 * this comment is the place to start, and `scripts/live_checks.py` already
 * covers the API side.
 */
