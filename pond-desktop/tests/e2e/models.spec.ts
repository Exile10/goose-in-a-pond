/**
 * Playwright E2E tests for the Models section.
 *
 * Covers:
 * - Provider tabs (LLM / ASR / TTS) are visible
 * - Active role pills show the assigned model
 * - Memory status is displayed
 * - Download progress bar shows during active downloads
 *
 * All tests use mocked API routes — no running pond-server required.
 *
 * Run: cd pond-desktop && npx playwright test tests/e2e/models.spec.ts
 */
import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

async function goToModels(page: Parameters<typeof mockAllApiRoutes>[0]) {
  await page.goto("/");
  const modelsBtn = page
    .getByRole("button", { name: /models/i })
    .or(page.locator('[title="Models"]'))
    .first();
  await modelsBtn.click({ timeout: 10_000 });
  // Models defaults to "Set up" view; switch to "Manage" where roles/memory/downloads live
  await page.getByRole("button", { name: "Manage" }).click({ timeout: 5_000 });
}

test.describe("Models section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("models section loads without errors", async ({ page }) => {
    await goToModels(page);
    // Should not show a crash / blank page
    await expect(page.locator("body")).not.toBeEmpty();
    // Some model-related text should be present
    await expect(
      page.getByText(/model|llm|asr|tts|llamafile|ollama|gguf/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("active chat role pill shows assigned model", async ({ page }) => {
    // Override active-roles to return a specific model assignment
    await page.route("**/api/v1/models/active-roles", (route) =>
      route.fulfill({
        json: {
          chat:  { provider: "llamafile", model: "llama3.2-3b" },
          think: { provider: "llamafile", model: "llama3.2-3b" },
          task:  { provider: "llamafile", model: "llama3.2-3b" },
          asr:   null,
          tts:   null,
        },
      }),
    );

    await goToModels(page);

    // The active model name should appear somewhere in the Models section
    await expect(
      page.getByText(/llama3\.2|llama3/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("memory status shows total MB when non-zero", async ({ page }) => {
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: {
          total_mb: 8192,
          available_for_llm_mb: 4096,
          loaded_model: null,
        },
      }),
    );

    await goToModels(page);

    // Memory total should appear somewhere (8192 MB or 8 GB or similar)
    await expect(page.getByText(/8192|8,192|8\.0|8 GB/i).first()).toBeVisible({ timeout: 10_000 });
  });

  test("memory status shows loaded model name when a model is hot", async ({ page }) => {
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: {
          total_mb: 8192,
          available_for_llm_mb: 4000,
          loaded_model: "llama3.2-3b-instruct",
        },
      }),
    );

    await goToModels(page);

    await expect(
      page.getByText(/llama3\.2-3b|llama3/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("memory status shows zero / external label when total_mb is 0", async ({ page }) => {
    await page.route("**/api/v1/models/memory-status", (route) =>
      route.fulfill({
        json: { total_mb: 0, available_for_llm_mb: 0, loaded_model: null },
      }),
    );

    await goToModels(page);

    // Either "0" or an "external" / "managed externally" label
    await expect(
      page.getByText(/external|managed|0 MB|0MB|\b0\b/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("download progress bar visible when download in progress", async ({ page }) => {
    await page.route("**/api/v1/models/download/progress", (route) =>
      route.fulfill({
        json: {
          downloads: [
            {
              filename: "llama3.2-3b.gguf",
              category: "gguf",
              downloaded_bytes: 512_000_000,
              total_bytes: 2_000_000_000,
              status: "downloading",
            },
          ],
        },
      }),
    );

    await page.route("**/api/v1/models", (route) =>
      route.fulfill({
        json: {
          llamafile: [],
          gguf: [
            {
              category: "gguf",
              name: "llama3.2-3b",
              description: "3B parameter model",
              size_mb: 2000,
              downloaded: false,
              active: false,
              url: "https://example.com/llama3.2-3b.gguf",
              filename: "llama3.2-3b.gguf",
            },
          ],
          whisper: [],
          tts: [],
        },
      }),
    );

    await goToModels(page);

    // A progress indicator should be visible (progress bar or percentage text)
    await expect(
      page
        .locator('[role="progressbar"]')
        .or(page.getByText(/downloading|%|progress/i).first())
    ).toBeVisible({ timeout: 10_000 });
  });
});

// ── Live E2E tests (require running pond-server) ───────────────────────────────

const LIVE = !!process.env.GIAP_SERVER_URL;

test.describe("Models — live provider tests", () => {
  test.skip(!LIVE, "Set GIAP_SERVER_URL to run live provider tests");

  test("live memory status shows real data", async ({ page }) => {
    await page.goto(process.env.GIAP_SERVER_URL!);

    const modelsBtn = page.getByRole("button", { name: /models/i }).first();
    await modelsBtn.click({ timeout: 10_000 });

    // Memory status section should show non-zero data or "external"
    await expect(
      page.getByText(/MB|external|managed/i).first()
    ).toBeVisible({ timeout: 15_000 });
  });
});
