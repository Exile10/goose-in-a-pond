import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act, cleanup } from "@testing-library/react";
import { useNow, greetingForHour, formatHubDate } from "./useNow";

const MINUTE = 60_000;

describe("useNow", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    // 14:29:20 — deliberately off a minute boundary.
    vi.setSystemTime(new Date(2026, 7, 3, 14, 29, 20));
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("returns the current time on first render", () => {
    const { result } = renderHook(() => useNow(MINUTE));
    expect(result.current.getMinutes()).toBe(29);
  });

  it("ticks on the period boundary, not one period after mount", () => {
    const { result } = renderHook(() => useNow(MINUTE));

    // 39s short of the boundary: still the mount-time value.
    act(() => {
      vi.advanceTimersByTime(39_000);
    });
    expect(result.current.getMinutes()).toBe(29);

    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(result.current.getMinutes()).toBe(30);
    expect(result.current.getSeconds()).toBe(0);
  });

  it("keeps ticking every period after the first boundary", () => {
    const { result } = renderHook(() => useNow(MINUTE));

    act(() => {
      vi.advanceTimersByTime(40_000 + 2 * MINUTE);
    });
    expect(result.current.getMinutes()).toBe(32);
  });

  it("crosses midnight so a day-old date does not survive", () => {
    vi.setSystemTime(new Date(2026, 7, 3, 23, 59, 30));
    const { result } = renderHook(() => useNow(MINUTE));
    expect(formatHubDate(result.current)).toContain("Monday");

    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    expect(formatHubDate(result.current)).toContain("Tuesday");
  });

  it("catches up when the document becomes visible again", () => {
    const { result } = renderHook(() => useNow(MINUTE));

    // Simulate a sleeping machine: the clock moved but no timer fired.
    vi.setSystemTime(new Date(2026, 7, 3, 17, 5, 0));
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current.getHours()).toBe(17);
  });

  it("clears its timers on unmount", () => {
    const { unmount } = renderHook(() => useNow(MINUTE));
    act(() => {
      vi.advanceTimersByTime(40_000);
    });
    expect(vi.getTimerCount()).toBeGreaterThan(0);

    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });
});

describe("greetingForHour", () => {
  it("maps each phase of the day", () => {
    expect(greetingForHour(0)).toBe("Good night");
    expect(greetingForHour(4)).toBe("Good night");
    expect(greetingForHour(5)).toBe("Good morning");
    expect(greetingForHour(11)).toBe("Good morning");
    expect(greetingForHour(12)).toBe("Good afternoon");
    expect(greetingForHour(17)).toBe("Good afternoon");
    expect(greetingForHour(18)).toBe("Good evening");
    expect(greetingForHour(23)).toBe("Good evening");
  });
});

describe("formatHubDate", () => {
  it("renders weekday, month and day", () => {
    const s = formatHubDate(new Date(2026, 7, 3));
    expect(s).toContain("Monday");
    expect(s).toContain("August");
    expect(s).toContain("3");
  });
});
