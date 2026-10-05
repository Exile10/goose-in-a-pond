/** Models section E2E against mocked API routes: bands, picks, engines, add-ons, fit, downloads. */
import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";
import { navigateTo } from "./helpers/nav";
import {
  e2bQat, e4bQat, foundGguf, litertE4b, mockModels, ollamaModel, typicalModels,
  whisperBase,
} from "./helpers/model-mocks";

async function goToModels(page: Page) {
  await page.goto("/");
  // Models sits behind the drawer's "Manage" group; navigateTo expands it.
  await navigateTo(page, "Models");
  await expect(page.getByRole("heading", { name: "Jobs" })).toBeVisible({ timeout: 10_000 });
}

test.describe("Models section", () => {
  test.beforeEach(async ({ page }) => {
    await mockAllApiRoutes(page);
  });

  test("opens on Jobs, Recommended, On this device and Get more, with nothing coming down", async ({ page }) => {
    await mockModels(page);
    await goToModels(page);
    await expect(page.locator(".mdl-band__title")).toHaveText([
      "Jobs", "Recommended for this pond", "On this device", "Get more",
    ]);
    await expect(page.getByRole("heading", { name: "Coming down" })).toHaveCount(0);
  });

  test("an empty pond loads without errors and invites the first download", async ({ page }) => {
    await goToModels(page);
    await expect(page.getByText("Nothing downloaded yet. Pick one above, or add one below.")).toBeVisible();
  });

  test("a job names its model and the engine that runs it", async ({ page }) => {
    await mockModels(page);
    await goToModels(page);
    const jobs = page.locator(".mdl-role", { hasText: "Conversation" });
    await expect(jobs.locator(".mdl-role__holder")).toHaveText("Gemma 4 E2B");
    await expect(jobs).toContainText("llama.cpp");
    await expect(jobs).toContainText(".gguf");
    await expect(page.getByText("Nothing assigned")).toHaveCount(0);
  });

  test("Change names a control that exists", async ({ page }) => {
    await mockModels(page);
    await goToModels(page);
    await page.getByRole("button", { name: "Change the model for conversation" }).click();
    await expect(page.getByText("Press Use on the model you want for conversation.")).toBeVisible();
    await expect(page.getByRole("button", { name: /^Use .* for conversation$/ }).first()).toBeVisible();
  });

  test("the budget header and the rows judge fit against the same room", async ({ page }) => {
    await mockModels(page, { memory: { total_mb: 8192, available_for_llm_mb: 4096, loaded_model: null, reclaimable_mb: 0 } });
    await goToModels(page);
    // 4096 MB free, less the 1024 MB kept back for the model's working memory: 3072 MB.
    await expect(page.locator(".mdl-stat", { hasText: "for one model" })).toContainText("3.2 GB");
    // The Llama on disk weighs 1900 MB, which is 62% of it; the model in use says so instead.
    await expect(page.locator(".mdl-row", { hasText: "Llama-3.2-3B" }).locator(".mm-fit__pct")).toHaveText("62%");
    await expect(page.locator(".mdl-row", { hasText: "Gemma 4 E2B" }).first().locator(".mm-fit")).toHaveText("In use");
  });

  test("the header never goes blank when the machine reports no budget", async ({ page }) => {
    await mockModels(page, { memory: { total_mb: 0, available_for_llm_mb: 0, loaded_model: null } });
    await goToModels(page);
    const stat = page.locator(".mdl-stat", { hasText: "for one model" });
    await expect(stat.locator(".mdl-stat__num")).toHaveText("—");
    await expect(stat).toHaveAttribute("title", /does not report a memory budget/);
    // And no row claims a fit it cannot know.
    await expect(page.locator(".mm-fit--ok, .mm-fit--big")).toHaveCount(0);
  });

  test("an over-budget model reads 'Too big for this pond', never a percentage in the thousands", async ({ page }) => {
    await mockModels(page, {
      models: typicalModels({ gguf: [e4bQat(), e2bQat({ downloaded: true, size_mb: 9000 }), foundGguf({ size_mb: 1500 })] }),
      memory: { total_mb: 7620, available_for_llm_mb: 1030, loaded_model: null, reclaimable_mb: 0 },
    });
    await goToModels(page);
    await expect(page.getByText("Too big for this pond").first()).toBeVisible();
    expect(await page.locator("body").innerText()).not.toMatch(/\d{3,}%/);
  });

  test("counts what a switch frees, so the model in use never blocks its replacement", async ({ page }) => {
    await mockModels(page, {
      models: typicalModels({ gguf: [e4bQat({ downloaded: true }), e2bQat({ downloaded: true })], litert: [] }),
      roles: { chat: { provider: "local", model: "gemma-4-E2B-it-qat-UD-Q4_K_XL" }, tool: { model: null }, asr: { model_id: null }, tts: { model_id: null }, embedding: { model_id: null, model: "", provider: "fastembed" } },
      // Almost nothing is free beside the model in use, but switching away from it returns 4800 MB.
      memory: { total_mb: 7620, available_for_llm_mb: 1000, loaded_model: null, reclaimable_mb: 4800 },
    });
    await goToModels(page);
    const e4b = page.locator(".mdl-row", { hasText: "Gemma 4 E4B" });
    await expect(e4b.locator(".mm-fit--ok")).toBeVisible();
    await expect(e4b.getByText("Too big for this pond")).toHaveCount(0);
  });

  test.describe("Recommended for this pond", () => {
    test("shows the three picks with their reasons, the engine's file format, and one raised card", async ({ page }) => {
      await mockModels(page, { roles: { chat: null, tool: null, asr: null, tts: null, embedding: null } });
      await goToModels(page);
      const picks = page.locator(".mm-pick");
      await expect(picks).toHaveCount(3);
      await expect(picks.nth(0)).toContainText("Best answers this pond can run");
      await expect(picks.nth(0)).toContainText(".gguf");
      await expect(picks.nth(2)).toContainText(".litertlm");
      await expect(picks.nth(2)).toContainText("Text only");
      await expect(page.locator(".mm-pick[data-raised='true']")).toHaveCount(1);
      await expect(page.locator(".mm-btn--ask")).toHaveCount(1);
    });

    test("shows measured numbers only where the server sent them", async ({ page }) => {
      const measured = {
        rank: "primary", reason: "Best answers this pond can run",
        measured: { device: "orin", summary: "First reply in about 1 s, 15-16 tokens a second, 16k window", measured_on: "2026-10-05" },
      };
      await mockModels(page, { models: typicalModels({ gguf: [e4bQat({ recommended: measured }), e2bQat()] }) });
      await goToModels(page);
      await expect(page.getByText("First reply in about 1 s, 15-16 tokens a second, 16k window")).toBeVisible();
      await expect(page.getByText("Measured on this kind of device, 5 Oct 2026")).toBeVisible();
      await expect(page.getByText(/tokens a second/)).toHaveCount(1);
    });

    test("says the number before it is spent, and the add-on can be unticked", async ({ page }) => {
      await mockModels(page, { roles: { chat: null, tool: null, asr: null, tts: null, embedding: null } });
      let body: unknown = "unset";
      await page.route("**/api/v1/models/gguf/*/download", (route) => {
        body = route.request().postDataJSON();
        return route.fulfill({ json: { status: "download_started", message: "Downloading Gemma 4 E4B (4.2 GB)" } });
      });
      await goToModels(page);
      const card = page.locator(".mm-pick", { hasText: "llama.cpp" }).first();
      await expect(card).toContainText("4.2 GB + 945 MB for pictures");
      const tick = card.getByRole("checkbox", { name: /Include picture support/ });
      await expect(tick).toBeChecked();
      await tick.uncheck();
      await expect(card).not.toContainText("for pictures");
      await card.getByRole("button", { name: /^Download Gemma 4 E4B/ }).click();
      await expect.poll(() => body).toEqual({ pictures: false });
      await expect(page.getByText("Downloading Gemma 4 E4B (4.2 GB)")).toBeVisible();
    });

    test("downloads with picture support by default", async ({ page }) => {
      await mockModels(page, { roles: { chat: null, tool: null, asr: null, tts: null, embedding: null } });
      let body: unknown = "unset";
      await page.route("**/api/v1/models/gguf/*/download", (route) => {
        body = route.request().postDataJSON();
        return route.fulfill({ json: { status: "download_started" } });
      });
      await goToModels(page);
      await page.locator(".mm-pick", { hasText: "llama.cpp" }).first().getByRole("button", { name: /^Download/ }).click();
      await expect.poll(() => body).toEqual({ pictures: true });
    });
  });

  test.describe("On this device", () => {
    test("splits conversation by engine, each opened by one plain sentence, with a chip only where it informs", async ({ page }) => {
      await mockModels(page, { models: typicalModels({ litert: [litertE4b({ downloaded: true })] }) });
      await goToModels(page);
      const device = page.locator("#mdl-device");
      await expect(device).toContainText("llama.cpp runs .gguf files and can read pictures with an add-on.");
      await expect(device).toContainText("LiteRT-LM runs .litertlm files on the GPU, text only.");
      await expect(device).toContainText("Ollama is your own Ollama server; llamafile runs a model packed into one file.");
      const chips = await device.locator(".mm-chip").allInnerTexts();
      expect(chips.sort()).toEqual(["Found on disk", "Ollama", "Recommended", "Recommended"]);
      await expect(device.getByText("(detected on disk)")).toHaveCount(0);
    });

    test("reads picture support in the household's words, and adds it only when pressed", async ({ page }) => {
      await mockModels(page, { models: typicalModels({ litert: [litertE4b({ downloaded: true })] }) });
      let asked = 0;
      await page.route("**/api/v1/models/gguf/*/companions/pictures", (route) => {
        asked += 1;
        return route.fulfill({ json: { status: "download_started", message: "Downloading picture support for Gemma 4 E2B (941 MB)" } });
      });
      await goToModels(page);
      const device = page.locator("#mdl-device");
      await expect(device.getByText("Text only").first()).toBeVisible();
      expect(asked).toBe(0);
      await device.getByRole("button", { name: "Add pictures · 941 MB" }).click();
      await expect(page.getByText("Downloading picture support for Gemma 4 E2B (941 MB)")).toBeVisible();
      expect(asked).toBe(1);
    });

    test("says In use for the model in use and offers it no Use; Delete explains why not", async ({ page }) => {
      await mockModels(page);
      await goToModels(page);
      const row = page.locator(".mdl-row", { hasText: "Gemma 4 E2B" }).first();
      await expect(row.getByText("In use")).toBeVisible();
      await expect(row.getByRole("button", { name: /^Use / })).toHaveCount(0);
      await row.getByRole("button", { name: /Delete/ }).click();
      await expect(page.getByRole("alert")).toContainText("is doing a job right now");
    });

    test("keeps a helper out of Conversation", async ({ page }) => {
      const helper = { ...e2bQat(), id: "gguf/functiongemma", name: "functiongemma", title: "FunctionGemma 270M", downloaded: true, kind: "helper", recommended_role: "tool", recommended: undefined, companions: [] };
      await mockModels(page, { models: typicalModels({ gguf: [e2bQat({ downloaded: true }), helper] }) });
      await goToModels(page);
      await expect(page.getByText("FunctionGemma 270M")).toHaveCount(0);
    });
  });

  test.describe("what is coming down", () => {
    const parts = (status = "downloading", extra: Record<string, unknown> = {}) => [
      { filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", category: "gguf", downloaded_bytes: 1_200_000_000, total_bytes: 4_215_695_776, status, model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", part: "model", ...extra },
      { filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf", category: "mmproj", downloaded_bytes: 500_000_000, total_bytes: 991_552_320, status, model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", part: "pictures" },
    ];

    test("sits on the row it belongs to, a bar per file, one set of controls for the model", async ({ page }) => {
      await mockModels(page, { downloads: parts() });
      await goToModels(page);
      const card = page.locator(".mm-pick", { hasText: "llama.cpp" }).first();
      await expect(card.getByRole("progressbar")).toHaveCount(2);
      await expect(card).toContainText("1.2 GB of 4.2 GB");
      await expect(card).toContainText("476 MB of 945 MB");
      await expect(card.getByRole("button", { name: "Pause" })).toHaveCount(1);
      await expect(card.getByRole("button", { name: "Stop" })).toHaveCount(1);
      await expect(card.getByRole("button", { name: /^Download/ })).toHaveCount(0);
    });

    test("Stop is told to the model, not to one of its files, and says nothing was kept", async ({ page }) => {
      await mockModels(page, { downloads: parts() });
      let sent: unknown = null;
      await page.route("**/api/v1/models/download/control", (route) => {
        sent = route.request().postDataJSON();
        return route.fulfill({ json: { status: "cancelled", files: [] } });
      });
      await goToModels(page);
      await page.locator(".mm-pick", { hasText: "llama.cpp" }).first().getByRole("button", { name: "Stop" }).click();
      await expect.poll(() => sent).toEqual({ model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL", action: "cancel" });
      await expect(page.getByText("Stopped Gemma 4 E4B. Nothing was kept.")).toBeVisible();
    });

    test("says why a part failed", async ({ page }) => {
      const failed = parts();
      failed[1] = { ...failed[1], status: "error", error: "HTTP 503" } as never;
      await mockModels(page, { downloads: failed });
      await goToModels(page);
      await expect(page.getByRole("alert").filter({ hasText: "Could not finish: HTTP 503" })).toBeVisible();
      await expect(page.getByRole("button", { name: "Try again" })).toBeVisible();
    });
  });

  test.describe("Get more", () => {
    test("lists what the pond knows and is not here, by engine, and searches Hugging Face", async ({ page }) => {
      await mockModels(page, { models: typicalModels({ gguf: [e2bQat({ downloaded: true })], litert: [litertE4b({ recommended: undefined })], whisper: [whisperBase({ downloaded: false })], ollama: [] }) });
      await goToModels(page);
      const more = page.locator("#mdl-get");
      await expect(more).toContainText("LiteRT-LM runs .litertlm files on the GPU, text only.");
      await expect(more).toContainText("Whisper base.en (en)");
      await expect(more.getByRole("searchbox", { name: "Search Hugging Face for a model" })).toBeVisible();
    });

    test("says a Hugging Face file's add-on size before it is spent", async ({ page }) => {
      await mockModels(page);
      await page.route("**/api/v1/models/search/gguf?q=*", (route) =>
        route.fulfill({ json: { models: [{ id: "unsloth/gemma-4-E4B-it-qat-GGUF", downloads: 1200, likes: 5, tags: [], url: "x" }] } }),
      );
      await page.route("**/api/v1/models/search/gguf/files?repo=*", (route) =>
        route.fulfill({
          json: {
            files: [{ filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", size_mb: 4020, url: "https://huggingface.co/unsloth/gemma-4-E4B-it-qat-GGUF/resolve/main/gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf", pictures: { size_bytes: 991_552_320, label: "Gemma 4 E4B" } }],
          },
        }),
      );
      await goToModels(page);
      const more = page.locator("#mdl-get");
      await more.getByRole("searchbox").fill("gemma");
      await more.getByRole("button", { name: "Search" }).click();
      await more.getByRole("button", { name: /unsloth\/gemma-4-E4B-it-qat-GGUF/ }).click();
      await expect(more).toContainText("4.2 GB + 945 MB for pictures");
      await more.getByRole("checkbox", { name: /Include picture support/ }).uncheck();
      await expect(more.locator(".mdl-file__size")).toHaveText("4.2 GB");
    });
  });

  test("a pond with Ollama's models lists them without a Download or a Delete", async ({ page }) => {
    await mockModels(page, { models: typicalModels({ ollama: [ollamaModel()] }) });
    await goToModels(page);
    const row = page.locator(".mdl-row", { hasText: "qwen3:4b" });
    await expect(row.getByText("Runs in Ollama")).toBeVisible();
    await expect(row.getByRole("button", { name: /Delete|Download/ })).toHaveCount(0);
  });
});

// ── Live E2E tests (require running pond-server) ───────────────────────────────

const LIVE = !!process.env.GIAP_SERVER_URL;

test.describe("Models — live provider tests", () => {
  test.skip(!LIVE, "Set GIAP_SERVER_URL to run live provider tests");

  test("live memory status shows real data", async ({ page }) => {
    await page.goto(process.env.GIAP_SERVER_URL!);

    await navigateTo(page, "Models");

    // The room for one model reads a figure or a dash, never blank.
    await expect(page.locator(".mdl-stat", { hasText: "for one model" }).locator(".mdl-stat__num")).toHaveText(/\S/, { timeout: 15_000 });
  });
});
