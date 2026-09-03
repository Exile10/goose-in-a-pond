// ────────────────────────────────────────────────────────────
// Suggestion — the one thing on Home that asks.
//
// Everything else on this screen reports: the weather is 24°, four lights are
// on, this track is playing. This is the only element that wants an answer, so
// it is the only one that gets weight, and it is answerable where it stands —
// approving must not cost a navigation, or the answer is "later" every time.
//
// Backed by the proposal system that has been generating these all along with
// nowhere to show them (`GET /api/v1/proposals`).
// ────────────────────────────────────────────────────────────

import { useCallback, useEffect, useState } from "react";
import { api } from "../../api/PondApiClient";
import type { Proposal, ProposalDecision } from "../../api/types";

interface Props {
  /** Who is asking. Without one the server cannot tell who a suggestion is
      addressed to, so there is nothing safe to show. */
  sessionId: string | null;
  /**
   * What to show when there is nothing to suggest. Falls back to a plain
   * reassurance when a caller has nothing more specific to say.
   */
  quiet?: React.ReactNode;
}

export function Suggestion({ sessionId, quiet }: Props) {
  const [proposals, setProposals] = useState<Proposal[]>([]);
  const [expanded, setExpanded] = useState(false);
  const [deciding, setDeciding] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!sessionId) {
      setProposals([]);
      return;
    }
    try {
      const list = await api.listProposals(sessionId);
      // Personal context produces suggestions from several sources at once —
      // conversations, calendar, sensors — so more than one can be waiting.
      // They are all kept, but only the first is open: a stack of expanded
      // cards is a to-do list, and a to-do list is the clutter this screen
      // exists without.
      setProposals(list.proposals);
      setError(null);
    } catch {
      // A suggestion that cannot be fetched is not an error worth a banner:
      // there is simply nothing to show. Errors here would be the loudest thing
      // on a screen whose whole job is quiet.
      setProposals([]);
    }
  }, [sessionId]);

  useEffect(() => {
    void load();
  }, [load]);

  async function decide(answered: Proposal, decision: ProposalDecision) {
    if (!sessionId || deciding) return;
    setDeciding(true);
    // Optimistic: the card goes the moment it is answered, and the next one
    // takes its place. A spinner on a decision this small reads as doubt about
    // whether the tap registered.
    const before = proposals;
    setProposals((all) => all.filter((p) => p.id !== answered.id));
    try {
      await api.decideProposal(answered.id, sessionId, decision);
      void load();
    } catch {
      // Put it back rather than swallow it — a suggestion that silently failed
      // to send would look answered and never happen.
      setProposals(before);
      setError("That didn't send. Try again.");
    } finally {
      setDeciding(false);
    }
  }

  if (proposals.length === 0) {
    return (
      <section className="sugg sugg--quiet" aria-live="polite">
        {/* Not an invitation to act: nothing needing you is the good outcome
            here, so it reads as reassurance rather than an empty inbox.

            A caller can supply something better. "Nothing needs you right now"
            is true, and true of any house on any day, which makes it wallpaper
            on a screen that is glanced at. Home passes a line about THIS house
            — which lights are on, whether the doors are locked — and the
            default stays for callers with no such context to offer. */}
        {quiet ?? <p className="sugg__quiet-text">Nothing needs you right now.</p>}
      </section>
    );
  }

  const [first, ...rest] = proposals;
  // Only the first is open. The rest are one line each, so a household can see
  // there is more waiting without the screen turning into a queue to work
  // through — and answering the top one promotes the next.
  const shown = expanded ? rest : [];

  return (
    <div className="sugg-stack" aria-live="polite">
      <section className="sugg">
        <p className="sugg__summary">{first.summary}</p>
        {first.rationale && <p className="sugg__why">{first.rationale}</p>}
        {error && <p className="sugg__error">{error}</p>}
        <div className="sugg__actions">
          <button
            type="button"
            className="sugg__go"
            disabled={deciding}
            onClick={() => void decide(first, "approve")}
          >
            Approve
          </button>
          <button
            type="button"
            className="sugg__no"
            disabled={deciding}
            onClick={() => void decide(first, "reject")}
          >
            Not now
          </button>
        </div>
      </section>

      {rest.length > 0 && (
        <button
          type="button"
          className="sugg__more"
          aria-expanded={expanded}
          onClick={() => setExpanded((v) => !v)}
        >
          {expanded
            ? "Hide the rest"
            : `${rest.length} more suggestion${rest.length === 1 ? "" : "s"}`}
        </button>
      )}

      {shown.map((p) => (
        <section key={p.id} className="sugg sugg--stacked">
          <p className="sugg__summary sugg__summary--small">{p.summary}</p>
          <div className="sugg__actions">
            <button
              type="button"
              className="sugg__go"
              disabled={deciding}
              onClick={() => void decide(p, "approve")}
            >
              Approve
            </button>
            <button
              type="button"
              className="sugg__no"
              disabled={deciding}
              onClick={() => void decide(p, "reject")}
            >
              Not now
            </button>
          </div>
        </section>
      ))}
    </div>
  );
}
