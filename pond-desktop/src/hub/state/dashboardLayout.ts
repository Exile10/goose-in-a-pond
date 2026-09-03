// ────────────────────────────────────────────────────────────
// What is on Home, and in what order — decided by the household.
//
// Home was pared back on purpose: the room pills, camera strip, routines row,
// to-do widget and category dock were removed because together they made a
// screen you read rather than glanced at. Both `hub/views/Home.tsx` and
// `sections/Dashboard.tsx` say so in their headers.
//
// This does not put them back. The DEFAULT is exactly the screen that decision
// produced — devices, weather, music, and the one suggestion that asks. What
// changes is that a household with four cameras and no music can say so,
// instead of every household getting the same compromise. The question moves
// from "what belongs on Home" to "what belongs on YOUR Home", which is a
// question only the household can answer and a cheaper one to get right.
//
// Persisted to localStorage rather than the settings table on purpose. It is a
// per-panel preference — the kitchen screen and the study screen are looking at
// the same pond and reasonably want different Homes — and `giap-section` next
// to it already works this way.
// ────────────────────────────────────────────────────────────

import { useSyncExternalStore } from "react";

/** Every card Home can show. Ordered as the default layout lists them. */
export type CardId =
  | "suggestion"
  | "devices"
  | "weather"
  | "nowPlaying"
  | "scenes"
  | "cameras"
  | "routines"
  | "todos";

export interface CardSpec {
  id: CardId;
  /** What the household calls it in the edit sheet. */
  title: string;
  /** One line, shown while editing, saying what the card is for. */
  hint: string;
  /**
   * Columns the card wants on a wide grid. Devices earn two because a row of
   * one tile is not a glance; everything else says its piece in one.
   */
  span: 1 | 2;
}

/**
 * The catalogue, in the order the edit sheet lists them.
 *
 * Every entry is backed by a real slice of `HomeData` — nothing here is a
 * placeholder for data the pond does not have, which is the same rule the
 * interface follows (DESIGN.md §3, "never invent meaning the data lacks").
 */
export const CARDS: readonly CardSpec[] = [
  { id: "suggestion", title: "Suggestion", hint: "The one thing asking for you", span: 2 },
  { id: "devices", title: "Devices", hint: "Lights, locks, plugs and thermostats", span: 2 },
  { id: "weather", title: "Weather", hint: "Now, and the days ahead", span: 1 },
  { id: "nowPlaying", title: "Music", hint: "What is playing, and the controls", span: 1 },
  { id: "scenes", title: "Scenes", hint: "Whole-house settings you can run", span: 1 },
  { id: "cameras", title: "Cameras", hint: "The latest frame from each", span: 1 },
  { id: "routines", title: "Routines", hint: "What runs on its own, and when", span: 1 },
  { id: "todos", title: "To-do", hint: "What you asked to be reminded of", span: 1 },
] as const;

const CARD_IDS = new Set<string>(CARDS.map((c) => c.id));

export interface DashboardLayout {
  /** Visible cards, in display order. */
  order: CardId[];
  /** Everything the household has switched off. Kept so the edit sheet can offer them back. */
  hidden: CardId[];
}

/**
 * Today's Home, exactly.
 *
 * Changing this changes what a household sees on first run and after a reset,
 * so it is the one place the pared-back decision still lives. Adding a card
 * here is a product decision; adding one in the edit sheet is theirs.
 */
export const DEFAULT_LAYOUT: DashboardLayout = {
  order: ["suggestion", "devices", "weather", "nowPlaying"],
  hidden: ["scenes", "cameras", "routines", "todos"],
};

const KEY = "giap-dashboard-layout";

let current: DashboardLayout = DEFAULT_LAYOUT;
let loaded = false;
const subs = new Set<() => void>();

function emit(): void {
  for (const s of subs) s();
}

/**
 * Read the stored layout, repairing anything that no longer makes sense.
 *
 * A stored layout outlives the release that wrote it: a card can be removed
 * from the catalogue, or added to it, between one launch and the next. Rather
 * than versioning the payload, every read is reconciled against `CARDS` —
 * unknown ids are dropped and cards the household has never seen are added to
 * `hidden`, so a new card appears in the edit sheet as something they may turn
 * on rather than appearing on their Home unannounced.
 */
