// ─── Context: everything the pond knows about you ───────────────────────────
//
// The screen the sidebar's Context entry lands on, replacing the old Memories
// tab. Three views of one subject, because "what the pond knows" is three
// different questions a person actually asks:
//
//   Remembered  what it has kept          (a wall, like Conversations)
//   Sources     where else it may read    (calendar, mail, the pond's own)
//   Lineage     how any of it connects    (the advanced view)
//
// It borrows Conversations' card treatment on purpose. That screen is the one
// people already know how to scan, and a second wall that behaves differently
// would be a second thing to learn for no reason.

import { useCallback, useEffect, useMemo, useState } from "react";
import { Plus, Search, RefreshCw } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ContextIndexHealth, MemoryFragment } from "../api/types";
import { ConnectionsPanel } from "../connections/ConnectionsPanel";
import { useAppState } from "../state/AppContext";
import { Lineage } from "./context/Lineage";
import "../styles/context.css";

type View = "remembered" | "sources" | "lineage";

const VIEWS: Array<{ id: View; label: string; blurb: string }> = [
  { id: "remembered", label: "Remembered", blurb: "What the pond has kept" },
  { id: "sources", label: "Sources", blurb: "Where else it may read" },
  { id: "lineage", label: "Lineage", blurb: "How it all connects" },
];

/** Cards are as tall as their memory is long, the way conversation cards are. */
function weightOf(content: string): "tile" | "card" | "column" {
  if (content.length < 90) return "tile";
  if (content.length < 240) return "card";
  return "column";
}

function relativeWhen(iso?: string): string {
  if (!iso) return "";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const mins = Math.max(0, Math.round((Date.now() - then) / 60000));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return `${Math.round(days / 30)}mo ago`;
}

export function Context() {
  const { sessionId } = useAppState();
  const [view, setView] = useState<View>("remembered");
  const [memories, setMemories] = useState<MemoryFragment[]>([]);
  const [health, setHealth] = useState<ContextIndexHealth | null>(null);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    try {
      // 500 rather than the default 20: this screen is about the whole of what
      // the pond holds, and the lineage view is meaningless over a slice of it.
      const [rows, index] = await Promise.all([
        api.listMemories(500),
        api.getContextIndexHealth().catch(() => null),
      ]);
      // `request` returns undefined cast to T for a 204 or any empty body, so
      // a method typed `Promise<MemoryFragment[]>` can hand back undefined and
      // the type will not warn. Coerced here rather than trusted: the whole
      // screen white-screens on the first `.filter`.
      setMemories(Array.isArray(rows) ? rows : []);
      setHealth(index);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not read what the pond remembers.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const live = useMemo(
    () => memories.filter((m) => !m.lifecycle || m.lifecycle === "active"),
    [memories],
  );

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return live;
    return live.filter((m) => m.content.toLowerCase().includes(q));
  }, [live, query]);

  async function addMemory(event: React.FormEvent) {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    setAdding(true);
    try {
      await api.addMemory(text, undefined, undefined, undefined, undefined);
      setDraft("");
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Could not save that.");
    } finally {
      setAdding(false);
    }
  }

  return (
    <div className="ctx">
      <header className="ctx__head">
        <div>
          <h1 className="ctx__title">Context</h1>
          <p className="ctx__sub">
            {live.length === 0
              ? "Nothing kept yet."
              : `${live.length} thing${live.length === 1 ? "" : "s"} the pond is holding onto` +
                (memories.length > live.length
                  ? `, folded down from ${memories.length}.`
                  : ".")}
          </p>
        </div>
        <button type="button" className="ctx__refresh" onClick={() => void load()}>
          <RefreshCw size={15} />
          <span>Refresh</span>
        </button>
      </header>

      <nav className="ctx__views" aria-label="Context views">
        {VIEWS.map((v) => (
          <button
            key={v.id}
            type="button"
            className="ctx__view"
            aria-pressed={view === v.id}
            // The label and its question are separate spans, which a screen
            // reader would otherwise run together into "Remembered what the
            // pond has kept" as the button's name.
            aria-label={v.label}
            onClick={() => setView(v.id)}
          >
            <span className="ctx__viewLabel">{v.label}</span>
            <span className="ctx__viewBlurb">{v.blurb}</span>
          </button>
        ))}
      </nav>

      {error && <p className="ctx__error">{error}</p>}

      {view === "remembered" && (
        <>
          <div className="ctx__tools">
            <label className="ctx__search">
              <Search size={15} aria-hidden="true" />
              <input
                className="ctx__searchInput"
                type="search"
                value={query}
                placeholder="Search what the pond remembers"
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
          </div>

          <form className="ctx__add" onSubmit={addMemory}>
            <input
              className="ctx__addInput"
              type="text"
              value={draft}
              placeholder="Tell the pond something worth keeping"
              onChange={(e) => setDraft(e.target.value)}
            />
            <button type="submit" className="ctx__addBtn" disabled={adding || !draft.trim()}>
              <Plus size={15} />
              <span>{adding ? "Saving" : "Remember"}</span>
            </button>
          </form>

          {loading ? (
            <div className="ctx__wall" aria-busy="true" aria-label="Loading">
              {["tile", "card", "column", "card", "tile", "card"].map((w, i) => (
                <div key={i} className="ctx__card ctx__card--ghost" data-weight={w} />
              ))}
            </div>
          ) : shown.length === 0 ? (
            <p className="ctx__empty">
              {query
                ? "Nothing here matches that."
                : "Nothing kept yet. Tell it something above, or connect a calendar under Sources."}
            </p>
          ) : (
            <div className="ctx__wall">
              {shown.map((m) => (
                <article key={m.id} className="ctx__card" data-weight={weightOf(m.content)}>
                  <div className="ctx__cardTop">
                    {m.tier && <span className="ctx__tier" data-tier={m.tier}>{m.tier}</span>}
                    <span className="ctx__when">{relativeWhen(m.created_at)}</span>
                  </div>
                  <p className="ctx__cardBody">{m.content}</p>
                </article>
              ))}
            </div>
          )}
        </>
      )}

      {view === "sources" && <ConnectionsPanel sessionId={sessionId} />}

      {view === "lineage" && <Lineage memories={memories} health={health} />}
    </div>
  );
}
