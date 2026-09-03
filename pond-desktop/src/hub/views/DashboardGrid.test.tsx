// The screen renders, at all of it.
//
// This file exists because it should have existed sooner. `DashboardGrid` was
// covered only by tests of the store beneath it, so a reference to `home` from
// inside a child component — where it was never in scope — typechecked, passed
// 603 unit tests, and put "home is not defined" on the panel. A render test is
// the only thing that catches that class of mistake, and every card has to be
// rendered for it to count.

import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, vi } from "vitest";
import { DashboardGrid } from "./DashboardGrid";
import { __resetLayoutCache, CARDS } from "../state/dashboardLayout";

function renderGrid() {
  return render(
    <DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />,
  );
}

beforeEach(() => {
  // This repo does not configure Testing Library's automatic cleanup, so
  // renders otherwise accumulate in the document and a second `getByRole`
  // finds the first test's markup as well as this one's.
  cleanup();
  localStorage.clear();
  __resetLayoutCache();
});

describe("the default screen", () => {
  it("renders without throwing", () => {
    expect(() => renderGrid()).not.toThrow();
  });

  it("shows the greeting and the device section", () => {
    renderGrid();
    expect(screen.getByRole("heading", { level: 1 })).toBeTruthy();
    // "Devices" is also a row in the arrange sheet and could be a room name, so
    // the query names the heading rather than the string.
    expect(screen.getByRole("heading", { name: "Devices", level: 2 })).toBeTruthy();
  });

  /**
   * The line that replaced "Nothing needs you right now". It is a sentence
   * about THIS house, so the only stable assertion is that it is a sentence
   * and that the old wallpaper is gone.
   */
  it("says something about this house rather than nothing", () => {
    const { container } = renderGrid();
    expect(screen.queryByText("Nothing needs you right now.")).toBeNull();
    const line = container.querySelector(".dash__line");
    expect(line?.textContent?.trim().endsWith(".")).toBe(true);
  });
});

/**
 * Every card, mounted.
 *
 * The bug this file was written for lived in ONE branch of a switch. Rendering
 * the default layout would not have found it; rendering all of them does.
 */
describe("every card in the catalogue", () => {
  it("mounts without throwing", () => {
    localStorage.setItem(
      "giap-dashboard-layout",
      JSON.stringify({ order: CARDS.map((c) => c.id), hidden: [] }),
    );
    __resetLayoutCache();
    expect(() => renderGrid()).not.toThrow();
  });
});

describe("the arrange button's name", () => {
  /**
   * `InkButton` does not forward `aria-label` — it renders
   * `<button class="ink-btn"><span>Arrange</span></button>` and drops the
   * attribute. So the visible text IS the accessible name, and the small-panel
   * rule has to CLIP it rather than `display: none` it, or every panel under
   * 900px gets an unlabelled icon button.
   *
   * Asserted here because the CSS that would break it lives in a media query
   * jsdom never evaluates: the guard has to be on the name existing at all.
   */
  it("comes from text, since the aria-label is dropped", () => {
    const { container } = renderGrid();
    const btn = screen.getByRole("button", { name: /Arrange/ });
    expect(btn.getAttribute("aria-label")).toBeNull();
    // The name must come from a real text node that CSS can clip but not remove.
    expect(container.querySelector(".dash__btn-label")?.textContent).toBe("Arrange");
  });
});

describe("search", () => {
  it("narrows to what was typed and says so", () => {
    const { container } = renderGrid();
    const input = container.querySelector('input[type="search"]') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "kitchen" } });
    expect(screen.getByText(/Matching "kitchen"/i)).toBeTruthy();
  });

  /** A search is a question about the whole house, so the room filter steps aside. */
  it("puts the room filter away while searching", () => {
    const { container } = renderGrid();
    expect(container.querySelector(".ink-segmented")).toBeTruthy();
    const input = container.querySelector('input[type="search"]') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "lamp" } });
    expect(container.querySelector(".ink-segmented")).toBeNull();
  });

  it("says so plainly when nothing matches", () => {
    const { container } = renderGrid();
    const input = container.querySelector('input[type="search"]') as HTMLInputElement;
    fireEvent.change(input, { target: { value: "zzzznope" } });
    expect(screen.getByText(/Nothing here matches that/i)).toBeTruthy();
  });
});

describe("arranging", () => {
  it("opens the sheet and lists what is on Home", () => {
    renderGrid();
    // The button and the sheet it opens share a name, which is correct for a
    // screen reader and ambiguous for a query — so ask for the button.
    fireEvent.click(screen.getByRole("button", { name: /Arrange/ }));
    expect(screen.getByRole("heading", { name: "On Home" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /Move .* up/ }).length).toBeGreaterThan(0);
  });

  /** The first card cannot move up, the last cannot move down. */
  it("disables the moves that would fall off an end", () => {
    renderGrid();
    fireEvent.click(screen.getByRole("button", { name: /Arrange/ }));
    const up = screen.getAllByRole("button", { name: /Move .* up/ })[0];
    expect((up as HTMLButtonElement).disabled).toBe(true);
  });
});


/**
 * Form carrying data (DESIGN.md §3).
 *
 * Both of these are a single ternary, which is exactly why they are worth a
 * test: a ternary on a boolean nobody asserts is a claim, and the browser will
 * not show you either case unless the house happens to be in it.
 */
describe("cards that grow when they have something to say", () => {
  const base = {
    user: "Jerry",
    weather: {
      temp: 64, cond: "Partly cloudy", icon: "cloudSun", hi: 68, lo: 54,
      hum: 62, wind: 12, sunrise: "06:30", sunset: "19:10", forecast: [],
    },
    rooms: [], cameras: [], categories: [], scenes: [], todos: [],
    gooseSuggestions: [],
  };

  function mountWith(devices: unknown[], nowPlaying: Record<string, unknown>) {
    vi.resetModules();
    vi.doMock("../state/hubDataStore", async () => {
      const real = await vi.importActual<Record<string, unknown>>("../state/hubDataStore");
      return { ...real, useHomeData: () => ({ ...base, devices, nowPlaying }), useRoutines: () => [] };
    });
    return import("./DashboardGrid");
  }

  const silent = { track: "", artist: "", elapsed: 0, hue: 0, connected: false, playing: false };
  const lamp = { id: "l1", name: "Lamp", kind: "light", on: false, room: "Hall" };

  it("gives music the width only while something is playing", async () => {
    const { DashboardGrid: Grid } = await mountWith(
      [lamp, lamp, lamp, lamp, lamp, lamp],
      { ...silent, connected: true, playing: true, track: "Weightless" },
    );
    const { container } = render(<Grid sessionId="s" onNavigate={() => {}} onTalk={() => {}} />);
    expect(container.querySelector('[data-playing] ')).toBeTruthy();
    expect(container.querySelector('[data-playing]')?.className).toContain("dash__cell--wide");
    cleanup();
  });

  /**
   * A house with two lamps has the same problem as a house with none: a devices
   * card holding one row of tiles, and empty columns beside it.
   */
  it("lets the weather spread when there is little else", async () => {
    const { DashboardGrid: Grid } = await mountWith([lamp], silent);
    const { container } = render(<Grid sessionId="s" onNavigate={() => {}} onTalk={() => {}} />);
    const wide = container.querySelectorAll(".dash__cell--wide");
    // Suggestion, devices AND weather — three, where a furnished house has two.
    expect(wide.length).toBeGreaterThanOrEqual(3);
    cleanup();
  });
});
