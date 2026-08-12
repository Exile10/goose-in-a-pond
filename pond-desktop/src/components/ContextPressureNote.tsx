import { useState } from "react";
import { Gauge } from "lucide-react";
import { api } from "../api/PondApiClient";
import { TURNS_REMAINING_UNKNOWN } from "../api/types";
import type { CompactionReport, ContextWarning } from "../api/types";

/**
 * PAI-4 P7b. The client half of the context-pressure loop.
 *
 * Until this component the `context_warning` SSE frame had zero consumers in
 * the shipped app: the server had been pushing it since before PAI-4, every
 * chat surface parsed it, and every chat surface then dropped it on the floor
 * because no branch matched. This renders it, and gives the person the third
 * compaction trigger `POST /sessions/{id}/compact` exists to serve.
 *
 * Three decisions worth keeping:
 *
 * 1. **It is not folded into `TurnStatsFooter`.** That footer is gated on the
 *    `show_turn_stats` setting, which defaults to `false` in Rust — hosting the
 *    control there would ship it invisible on every default install.
 * 2. **A refusal is not an error.** The endpoint answers 200 with a
 *    `status`/`reason` pair for everything short of a server fault, and being
 *    refused is the *common* path: it deliberately does not bypass the pressure
 *    axis's rate limiter, so "cooling_down" and "not_under_pressure" are what a
 *    user actually hits. They render as a neutral `aria-live="polite"` note,
 *    never `role="alert"`.
 * 3. **`turns_remaining` is clamped.** The SSE frame serialises the monitor's
 *    "growth unknown" sentinel as the raw `u32::MAX`, and printing 4294967295
 *    turns to a user is worse than saying nothing.
 */

interface ContextPressureNoteProps {
  warning: ContextWarning;
  /**
   * Read at CLICK time by the caller, not at frame time: the `context_warning`
   * frame carries no session id, and on a session's first turn the id only
   * arrives with `done`, which is emitted after this frame.
   */
  sessionId: string | null;
  onCompacted?: (report: CompactionReport) => void;
}

/**
 * The server's `reason` vocabulary, in plain language. Kept verbatim on the
 * left so a new reason added in `compact_session` shows up here as a miss
 * rather than as a wrong sentence.
 */
const REASON_TEXT: Record<string, string> = {
  cooling_down: "A compaction ran recently — try again in a few turns.",
  not_under_pressure: "There is still room in this window.",
  already_running: "Already compacting.",
  nothing_to_summarise: "Nothing new to summarise.",
  preempted_by_turn: "Stopped because you started typing.",
  no_summariser: "No model is loaded to write the summary.",
  monitor_disabled: "Context monitoring is switched off in Settings.",
  compaction_disabled: "Compaction is switched off in Settings.",
  failed: "The summariser did not finish. Nothing was changed.",
};

function reportText(report: CompactionReport): string {
  if (report.status === "compacted") {
    return "Compacted — the window has room again.";
  }
  if (report.reason && REASON_TEXT[report.reason]) {
    return REASON_TEXT[report.reason];
  }
  return "Nothing was compacted.";
}

function pressureLine(warning: ContextWarning): string {
  // The server only writes `warning` above 60% utilisation, but `should_compact`
  // also fires through the turns-remaining limb below that threshold — so this
  // frame genuinely arrives with `warning: null` and the fallback is a real
  // path, not a defensive one.
  const base =
    warning.warning ??
    `This conversation is using ${Math.round(warning.utilization_pct)}% of the context window.`;

  if (warning.turns_remaining === TURNS_REMAINING_UNKNOWN) {
    return base;
  }
  const turns = warning.turns_remaining;
  return `${base} About ${turns} ${turns === 1 ? "turn" : "turns"} left at this rate.`;
}

export function ContextPressureNote({
  warning,
  sessionId,
  onCompacted,
}: ContextPressureNoteProps) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  async function compactNow() {
    if (!sessionId || busy) return;
    setBusy(true);
    setNote(null);
    try {
      const report = await api.compactSession(sessionId);
      setNote(reportText(report));
      onCompacted?.(report);
    } catch {
      // A genuine fault (the server is gone, a 500). Still stated neutrally —
      // nothing the user typed was lost and nothing was changed.
      setNote("Could not reach the pond just now. Nothing was changed.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="ctx-pressure">
      <span className="ctx-pressure__note">
        <Gauge size={11} aria-hidden /> {pressureLine(warning)}
      </span>
      <button
        className="ctx-pressure__btn"
        onClick={compactNow}
        disabled={busy || !sessionId}
      >
        {busy ? "Compacting…" : "Compact now"}
      </button>
      {note && (
        <span className="ctx-pressure__result" aria-live="polite">
          {note}
        </span>
      )}
    </div>
  );
}
