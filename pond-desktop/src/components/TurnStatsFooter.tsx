import { Gauge } from "lucide-react";
import type { TurnStats } from "../api/types";

interface TurnStatsFooterProps {
  stats: TurnStats;
}

function fmtMs(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`;
}

function fmtNum(n: number): string {
  return n.toLocaleString("en-US");
}

function fmtTokPerSec(n: number): string {
  return `${n.toFixed(1)} tok/s`;
}

export function TurnStatsFooter({ stats }: TurnStatsFooterProps) {
  const segments: string[] = [];

  if (stats.ttft_ms != null) {
    segments.push(`${fmtMs(stats.ttft_ms)} to first token`);
  }

  if (stats.decode_tok_per_sec != null) {
    segments.push(fmtTokPerSec(stats.decode_tok_per_sec));
  }

  if (stats.context_used_tokens != null && stats.context_limit_tokens != null) {
    const pct = stats.context_pct != null ? ` (${Math.round(stats.context_pct)}%)` : "";
    segments.push(`ctx ${fmtNum(stats.context_used_tokens)}/${fmtNum(stats.context_limit_tokens)}${pct}`);
  }

  if (stats.model_load_ms != null && stats.model_load_ms > 0) {
    segments.push(`model load ${fmtMs(stats.model_load_ms)}`);
  }

  if (stats.inference_count > 1) {
    segments.push(`${stats.inference_count} inferences`);
  }

  if (segments.length === 0) return null;

  return (
    <div className="turn-stats-footer">
      <Gauge size={11} aria-hidden />
      {segments.join(" · ")}
    </div>
  );
}
