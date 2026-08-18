// ─── Lineage: how what the pond kept got that way ───────────────────────────
//
// The advanced view, and the shape of it is an argument.
//
// The obvious thing to draw here is a force-directed graph, and it would be
// wrong. This pond holds 276 memory fragments of which 5 are live: 211 of them
// were superseded, folded into survivors by consolidation and correction. That
// is not a web, it is a CONVERGENCE, and it runs in one direction through time.
// A physics blob would scatter that into an even mist and hide the only thing
// the picture has to say.
//
// So it is drawn as generations flowing left to right into the few rows that
// survived. Depth is real — how many supersessions deep a chain runs — and the
// widths are counts, not decoration.
//
// # What is NOT drawn, and why
//
// `memory_edges` (migration 0017) has a `caused` / `referenced` / `superseded`
// relation and would be the richer graph. It has zero rows, and grep finds no
// production writer for it — the port's edge methods are default bodies that
// `sqlite_memory` never implements. Drawing an empty DAG as though it were a
// sparse one would be inventing a picture. It is reported as absent instead.

import { useMemo, useState } from "react";
import { RefreshCw } from "lucide-react";
import { api } from "../../api/PondApiClient";
import type { ContextIndexHealth, MemoryFragment } from "../../api/types";

interface Props {
  memories: MemoryFragment[];
  health: ContextIndexHealth | null;
  /** Re-read what the pond holds after a rebuild clears the index. */
  onRebuilt?: () => void;
}

interface Chain {
  /** The surviving memory this chain ends at. */
  survivor: MemoryFragment;
  /** Every superseded fragment that folded into it, nearest first. */
  ancestors: MemoryFragment[];
}

/** Follow `superseded_by` forward until it stops, so each chain ends at a row
 *  that nothing replaced. Cycles cannot happen by design, but a corrupt row
 *  should not hang the screen, so the walk is bounded. */
function buildChains(memories: MemoryFragment[]): {
  chains: Chain[];
  orphaned: number;
  deepest: number;
} {
  const byId = new Map(memories.map((m) => [m.id, m]));
  const chains = new Map<string, Chain>();
  let orphaned = 0;
  let deepest = 0;

  for (const memory of memories) {
    if (!memory.superseded_by) continue;
    let cursor: MemoryFragment | undefined = memory;
    const walked: MemoryFragment[] = [];
    let hops = 0;
    while (cursor?.superseded_by && hops < 64) {
      walked.push(cursor);
      cursor = byId.get(cursor.superseded_by);
      hops += 1;
    }
    if (!cursor) {
      // Points at a row this pond no longer has. Counted rather than dropped:
      // a lineage with missing ends is a real state, and silently omitting it
      // would make the totals disagree with the memory count above.
      orphaned += 1;
      continue;
    }
    deepest = Math.max(deepest, walked.length);
    const existing = chains.get(cursor.id);
    if (existing) existing.ancestors.push(memory);
    else chains.set(cursor.id, { survivor: cursor, ancestors: [memory] });
  }

  return {
    chains: [...chains.values()].sort((a, b) => b.ancestors.length - a.ancestors.length),
    orphaned,
    deepest,
  };
}

