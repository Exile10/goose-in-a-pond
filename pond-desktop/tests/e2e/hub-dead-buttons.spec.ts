import { test } from "@playwright/test";
import { mockAllApiRoutes } from "./helpers/api-mocks";

interface ButtonInfo {
  view: string;
  text: string;
  effect: string; // 'route-change' | 'dom-change' | 'console' | 'no-op' | 'error'
}

test.setTimeout(180_000);
test("Hub dead-button audit", async ({ page }) => {
  await mockAllApiRoutes(page);
  await page.addInitScript(() => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("goosehub_route", "home");
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/");
  await page.waitForSelector(".ghub", { timeout: 10_000 });

  const VIEWS = ["Home", "Goose", "Canvas", "Routines", "Settings"];
  const inventory: Array<{ view: string; text: string; hasOnClick: boolean; cls: string }> = [];

  for (const viewName of VIEWS) {
    await page.getByRole("button", { name: viewName, exact: true }).first().click();
    await page.waitForTimeout(300);

    const buttons = await page.evaluate(() => {
      const out: Array<{ text: string; hasOnClick: boolean; cls: string }> = [];
      const main = document.querySelector(".ghub__main");
      if (!main) return out;
      main.querySelectorAll("button").forEach((b) => {
        const text = (b.textContent || "").trim().slice(0, 50) || "(no text)";
        // React attaches event handlers via __reactProps$… - look for it
        const propsKey = Object.keys(b).find((k) => k.startsWith("__reactProps$"));
        const props = propsKey ? (b as unknown as Record<string, { onClick?: unknown }>)[propsKey] : undefined;
        const hasOnClick = !!(props && props.onClick);
        out.push({ text, hasOnClick, cls: b.className.slice(0, 40) });
      });
      return out;
    });

    for (const b of buttons) inventory.push({ view: viewName, ...b });
  }

  const dead = inventory.filter((b) => !b.hasOnClick);
  // eslint-disable-next-line no-console
  console.log("BUTTON_INVENTORY_TOTAL=" + inventory.length);
  console.log("DEAD_BUTTONS=" + JSON.stringify(dead, null, 0));
});
