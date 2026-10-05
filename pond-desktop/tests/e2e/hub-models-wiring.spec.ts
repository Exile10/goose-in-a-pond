/** Models sub-screen (Settings > Models) wired to listModels and activateModel, on the real shapes. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import { mockModels, ollamaModel, typicalModels, whisperBase, type ModelMocks } from "./helpers/model-mocks";

const NO_ROLES = {
  chat: null, tool: null, asr: null, tts: null,
  embedding: { model_id: null, model: "", provider: "fastembed" },
};

const E2B_IN_USE = {
  ...NO_ROLES,
  chat: { provider: "local", model: "gemma-4-E2B-it-qat-UD-Q4_K_XL", model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL" },
};

/** Call before navigating, so the routes are in place. */
async function setupHub(page: Page, opts: ModelMocks = {}) {
  await mockAllApiRoutes(page);
  await mockModels(page, opts);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "home");
  });
}

async function goToModelsScreen(page: Page) {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  // Models is a "Manage" chip in the drawer and opens the Models detail screen directly.
  await navigateTo(page, "Models");
  await expect(page.getByRole("heading", { name: "Models" })).toBeVisible({ timeout: 10_000 });
}

test.describe("Hub — Models sub-screen wiring", () => {
  test("shows the four jobs, the picks still to get, and what is here by engine", async ({ page }) => {
    await setupHub(page, { roles: E2B_IN_USE });
    await goToModelsScreen(page);

    const tiles = page.getByRole("list", { name: "Which model does each job" }).getByRole("listitem");
    await expect(tiles).toHaveCount(4);
    await expect(tiles.nth(0)).toContainText("Conversation");
    await expect(tiles.nth(0)).toContainText("Gemma 4 E2B");
    await expect(tiles.nth(0)).toContainText("llama.cpp");
    await expect(page.getByText("Think", { exact: true })).toHaveCount(0);
    await expect(page.getByText("Task", { exact: true })).toHaveCount(0);

    // The picks not yet here, with their size said first; the one that is here is listed below.
    const recommended = page.locator(".setcard", { hasText: "Recommended for this pond" });
    await expect(recommended.locator(".mm-pick")).toHaveCount(2);
    await expect(recommended).toContainText("4.2 GB + 945 MB for pictures");

    const conversation = page.locator(".setcard", { hasText: "Conversation" }).last();
    await expect(conversation).toContainText("llama.cpp runs .gguf files and can read pictures with an add-on.");
    await expect(conversation).toContainText("Ollama is your own Ollama server; llamafile runs a model packed into one file.");
    await expect(conversation.getByText("(detected on disk)")).toHaveCount(0);
  });

  test("does not list the whole catalogue, and has no Download button that can never be pressed", async ({ page }) => {
    await setupHub(page, {
      models: typicalModels({
        whisper: [whisperBase({ downloaded: false }), whisperBase({ id: "whisper/small", name: "small", title: "Whisper small (en)", downloaded: false })],
      }),
    });
    await goToModelsScreen(page);
    await expect(page.getByText("Whisper small (en)")).toHaveCount(0);
    await expect(page.locator("[title*='coming in Phase']")).toHaveCount(0);
    await expect(page.locator(".setd button:disabled")).toHaveCount(0);
  });

  test("Use calls activateModel with the model's real category and the chat role, and says In use", async ({ page }) => {
    await setupHub(page, { roles: NO_ROLES });
    let activated: { url: string; body: unknown } | null = null;
    await page.route("**/api/v1/models/*/*/activate", (route) => {
      activated = { url: route.request().url(), body: route.request().postDataJSON() };
      return route.fulfill({ json: { role: "chat", model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL" } });
    });
    await goToModelsScreen(page);

    // After activation the pond reports the model in use, and the screen reads it again.
    await page.route("**/api/v1/models/active-roles", (route) =>
      route.fulfill({ json: activated ? E2B_IN_USE : NO_ROLES }),
    );
    await page.getByRole("button", { name: "Use Gemma 4 E2B, llama.cpp for conversation" }).click();
    await expect.poll(() => activated?.url).toContain("/api/v1/models/gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL/activate");
    expect(activated!.body).toEqual({ role: "chat" });
    await expect(page.getByText("Now using Gemma 4 E2B for conversation.")).toBeVisible();
    await expect(page.locator(".hm-row[data-inuse='true']")).toContainText("Gemma 4 E2B");
    await expect(page.locator(".hm-row[data-inuse='true']")).toContainText("In use");
  });

  test("an uninstalled LiteRT-LM pick is downloaded from its own card, text only, with no add-on to tick", async ({ page }) => {
    await setupHub(page, { roles: E2B_IN_USE });
    let body: unknown = "unset";
    await page.route("**/api/v1/models/litert/*/download", (route) => {
      body = route.request().postDataJSON();
      return route.fulfill({ json: { status: "download_started", message: "Downloading Gemma 4 E4B (3.7 GB)" } });
    });
    await goToModelsScreen(page);
    const card = page.locator(".mm-pick", { hasText: "LiteRT-LM" });
    await expect(card.getByRole("checkbox")).toHaveCount(0);
    await card.getByRole("button", { name: /^Download/ }).click();
    await expect.poll(() => body).toBeNull();
    await expect(page.getByText("Downloading Gemma 4 E4B (3.7 GB)")).toBeVisible();
  });

  test("speech lists the listening model that is here, and sends a household to Voice for a voice", async ({ page }) => {
    await setupHub(page, { roles: E2B_IN_USE });
    await goToModelsScreen(page);
    const speech = page.locator(".setcard", { hasText: "Speech" });
    await expect(speech).toContainText("Whisper base.en (en)");
    await expect(speech.getByRole("button", { name: /^Use Whisper base.en/ })).toBeVisible();
    await speech.getByRole("button", { name: "Choose a voice" }).click();
    await expect(page.getByRole("heading", { name: "Voice" })).toBeVisible();
  });

  test("lists Ollama's models as in-use candidates with no download", async ({ page }) => {
    await setupHub(page, { models: typicalModels({ ollama: [ollamaModel()] }), roles: E2B_IN_USE });
    await goToModelsScreen(page);
    const row = page.locator(".hm-row", { hasText: "qwen3:4b" });
    await expect(row.getByRole("button", { name: /Use qwen3:4b/ })).toBeVisible();
    await expect(row.getByRole("button", { name: /Download|Delete/ })).toHaveCount(0);
  });

  test("every target on the screen is at least 44px tall", async ({ page }) => {
    await setupHub(page, { roles: E2B_IN_USE, downloads: [
      { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", category: "gguf", downloaded_bytes: 1_200_000_000, total_bytes: 4_215_695_776, status: "downloading", model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", part: "model" },
    ] });
    await goToModelsScreen(page);
    await expect(page.locator(".setd .mm-btn").first()).toBeVisible();
    const small = await page.evaluate(() =>
      Array.from(document.querySelectorAll<HTMLElement>(".setd button, .setd label.mm-choice, .setd__back"))
        .filter((el) => !el.matches(".setd__back"))
        .map((el) => ({ text: (el.textContent ?? "").trim().slice(0, 30), h: Math.round(el.getBoundingClientRect().height) }))
        // The add-on link reaches 44px through an invisible extension (`.reach`).
        .filter((t) => t.h < 44 && !t.text.startsWith("Add pictures")),
    );
    expect(small, `Targets under 44px: ${JSON.stringify(small)}`).toEqual([]);
  });

  test("follows dark mode and the chosen accent, because it writes no colour of its own", async ({ page }) => {
    await setupHub(page, { roles: E2B_IN_USE, downloads: [
      { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", category: "gguf", downloaded_bytes: 1_200_000_000, total_bytes: 4_215_695_776, status: "downloading", model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", part: "model" },
    ] });
    await page.addInitScript(() => {
      localStorage.setItem("goosehub_theme", "Dark");
      localStorage.setItem("goosehub_accent", "Teal");
    });
    await goToModelsScreen(page);
    await expect(page.locator(".mm-xfer__fill").first()).toBeVisible();
    const colours = await page.evaluate(() => {
      const css = (sel: string, prop: string) => getComputedStyle(document.querySelector(sel)!).getPropertyValue(prop);
      return {
        tile: css(".hm-tile", "background-color"),
        title: css(".hm-tile__holder", "color"),
        fill: css(".mm-xfer__fill", "background-color"),
      };
    });
    // Dark panel and light text; the progress is the teal accent, not the purple it used to be.
    expect(colours.tile).toBe("rgb(30, 27, 38)");
    expect(colours.title).toBe("rgb(243, 241, 248)");
    expect(colours.fill).toBe("rgb(13, 148, 136)");
  });
});