export function Lineage({ memories, health, onRebuilt }: Props) {
  const [rebuilding, setRebuilding] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  /** Clear every vector so the sweep reads everything again. */
  async function rebuild() {
    setRebuilding(true);
    setNote(null);
    try {
      const r = await api.rebuildContextIndex();
      // The route only CLEARS -- refilling is the idle sweep's job, and it is
      // deferred, so promising "reindexed" would be a promise about something
      // that has not happened yet.
      setNote(
        r.cleared === 0
          ? "Nothing was indexed, so there was nothing to clear."
          : `Cleared ${r.cleared} entries. The pond reads them again in the background; this page will fill back in.`,
      );
      onRebuilt?.();
    } catch (e) {
      setNote(e instanceof Error ? e.message : "Could not start a rebuild.");
    } finally {
      setRebuilding(false);
    }
  }

  const { chains, orphaned, deepest } = useMemo(() => buildChains(memories), [memories]);

  const live = memories.filter((m) => !m.lifecycle || m.lifecycle === "active");
  const superseded = memories.filter((m) => m.superseded_by).length;
  const archived = memories.filter((m) => m.lifecycle === "archived").length;
  const widest = chains[0]?.ancestors.length ?? 1;

  return (
    <div className="lin">
      <section className="lin__intro">
        <h2 className="lin__h">How this got here</h2>
        <p>
          The pond does not keep everything it is told. It merges what repeats, corrects what
          changed, and lets the rest fade. What survives is on the right; what folded into it
          is on the left.
        </p>
      </section>

      <dl className="lin__stats">
        <div>
          <dt>Held now</dt>
          <dd>{live.length}</dd>
        </div>
        <div>
          <dt>Folded in</dt>
          <dd>{superseded}</dd>
        </div>
        <div>
          <dt>Faded</dt>
          <dd>{archived}</dd>
        </div>
        <div>
          <dt>Deepest chain</dt>
          <dd>
            {deepest}
            <span className="lin__unit">{deepest === 1 ? " step" : " steps"}</span>
          </dd>
        </div>
      </dl>

      {chains.length === 0 ? (
        <p className="lin__empty">
          Nothing has been folded together yet. This fills in as the pond consolidates what it
          has heard more than once.
        </p>
      ) : (
        <ol className="lin__chains">
          {chains.map((chain) => (
            <li key={chain.survivor.id} className="lin__chain">
              <div
                className="lin__flow"
                style={{ ["--fan" as string]: String(chain.ancestors.length / widest) }}
                aria-hidden="true"
              >
                <span className="lin__fanCount">{chain.ancestors.length}</span>
              </div>
              <div className="lin__survivor">
                <p className="lin__survivorText">{chain.survivor.content}</p>
                <p className="lin__survivorMeta">
                  {chain.ancestors.length} earlier version
                  {chain.ancestors.length === 1 ? "" : "s"} folded into this
                  {chain.survivor.tier ? ` · kept as ${chain.survivor.tier}` : ""}
                </p>
              </div>
            </li>
          ))}
        </ol>
      )}

      {orphaned > 0 && (
        <p className="lin__note">
          {orphaned} chain{orphaned === 1 ? "" : "s"} point at a memory this pond no longer
          has. That happens when something was deleted after it had already been folded into.
        </p>
      )}

      <section className="lin__vectors">
        <div className="lin__vectorsHead">
          <h2 className="lin__h">Searchable by meaning</h2>
          <button
            type="button"
            className="lin__rebuild"
            onClick={() => void rebuild()}
            disabled={rebuilding}
          >
            <RefreshCw size={14} className={rebuilding ? "connections-spin" : undefined} />
            <span>{rebuilding ? "Clearing" : "Read everything again"}</span>
          </button>
        </div>
        {note && <p className="lin__note">{note}</p>}
        {!health || !health.indexed ? (
          <p className="lin__empty">
            {health?.reason ??
              "Nothing is indexed for meaning yet, so searches fall back to matching words."}
          </p>
        ) : (
          <>
            <p className="lin__vectorsIntro">
              Read by <strong>{health.model_id}</strong> at {health.dims} dimensions. Anything
              not indexed is still found by word, just less well.
            </p>
            <ul className="lin__corpora">
              {health.corpora.map((corpus) => {
                const pct =
                  corpus.rows === 0
                    ? null
                    : Math.round((corpus.indexed_rows / corpus.rows) * 100);
                // Three different zeroes, and they need opposite answers.
                // "Nothing here" is fine; "everything here is being excluded"
                // is a fault that no amount of indexing repairs, and reporting
                // both as 0% would hide the one worth acting on.
                const note = corpus.structurally_excluded
                  ? `all ${corpus.source_rows} held back`
                  : corpus.rows === 0
                    ? "nothing here yet"
                    : `${corpus.indexed_rows} of ${corpus.rows}`;
                return (
                  <li key={corpus.corpus} className="lin__corpus">
                    <div className="lin__corpusHead">
                      <span className="lin__corpusName">{corpus.corpus}</span>
                      <span
                        className={
                          corpus.structurally_excluded
                            ? "lin__corpusCount lin__corpusCount--held"
                            : "lin__corpusCount"
                        }
                      >
                        {note}
                      </span>
                    </div>
                    <div
                      className="lin__bar"
                      role="img"
                      aria-label={`${corpus.corpus}: ${
                        corpus.structurally_excluded
                          ? "held back, nothing searchable by meaning"
                          : pct === null
                            ? "nothing here yet"
                            : `${pct}% searchable by meaning`
                      }`}
                    >
                      <span className="lin__barFill" style={{ width: `${pct ?? 0}%` }} />
                    </div>
                    {corpus.structurally_excluded && (
                      <p className="lin__held">
                        Everything in this corpus is being held back, so none of it can be
                        found by meaning. Indexing more will not change that.
                      </p>
                    )}
                    {corpus.mismatched > 0 && (
                      <p className="lin__held">
                        {corpus.mismatched} were read by an older model and cannot be
                        searched until they are read again.
                      </p>
                    )}
                  </li>
                );
              })}
            </ul>
          </>
        )}
      </section>

      <p className="lin__note">
        A richer picture — what caused what, what was quoted when — is designed but not
        recorded: the pond has a table for it and nothing writes to it yet, so there is
        nothing honest to draw.
      </p>
    </div>
  );
}
