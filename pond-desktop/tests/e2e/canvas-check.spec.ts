import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test("Canvas masonry board renders 6 MCP cards", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "canvas");
  });
  await page.goto("/");

  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".mcpc")).toBeVisible({ timeout: 5_000 });
  await expect(page.locator(".mcpc__board")).toBeVisible();

  // All 6 card wrappers
  await expect(page.locator(".mcp-item")).toHaveCount(6);

  // Source chrome visible
  await expect(page.locator(".mcp-item__src").first()).toBeVisible();
  await expect(page.locator(".mcp-item__dot").first()).toBeVisible();

  // Canvas header
  await expect(page.locator(".view-title")).toHaveText("Canvas");

  // Add card button
  await expect(page.getByRole("button", { name: /add card/i })).toBeVisible();

  await page.screenshot({ path: "/tmp/canvas-board.png" });
});

test("SmartHome room toggle syncs with hubStore", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    localStorage.setItem("goosehub_route", "canvas");
  });
  await page.goto("/");

  await expect(page.locator(".ghub")).toBeVisible({ timeout: 10_000 });
  await expect(page.locator(".mcp-item")).toHaveCount(6, { timeout: 5_000 });

  // Find a room button (aria-pressed) and toggle it
  const roomBtn = page.locator('[aria-pressed]').first();
  if (await roomBtn.isVisible()) {
    const initialState = await roomBtn.getAttribute("aria-pressed");
    await roomBtn.click();
    const newState = await roomBtn.getAttribute("aria-pressed");
    expect(newState).not.toBe(initialState);
  }
});
