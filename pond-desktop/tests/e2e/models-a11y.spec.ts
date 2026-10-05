/** WCAG 2 A/AA scan of every surface that chooses a model, in both schemes. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { mockModels, type ModelMocks } from "./helpers/model-mocks";

const NO_ROLES = {
  chat: null, tool: null, asr: null, tts: null,
  embedding: { model_id: null, model: "", provider: "fastembed" },
};

const COMING_DOWN = [
  { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", category: "gguf", downloaded_bytes: 1_200_000_000, total_bytes: 4_215_695_776, status: "downloading", model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", part: "model" },
  { filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf", category: "mmproj", downloaded_bytes: 500_000_000, total_bytes: 991_552_320, status: "error", error: "HTTP 503", model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", part: "pictures" },
];

async function scan(page: Page, within: string, label: string) {
  const results = await new AxeBuilder({ page }).include(within).withTags(["wcag2a", "wcag2aa"]).analyze();
  const found = results.violations.flatMap((v) =>
    v.nodes.map((n) => `${v.id}: ${n.target.join(" ")} (${n.failureSummary?.split("\n")[1]?.trim()})`),
  );
  expect(found, label).toEqual([]);
}

/** Registered before navigating; the settings read is what says no conversation model is chosen. */
async function setup(page: Page, opts: ModelMocks, scheme: "Light" | "Dark", section: string, route?: string) {
  await mockAllApiRoutes(page);
  await mockModels(page, opts);
  await page.route("**/api/v1/settings", (r) =>
    r.fulfill({ json: { assistant_name: "Pond", user_name: "Jerry", chat_provider: "", chat_model: "" } }),
  );
  await page.addInitScript(({ scheme, section, route }) => {
    localStorage.setItem("goosehub_theme", scheme);
    localStorage.setItem("giap-section", section);
    if (section === "hub") {
      localStorage.setItem("giap-force-hub", "1");
      localStorage.setItem("goosehub_route", route ?? "home");
    }
  }, { scheme, section, route });
}

for (const scheme of ["Light", "Dark"] as const) {
  test.describe(`Models accessibility, ${scheme.toLowerCase()}`, () => {
    test("the classic Models page, with a download coming down and one that failed", async ({ page }) => {
      await setup(page, { downloads: COMING_DOWN }, scheme, "models");
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Jobs" })).toBeVisible({ timeout: 10_000 });
      await expect(page.getByRole("alert").filter({ hasText: "Could not finish: HTTP 503" })).toBeVisible();
      await scan(page, ".mdl", "classic Models");
    });

    test("the hub Models screen", async ({ page }) => {
      await setup(page, { downloads: COMING_DOWN }, scheme, "hub");
      await page.setViewportSize({ width: 1024, height: 600 });
      await page.goto("/");
      await page.waitForSelector(".ghub", { timeout: 10_000 });
      await navigateTo(page, "Models");
      await expect(page.getByRole("heading", { name: "Models" })).toBeVisible({ timeout: 10_000 });
      await expect(page.locator(".hm-tile").first()).toBeVisible();
      await scan(page, ".ghub__main", "hub Models");
    });

    test("a chat with no conversation model, and the model list opened from its chip", async ({ page }) => {
      await setup(page, { roles: NO_ROLES }, scheme, "chat");
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Pick a model to talk with" })).toBeVisible({ timeout: 10_000 });
      await scan(page, ".nm", "no-model picks");

      await page.getByRole("button", { name: "Select model" }).click();
      // The list fades in over 120ms; scanned part-way it is part transparent, and so low in contrast.
      await expect(page.locator(".model-selector-dropdown")).toHaveCSS("opacity", "1");
      await scan(page, ".model-selector-wrap", "model list");
    });

    test("the hub chat with no conversation model", async ({ page }) => {
      await setup(page, { roles: NO_ROLES }, scheme, "hub", "chat");
      await page.setViewportSize({ width: 800, height: 480 });
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Pick a model to talk with" })).toBeVisible({ timeout: 10_000 });
      await scan(page, ".nm", "hub no-model picks");
    });
  });
}