function read(): DashboardLayout {
  let stored: unknown;
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_LAYOUT;
    stored = JSON.parse(raw);
  } catch {
    // Unreadable or unavailable storage (a private window, a wiped panel) is
    // not an error worth surfacing — it means "no preference expressed yet".
    return DEFAULT_LAYOUT;
  }

  if (typeof stored !== "object" || stored === null) return DEFAULT_LAYOUT;
  const raw = stored as Partial<Record<keyof DashboardLayout, unknown>>;

  const keep = (v: unknown): CardId[] =>
    Array.isArray(v) ? (v.filter((x) => typeof x === "string" && CARD_IDS.has(x)) as CardId[]) : [];

  const order = dedupe(keep(raw.order));
  const hidden = dedupe(keep(raw.hidden)).filter((id) => !order.includes(id));

  // Cards the stored layout never mentioned are new since it was written.
  const seen = new Set<CardId>([...order, ...hidden]);
  const unseen = CARDS.map((c) => c.id).filter((id) => !seen.has(id));

  // An empty Home is a broken Home, not a preference. Someone who hides
  // everything gets the default back rather than a blank panel with no way
  // into the edit sheet except memory.
  if (order.length === 0) return DEFAULT_LAYOUT;

  return { order, hidden: [...hidden, ...unseen] };
}

function dedupe(ids: CardId[]): CardId[] {
  return [...new Set(ids)];
}

function write(next: DashboardLayout): void {
  current = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // The arrangement still applies for this session; it just will not survive
    // a reload. Losing the preference is better than losing the interaction.
  }
  emit();
}

function ensureLoaded(): DashboardLayout {
  if (!loaded) {
    current = read();
    loaded = true;
  }
  return current;
}

function subscribe(fn: () => void): () => void {
  ensureLoaded();
  subs.add(fn);
  return () => subs.delete(fn);
}

function snapshot(): DashboardLayout {
  return ensureLoaded();
}

export function useDashboardLayout(): DashboardLayout {
  return useSyncExternalStore(subscribe, snapshot, () => DEFAULT_LAYOUT);
}

export function getDashboardLayout(): DashboardLayout {
  return ensureLoaded();
}

/** Put a hidden card on Home, at the end where it can be seen to have arrived. */
export function showCard(id: CardId): void {
  const l = ensureLoaded();
  if (l.order.includes(id)) return;
  write({ order: [...l.order, id], hidden: l.hidden.filter((h) => h !== id) });
}

/** Take a card off Home. It goes back to the sheet rather than being forgotten. */
export function hideCard(id: CardId): void {
  const l = ensureLoaded();
  if (!l.order.includes(id)) return;
  const order = l.order.filter((o) => o !== id);
  // Refuse to empty the screen. The last card stays, because a Home with
  // nothing on it also has no way back to the sheet that would fix it.
  if (order.length === 0) return;
  write({ order, hidden: [...l.hidden, id] });
}

/**
 * Move a card one place, in `delta` direction.
 *
 * Explicit moves rather than drag alone: a drag is unusable from a keyboard and
 * awkward with a thumb on a 480px-tall panel, and DESIGN.md §6 makes keyboard
 * operability a floor rather than an enhancement. Drag can be added over this
 * later; it cannot replace it.
 */
export function moveCard(id: CardId, delta: -1 | 1): void {
  const l = ensureLoaded();
  const from = l.order.indexOf(id);
  if (from < 0) return;
  const to = from + delta;
  if (to < 0 || to >= l.order.length) return;
  const order = [...l.order];
  [order[from], order[to]] = [order[to], order[from]];
  write({ order, hidden: l.hidden });
}

/** Back to the Home the release ships with. */
export function resetLayout(): void {
  write(DEFAULT_LAYOUT);
}

/** Testing seam: forget what was read so the next call re-reads storage. */
export function __resetLayoutCache(): void {
  loaded = false;
  current = DEFAULT_LAYOUT;
}
