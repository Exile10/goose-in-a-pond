import { useEffect, useState } from "react";

// A GIAP dashboard is normally left open indefinitely (wall panel, kiosk, a
// desktop window nobody closes), so anything derived from `new Date()` at
// render time silently rots — the greeting stays on "Good afternoon" all
// evening and the date survives midnight. Components that show the current
// time derive it from this hook instead of reading the clock once.

/**
 * Current time, re-read every `periodMs` and whenever the document becomes
 * visible again. Ticks are aligned to the next `periodMs` boundary so a
 * minute-resolution clock flips at the top of the minute rather than drifting.
 */
export function useNow(periodMs = 60_000): Date {
  const [now, setNow] = useState(() => new Date());

  useEffect(() => {
    let intervalId: ReturnType<typeof setInterval> | undefined;

    const tick = () => setNow(new Date());

    const msToBoundary = periodMs - (Date.now() % periodMs);
    const timeoutId = setTimeout(() => {
      tick();
      intervalId = setInterval(tick, periodMs);
    }, msToBoundary);

    // A machine that slept (lid closed, kiosk display off) freezes its timers,
    // so the first thing on screen after a wake would otherwise be the stale
    // pre-sleep time until the next tick.
    const onVisibility = () => {
      if (document.visibilityState === "visible") tick();
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      clearTimeout(timeoutId);
      if (intervalId !== undefined) clearInterval(intervalId);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [periodMs]);

  return now;
}

/** Greeting for the hour of day, matching the phases the hub UI uses. */
export function greetingForHour(h: number): string {
  if (h < 5) return "Good night";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

/** The hub's long date form, e.g. "Monday, June 1", in the browser's locale. */
export function formatHubDate(now: Date): string {
  return now.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });
}
