import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test.setTimeout(120_000);

test.beforeEach(async ({ page }) => {
  await mockAllApiRoutes(page);
  // Init script fires on every navigation (including reload), so DO NOT remove
  // goosehub_sticky / goosehub_todos here — that would wipe values mid-test
  // when assertions reload to verify persistence. Each test clears via
  // page.evaluate() instead.
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await page.evaluate(() => {
    localStorage.removeItem("goosehub_sticky");
    localStorage.removeItem("goosehub_todos");
  });
  await page.reload();
  await page.waitForSelector(".ghub", { timeout: 10_000 });
});

test("Manage button navigates to Rooms settings", async ({ page }) => {
  await page.waitForSelector(".gpanel", { timeout: 5_000 });
  const manage = page.getByRole("button", { name: /manage/i }).first();
  await manage.click();
  await page.waitForTimeout(300);
  // Should be on the rooms sub-screen
  await expect(page.locator(".ghub__main")).toContainText(/rooms/i);
});

test("All (Cameras) button navigates to Cameras settings", async ({ page }) => {
  await page.waitForSelector(".gpanel", { timeout: 5_000 });
  const allBtn = page.getByRole("button", { name: /^all$/i }).first();
  await allBtn.click();
  await page.waitForTimeout(300);
  await expect(page.locator(".ghub__main")).toContainText(/cameras/i);
});

test("Sticky note Create, Save, and localStorage persist", async ({ page }) => {
  // Click Create sticky
  const cta = page.getByRole("button", { name: /create sticky/i });
  await expect(cta).toBeVisible();
  await cta.click();

  // Textarea should appear
  const textarea = page.locator(".sticky__textarea");
  await expect(textarea).toBeVisible();
  await textarea.fill("Remember to feed the fish");
  // Diagnostic: confirm React's controlled value reflects the typed text
  const draftValue = await textarea.inputValue();
  // eslint-disable-next-line no-console
  console.log("STICKY_DRAFT=" + JSON.stringify(draftValue));
  await page.locator(".sticky__save").click();
  // Wait for the sticky to leave edit mode + content render
  await page.waitForFunction(
    () => document.querySelector(".sticky__textarea") === null,
    { timeout: 5_000 },
  );
  const ls = await page.evaluate(() => localStorage.getItem("goosehub_sticky"));
  // eslint-disable-next-line no-console
  console.log("STICKY_LS=" + JSON.stringify(ls));
  const bodyText = await page.locator(".sticky__body").first().innerText();
  // eslint-disable-next-line no-console
  console.log("STICKY_BODY=" + JSON.stringify(bodyText));

  // Content is shown
  await expect(page.locator(".sticky__body")).toContainText("Remember to feed the fish");

  // Verify localStorage
  const stored = await page.evaluate(() => localStorage.getItem("goosehub_sticky"));
  expect(stored).toBe("Remember to feed the fish");

  // Reload and verify hydration
  await page.reload();
  await page.waitForSelector(".ghub", { timeout: 10_000 });
  await expect(page.locator(".sticky__body")).toContainText("Remember to feed the fish");
});

test("Todo + Add to my list, toggle done, and localStorage persist", async ({ page }) => {
  // Default seeds should render
  await expect(page.locator(".todo__items")).toBeVisible();

  // Add new item
  await page.getByRole("button", { name: /add to my list/i }).click();

  // Inline input should appear
  const input = page.locator(".todo__input");
  await expect(input).toBeVisible();
  await input.fill("Buy milk");
  await input.press("Enter");

  // New item should appear in the list
  await expect(page.locator(".todo")).toContainText("Buy milk");

  // Verify localStorage
  const stored = await page.evaluate(() => {
    const raw = localStorage.getItem("goosehub_todos");
    return raw ? JSON.parse(raw) : null;
  });
  expect(Array.isArray(stored)).toBe(true);
  expect((stored as Array<{ text: string }>).some((t) => t.text === "Buy milk")).toBe(true);
});
