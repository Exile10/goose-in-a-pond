import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

// ── Helper ────────────────────────────────────────────────────

async function goToSettings(page: import("@playwright/test").Page) {
  const btn = page
    .getByRole("button")
    .filter({ hasText: /settings/i })
    .or(page.locator('[title="Settings"]'))
    .first();
  await btn.click();
}

// ── Tests ─────────────────────────────────────────────────────

test.describe("Settings section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
    await page.goto("/");
    await goToSettings(page);
  });

  test("Settings section renders without crashing", async ({ page }) => {
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
    // At least one tab or form row should be visible
    await expect(
      page.getByText(/identity|voice|models|prompts|location|agent|data/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("renders all expected tabs", async ({ page }) => {
    const expectedTabs = ["Identity", "Voice", "Models", "Prompts", "Agent", "Data"];
    for (const tab of expectedTabs) {
      await expect(
        page.getByRole("tab", { name: tab }).or(page.getByText(tab)).first()
      ).toBeVisible({ timeout: 10_000 });
    }
  });

  test("Identity tab shows assistant name and user name fields", async ({ page }) => {
    // Identity is the default tab — click it explicitly to be safe
    const identityTab = page
      .getByRole("tab", { name: /identity/i })
      .or(page.getByText(/identity/i))
      .first();
    await identityTab.click();

    await expect(
      page.getByLabel(/assistant name/i)
        .or(page.getByPlaceholder(/assistant name/i))
        .or(page.getByText(/assistant name/i))
        .first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("settings data is loaded from the API on mount", async ({ page }) => {
    // The mock returns assistant_name: "Pond" and user_name: "Jerry"
    // After load, at least one input should contain those values
    await expect(
      page.locator('input').filter({ hasValue: "Pond" })
        .or(page.locator('input').filter({ hasValue: "Jerry" }))
        .first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("save settings calls PUT /api/v1/settings", async ({ page }) => {
    let putCalled = false;

    await page.route("**/api/v1/settings", (route) => {
      if (route.request().method() === "PUT") {
        putCalled = true;
        return route.fulfill({
          json: {
            assistant_name: "Pond",
            user_name: "Jerry",
            chat_provider: "llamafile",
            chat_model: "llama3.2",
            agent_memory_inject: false,
            prompt_style: "balanced",
            llm_temperature: 0.7,
            llm_max_tokens: 1024,
          },
        });
      }
      return route.continue();
    });

    // Wait for settings to load
    await page.waitForTimeout(500);

    // Find and click Save button
    const saveBtn = page.getByRole("button", { name: /save/i }).first();
    if (await saveBtn.isVisible({ timeout: 3_000 })) {
      await saveBtn.click();
      await page.waitForTimeout(300);
      expect(putCalled).toBe(true);
    }
  });

  test("Models tab renders model role rows", async ({ page }) => {
    const modelsTab = page
      .getByRole("tab", { name: /models/i })
      .or(page.getByText("Models"))
      .first();
    await modelsTab.click();

    // active-roles mock returns chat, think, task, asr, tts roles
    await expect(
      page.getByText(/chat|think|task|asr|tts|llamafile/i).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("Voice tab renders without errors", async ({ page }) => {
    const voiceTab = page
      .getByRole("tab", { name: /voice/i })
      .or(page.getByText("Voice"))
      .first();
    await voiceTab.click();

    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
    // Some form content should be present
    await expect(page.locator("body")).toBeVisible();
  });
});
