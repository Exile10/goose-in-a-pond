import { useMemo, useState } from "react";
import { PenSquare, Search, Trash2 } from "lucide-react";
import type { SessionSummary } from "../api/types";
import "../styles/chat-history.css";

/**
 * Every conversation this pond has had, as a wall of cards.
 *
 * # Why the cards are different heights
 *
 * The layout this is modelled on varies card height because some entries carry
 * a photograph. Conversations have no photographs, so height had to be earned
 * from something true about the content rather than invented for texture: a
 * card is as tall as the conversation was long. A three-message "what time is
 * it" is a tile; a forty-message debugging session is a column. The wall ends
 * up a rough histogram of where the household's attention went, which is
 * exactly the thing you are scanning for when you are trying to find a
 * conversation you only half remember.
 *
 * Ordering stays newest-first, left to right. That rules out CSS multi-column,
 * which is the easy way to get a masonry wall and would put the newest half of
 * the history in the left-hand column and the rest in the right — correct
 * masonry, unreadable chronology. Row spans keep the reading order and cost a
 * little raggedness at the bottom edge, which is the honest trade.
 */

/** How much conversation a card has to show, which is how tall it gets. */
export type CardWeight = "tile" | "card" | "column";

/**
 * Bucket a conversation by how much was said in it.
 *
 * Deliberately coarse. Three heights tile predictably and read as deliberate;
 * a continuous mapping from message count to pixels produces a wall of
 * almost-but-not-quite equal cards, which reads as a rendering bug.
 */
export function cardWeight(session: SessionSummary): CardWeight {
  const messages = session.message_count ?? 0;
  const preview = session.preview?.length ?? 0;
  if (messages >= 20 || preview >= 160) return "column";
  if (messages >= 6 || preview >= 60) return "card";
  return "tile";
}

/**
 * When a conversation last moved, in the shortest form that is still unambiguous.
 *
 * Same ladder the reference uses: a time for today, a word for yesterday, a
 * weekday inside the last week, then a date. Anything older than a week is
 * being scanned rather than recalled, and a weekday name stops helping.
 */
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

/** Where on screen a card was when it was opened, in viewport pixels. */
export interface OpenOrigin {
  x: number;
  y: number;
}

interface Props {
  sessions: SessionSummary[];
  loading: boolean;
  /** The conversation to open, and the point the chat should grow out of. */
  onOpen: (id: string, origin: OpenOrigin) => void;
  onNewChat: () => void;
  onDelete: (id: string) => void;
}

export function ChatHistory({ sessions, loading, onOpen, onNewChat, onDelete }: Props) {
  const [query, setQuery] = useState("");
  const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);

  const visible = useMemo(
    () => sessions.filter((s) => matchesQuery(s, query)),
    [sessions, query],
  );

  function open(session: SessionSummary, event: React.MouseEvent<HTMLElement>) {
    const card = event.currentTarget.getBoundingClientRect();
    // Pane-relative, not viewport-relative: the chat that grows out of this
    // point fills the same pane this wall does, and `transform-origin` is
    // measured from the element's own box. Viewport coordinates would put the
    // origin off by the sidebar's width, so every card would appear to open
    // from somewhere to its left.
    const pane = event.currentTarget.closest(".chist")?.getBoundingClientRect();
    onOpen(session.id, {
      x: card.left + card.width / 2 - (pane?.left ?? 0),
      y: card.top + card.height / 2 - (pane?.top ?? 0),
    });
  }

  return (
    <div className="chist">
      <header className="chist__head">
        <div>
          <h1 className="chist__title">Conversations</h1>
          <p className="chist__sub">
            {sessions.length === 0
              ? "Nothing here yet."
              : `${sessions.length} on this device, and nowhere else.`}
          </p>
        </div>
        <div className="chist__actions">
          <label className="chist__search">
            <Search size={15} aria-hidden="true" />
            <input
              type="search"
              className="chist__searchInput"
              aria-label="Search conversations"
              placeholder="Search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <button type="button" className="chist__new" onClick={onNewChat}>
            <PenSquare size={15} aria-hidden="true" />
            <span>New chat</span>
          </button>
        </div>
      </header>

      {loading && (
        <div className="chist__grid" aria-busy="true" aria-label="Loading conversations">
          {(["card", "tile", "column", "tile", "card", "tile"] as const).map((w, i) => (
            <div key={i} className="chist__card chist__card--ghost" data-weight={w} />
          ))}
        </div>
      )}

      {!loading && sessions.length === 0 && (
        <div className="chist__empty">
          <p className="chist__emptyLine">No conversations yet.</p>
          <button type="button" className="chist__new chist__new--lg" onClick={onNewChat}>
            <PenSquare size={16} aria-hidden="true" />
            <span>Start one</span>
          </button>
        </div>
      )}

      {!loading && sessions.length > 0 && visible.length === 0 && (
        <p className="chist__empty chist__emptyLine">
          Nothing matches “{query.trim()}”.
        </p>
      )}

      {!loading && visible.length > 0 && (
        <div className="chist__grid">
          {visible.map((session) => {
            const title = session.title?.trim() || "Untitled";
            const count = messageCountLabel(session.message_count);
            return (
              <article
                key={session.id}
                className="chist__card"
                data-weight={cardWeight(session)}
                onClick={(e) => open(session, e)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    open(session, e as unknown as React.MouseEvent<HTMLElement>);
                  }
                }}
                role="button"
                tabIndex={0}
                aria-label={`Open conversation: ${title}`}
              >
                <span className="chist__when">{relativeWhen(session.updated_at)}</span>
                <h2 className="chist__cardTitle">{title}</h2>
                {session.preview && <p className="chist__preview">{session.preview}</p>}
                {count && <span className="chist__count">{count}</span>}

                {confirmingDelete === session.id ? (
                  <span className="chist__confirm">
                    <button
                      type="button"
                      className="chist__confirmYes"
                      aria-label={`Delete conversation: ${title}`}
                      onClick={(e) => { e.stopPropagation(); onDelete(session.id); setConfirmingDelete(null); }}
                    >
                      Delete
                    </button>
                    <button
                      type="button"
                      className="chist__confirmNo"
                      aria-label="Keep conversation"
                      onClick={(e) => { e.stopPropagation(); setConfirmingDelete(null); }}
                    >
                      Keep
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="chist__del"
                    aria-label={`Delete conversation: ${title}`}
                    onClick={(e) => { e.stopPropagation(); setConfirmingDelete(session.id); }}
                  >
                    <Trash2 size={14} aria-hidden="true" />
                  </button>
                )}
              </article>
            );
          })}
        </div>
      )}
    </div>
  );
}
