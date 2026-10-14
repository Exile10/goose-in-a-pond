import type { SessionSummary } from "../api/types";

// What each conversation card in ChatHistory shows, kept out of the component so it can be tested.

/** How much conversation a card has to show, which is how tall it gets. */
export type CardWeight = "tile" | "card" | "column";

/** Deliberately three coarse buckets: a continuous height mapping reads as a rendering bug. */
export function cardWeight(session: SessionSummary): CardWeight {
  const messages = session.message_count ?? 0;
  const preview = session.preview?.length ?? 0;
  if (messages >= 20 || preview >= 160) return "column";
  if (messages >= 6 || preview >= 60) return "card";
  return "tile";
}

/** Shortest unambiguous form: time today, yesterday, weekday within a week, then a date. */
export function relativeWhen(iso: string, now: Date = new Date()): string {
  const then = new Date(iso);
  if (Number.isNaN(then.getTime())) return "";

  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDay(now) - startOfDay(then)) / 86_400_000);

  if (days <= 0) {
    return then.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  }
  if (days === 1) return "Yesterday";
  if (days < 7) return then.toLocaleDateString(undefined, { weekday: "long" });
  return then.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** "12 messages" — plural handled, and absent counts simply say nothing. */
export function messageCountLabel(count: number | undefined): string {
  if (!count) return "";
  return count === 1 ? "1 message" : `${count} messages`;
}

/** Case-insensitive match across the parts of a conversation a person would recall. */
export function matchesQuery(session: SessionSummary, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    (session.title ?? "").toLowerCase().includes(q) ||
    (session.preview ?? "").toLowerCase().includes(q)
  );
}
