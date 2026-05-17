import { Card, CardContent } from "@heroui/react";
import { Coins, ArrowDown, ArrowUp } from "lucide-react";
import type { UsageSummary } from "../api/types";

// Fallback pricing if not provided via settings (GPT-4o as of 2025)
const DEFAULT_INPUT_PER_1M = 2.50;
const DEFAULT_OUTPUT_PER_1M = 10.00;

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10000) return `~${(n / 1000).toFixed(1)}k`;
  return `~${Math.round(n / 1000)}k`;
}

function formatDollars(n: number): string {
  if (n < 0.01) return "<$0.01";
  if (n < 1) return `$${n.toFixed(2)}`;
  return `$${n.toFixed(2)}`;
}

interface Props {
  data: UsageSummary | null;
  loading?: boolean;
  /** Cloud input price per million tokens (from settings). */
  inputPricePerMillion?: number;
  /** Cloud output price per million tokens (from settings). */
  outputPricePerMillion?: number;
}

export function UsageStatsCard({ data, loading, inputPricePerMillion, outputPricePerMillion }: Props) {
  const inputPrice = inputPricePerMillion ?? DEFAULT_INPUT_PER_1M;
  const outputPrice = outputPricePerMillion ?? DEFAULT_OUTPUT_PER_1M;
  if (loading) {
    return (
      <Card className="card">
        <CardContent>
          <div className="card-header" style={{ padding: 0 }}>
            <span className="card__label">Usage & Savings</span>
          </div>
          <p className="muted-12" style={{ marginTop: 8 }}>Loading...</p>
        </CardContent>
      </Card>
    );
  }

  if (!data || data.total_tokens === 0) {
    return (
      <Card className="card">
        <CardContent>
          <div className="card-header" style={{ padding: 0 }}>
            <span className="card__label">Usage & Savings</span>
          </div>
          <p style={{ color: "var(--grey-400)", fontSize: 13, marginTop: 8 }}>
            No usage data yet. Start a conversation to see token usage and savings.
          </p>
        </CardContent>
      </Card>
    );
  }

  const saved =
    (data.total_prompt_tokens / 1_000_000) * inputPrice +
    (data.total_completion_tokens / 1_000_000) * outputPrice;

  return (
    <Card className="card">
      <CardContent>
        <div className="card-header" style={{ padding: 0, marginBottom: 12 }}>
          <span className="card__label">Usage & Savings</span>
        </div>

        <p style={{ fontSize: 13, color: "var(--grey-600)", margin: "0 0 12px" }}>
          {formatTokens(data.total_tokens)} tokens used across {data.session_count} session{data.session_count !== 1 ? "s" : ""}
        </p>

        <div style={{ display: "flex", gap: 12, marginBottom: 14 }}>
          <div style={statBox}>
            <ArrowUp size={12} style={{ color: "var(--color-role-chat)" }} />
            <span style={statNum}>{formatTokens(data.total_prompt_tokens)}</span>
            <span style={statLabel}>input</span>
          </div>
          <div style={statBox}>
            <ArrowDown size={12} style={{ color: "var(--color-success)" }} />
            <span style={statNum}>{formatTokens(data.total_completion_tokens)}</span>
            <span style={statLabel}>output</span>
          </div>
        </div>

        <div style={{
          display: "flex", alignItems: "center", gap: 8,
          padding: "10px 12px", borderRadius: "var(--radius-sm, 6px)",
          background: "rgba(52,199,89,0.06)", border: "1px solid rgba(52,199,89,0.15)",
        }}>
          <Coins size={16} style={{ color: "var(--color-success)", flexShrink: 0 }} />
          <div>
            <div style={{ fontWeight: 600, fontSize: 14, color: "var(--color-success)" }}>
              ~{formatDollars(saved)} saved
            </div>
            <div style={{ fontSize: 11, color: "var(--grey-500)" }}>
              vs cloud API (${inputPrice}/M in + ${outputPrice}/M out)
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

const statBox: React.CSSProperties = {
  display: "flex", flexDirection: "column", alignItems: "center", gap: 2,
  flex: 1, padding: "10px 12px",
  background: "var(--grey-50, #fafafa)", borderRadius: "var(--radius-sm, 6px)",
};
const statNum: React.CSSProperties = { fontWeight: 700, fontSize: 18, fontFamily: "var(--font-mono)" };
const statLabel: React.CSSProperties = { fontSize: 11, color: "var(--grey-500)", textTransform: "uppercase" as const, letterSpacing: "0.04em" };
