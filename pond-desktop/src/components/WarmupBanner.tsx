// ────────────────────────────────────────────────────────────
// WarmupBanner — one hairline strip while the pond precompiles its prompt.
//
// Chrome, not content: hairline border, slate meta text, no ink offset (the
// edge marks content — DESIGN.md rule 3). It names the model and counts the
// seconds in mono, because "Warming up gemma-4-E2B-it · 12s" says what is
// happening; a bare spinner only says that something is. Renders nothing
// outside the warming state — ready needs no announcement here (voice mode
// speaks it), and a failed warm-up changes nothing the user can act on: the
// first turn simply pays the prefill it always used to.
// ────────────────────────────────────────────────────────────

import { useEffect, useState } from "react";
import { Loader2 } from "lucide-react";
import { useWarmupStatus } from "../api/useWarmupStatus";
import "../styles/warmup.css";

export function WarmupBanner() {
  const status = useWarmupStatus();
  const [now, setNow] = useState(() => Date.now());

  const warming = status?.state === "warming";
  useEffect(() => {
    if (!warming) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [warming]);

  if (!status || status.state !== "warming") return null;

  const seconds = Math.max(
    0,
    Math.round(
      (status.started_unix_ms > 0 ? now - status.started_unix_ms : status.elapsed_ms) / 1000,
    ),
  );

  return (
    <div className="warmup-banner" role="status" aria-live="polite">
      <Loader2 size={14} className="warmup-spin" aria-hidden />
      <span>
        Warming up{status.model ? ` ${status.model}` : ""} — first reply will be instant
      </span>
      <span className="warmup-elapsed">{seconds}s</span>
    </div>
  );
}
