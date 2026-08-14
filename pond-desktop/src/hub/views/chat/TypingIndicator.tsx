import { Goose } from "../../../components/Goose";
import { workingQuip } from "../../../components/quips";

interface TypingIndicatorProps {
  /**
   * Stable for the length of a turn, so the quip does not reshuffle mid-run.
   * Optional: callers without a per-turn seed still get a usable line.
   */
  seed?: number;
  /**
   * What the backend says it is doing, from the stream's `status` frames.
   * Preferred over the local quip whenever it is present — the server knows
   * which tool is running and a made-up line does not.
   */
  status?: string;
}

/**
 * Shown while a turn is running.
 *
 * Sits directly above the composer rather than as a row in the thread. It is a
 * statement about the app's current state, not a message in the conversation —
 * and putting it at the bottom keeps it in view without the thread having to
 * stay scrolled.
 */
export function TypingIndicator({ seed = 0, status }: TypingIndicatorProps) {
  return (
    <div className="ch-working" role="status" aria-live="polite">
      <Goose state="working" size={40} water={false} />
      {/* The backend's own account first; the local quip only covers the gap
          before the first status frame arrives. */}
      <span className="ch-working__quip">{status?.trim() || workingQuip(seed)}</span>
      <span className="ch-working__dots" aria-hidden="true">
        <span />
        <span />
        <span />
      </span>
    </div>
  );
}
