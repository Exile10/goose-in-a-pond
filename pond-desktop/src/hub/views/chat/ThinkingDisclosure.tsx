import { useState } from "react";
import { ChevronRight } from "lucide-react";

/**
 * The model's reasoning, as one line you can open.
 *
 * Replaces the always-expanded panel that put several paragraphs of half-formed
 * reasoning above every answer. Reasoning is worth keeping and rarely worth
 * reading, so it collapses to a single line that reports how long it took —
 * and says so in the past tense once it is over, because "Thinking…" that never
 * stops is a spinner that lies.
 *
 * While it runs, the label carries a sweep from left to right. That is the only
 * moving thing in the thread at that moment, so it reads as the model working
 * without needing a separate spinner.
 */

/** "a moment" / "8 seconds" / "1m 04s" — never a bare millisecond count. */
export function formatThinkingTime(ms: number | undefined): string {
  if (ms === undefined || ms < 0) return "a moment";
  const seconds = Math.round(ms / 1000);
  if (seconds < 1) return "a moment";
  if (seconds === 1) return "1 second";
  if (seconds < 60) return `${seconds} seconds`;
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

interface ThinkingDisclosureProps {
  blocks: string[];
  /** Still reasoning — shows the sweep and the present tense. */
  active: boolean;
  /** Wall time the reasoning spanned; undefined while it is still running. */
  ms?: number;
}

export function ThinkingDisclosure({ blocks, active, ms }: ThinkingDisclosureProps) {
  const [open, setOpen] = useState(false);
  const label = active ? "Thinking" : `Thought for ${formatThinkingTime(ms)}`;

  return (
    <div className={`think${active ? " think--active" : ""}${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="think__toggle"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="think__label">{label}</span>
        <ChevronRight className="think__chev" size={14} aria-hidden="true" />
      </button>

      {open && (
        <div className="think__body">
          {blocks.map((block, i) => (
            <p key={i}>{block}</p>
          ))}
        </div>
      )}
    </div>
  );
}
