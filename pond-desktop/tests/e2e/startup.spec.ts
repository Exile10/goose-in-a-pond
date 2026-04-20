import { test, expect } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

test.beforeEach(async ({ page }) => {
  await mockAllApiRoutes(page);
});

test.describe("App startup", () => {
  test("app loads and renders the sidebar", async ({ page }) => {
    await page.goto("/");
    // <aside aria-label="Navigation"> — ARIA role is "complementary" for <aside>
    await expect(page.locator('aside[aria-label="Navigation"]')).toBeVisible({ timeout: 10_000 });
  });

  test("sidebar shows core navigation items", async ({ page }) => {
    await page.goto("/");
    await expect(page.locator('aside[aria-label="Navigation"]')).toBeVisible({ timeout: 10_000 });
    // Collapsed sidebar shows icon-only buttons with title attributes
    // Expanded sidebar shows text labels — wait for either
    await expect(
      page.getByRole("button").filter({ hasText: /dashboard|home/i }).or(
        page.locator('[title="Dashboard"], [title="Home"]')
      ).first()
    ).toBeVisible({ timeout: 10_000 });
  });

  test("server status indicator is visible", async ({ page }) => {
    await page.goto("/");
    // Status dot exists — health + handshake mocked so it should show Connected
    await expect(page.locator('[style*="border-radius: 50%"]').first()).toBeVisible();
  });

  test("Voice mode button is present in sidebar footer", async ({ page }) => {
    await page.goto("/");
    // The sidebar footer has a button with aria-label="Voice mode"
    await expect(
      page.locator('[aria-label="Voice mode"]').first()
    ).toBeVisible();
  });

  test("Settings section loads without errors", async ({ page }) => {
    await page.goto("/");

    // Navigate to Settings
    const settingsBtn = page.getByRole("button", { name: /settings/i }).first();
    await settingsBtn.click();

    // Identity tab (default) should render a recognisable field
    await expect(
      page.getByText(/identity|assistant name|user name/i).first()
    ).toBeVisible({ timeout: 10_000 });

    // No unhandled error overlay
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("Dashboard section renders without errors", async ({ page }) => {
    await page.goto("/");

    // Dashboard is typically the default section or first nav item
    const dashBtn = page
      .getByRole("button")
      .filter({ hasText: /dashboard/i })
      .or(page.locator('[title="Dashboard"]'))
      .first();

    if (await dashBtn.isVisible()) {
      await dashBtn.click();
    }

    // Should not show a crash / unhandled error
    await expect(page.getByText(/something went wrong/i)).not.toBeVisible();
  });

  test("page title is present", async ({ page }) => {
    await page.goto("/");
    // Any non-empty title indicates the shell rendered
    const title = await page.title();
    expect(title.length).toBeGreaterThan(0);
  });
});
