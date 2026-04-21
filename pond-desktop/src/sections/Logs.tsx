import { useState, useEffect, useCallback } from "react";
import { Button } from "@heroui/react";
import { RefreshCw, Download } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { LogEntry } from "../api/types";

type LevelFilter = "ALL" | "INFO" | "WARN" | "ERROR";

export function Logs() {
  const [entries, setEntries]   = useState<LogEntry[]>([]);
  const [level, setLevel]       = useState<LevelFilter>("ALL");
  const [loading, setLoading]   = useState(true);
  const [error, setError]       = useState<string | null>(null);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    api.listLogs({ limit: 200, level: level === "ALL" ? undefined : level })
      .then(setEntries)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [level]);

  useEffect(() => { load(); }, [load]);

  // Auto-refresh every 10s
  useEffect(() => {
    const id = setInterval(load, 10_000);
    return () => clearInterval(id);
  }, [load]);

  function downloadCsv() {
    const url = api.exportLogsUrl();
    const a = document.createElement("a");
    a.href = url;
    a.download = "pond-logs.csv";
    a.click();
  }

  return (
    <div style={styles.root}>
      {/* Filter bar */}
      <div style={styles.filterBar}>
        <div style={styles.filterGroup}>
          {(["ALL", "INFO", "WARN", "ERROR"] as LevelFilter[]).map((l) => (
            <button
              key={l}
              style={{ ...styles.filterBtn, ...(level === l ? styles.filterBtnActive : {}) }}
              onClick={() => setLevel(l)}
            >
              {l}
            </button>
          ))}
        </div>
        <div style={styles.filterActions}>
          <Button variant="outline" onPress={load} isDisabled={loading}>
            <RefreshCw size={13} />
            Refresh
          </Button>
          <Button variant="outline" onPress={downloadCsv}>
            <Download size={13} />
            Download CSV
          </Button>
        </div>
      </div>

      {error && <p style={styles.error}>{error}</p>}

      {/* Table */}
      <div style={styles.tableWrap}>
        <table style={styles.table}>
          <thead>
            <tr>
              <th style={{ ...styles.th, width: "160px" }}>Timestamp</th>
              <th style={{ ...styles.th, width: "72px" }}>Level</th>
              <th style={{ ...styles.th, width: "120px" }}>Source</th>
              <th style={styles.th}>Message</th>
            </tr>
          </thead>
          <tbody>
            {loading && entries.length === 0 && (
              <tr><td colSpan={4} style={styles.empty}>Loading logs…</td></tr>
            )}
            {!loading && entries.length === 0 && (
              <tr><td colSpan={4} style={styles.empty}>No log entries found.</td></tr>
            )}
            {entries.map((e) => (
              <tr key={e.id} style={styles.tr}>
                <td style={styles.td}><span style={styles.ts}>{formatTs(e.timestamp)}</span></td>
                <td style={styles.td}><span style={{ ...styles.levelBadge, ...levelStyle(e.level) }}>{e.level}</span></td>
                <td style={styles.td}><span style={styles.source}>{e.source}</span></td>
                <td style={styles.td}>
                  <span style={styles.msg}>{e.message}</span>
                  {e.metadata && <code style={styles.meta}>{truncate(e.metadata, 120)}</code>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ── Helpers ───────────────────────────────────────────────────

function formatTs(ts: string): string {
  try {
    const d = new Date(ts);
    return d.toLocaleString(undefined, { dateStyle: "short", timeStyle: "medium" });
  } catch {
    return ts;
  }
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n) + "…" : s;
}

function levelStyle(level: string): React.CSSProperties {
  switch (level.toUpperCase()) {
    case "ERROR": return { background: "rgba(220,38,38,0.10)", color: "rgb(185,28,28)", borderColor: "rgba(220,38,38,0.25)" };
    case "WARN":  return { background: "rgba(245,158,11,0.10)", color: "rgb(180,108,0)", borderColor: "rgba(245,158,11,0.25)" };
    default:      return { background: "rgba(23,22,22,0.05)", color: "var(--color-text-secondary)", borderColor: "var(--color-border)" };
  }
}

// ── Styles ────────────────────────────────────────────────────

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-3)", maxWidth: "100%" },
  filterBar: { display: "flex", alignItems: "center", gap: "var(--space-3)", flexWrap: "wrap" as const },
  filterGroup: { display: "flex", gap: "4px" },
  filterBtn: {
    height: "30px", padding: "0 var(--space-3)", borderRadius: "var(--radius-sm)",
    border: "1px solid var(--color-border)", background: "var(--color-bg)",
    cursor: "pointer", fontSize: "var(--text-xs)", color: "var(--color-text-secondary)",
    fontFamily: "var(--font-mono)", fontWeight: 500,
  },
  filterBtnActive: {
    background: "var(--color-accent)", color: "#fff", borderColor: "var(--color-accent)",
  },
  filterActions: { display: "flex", gap: "var(--space-2)", marginLeft: "auto" },
  error: { color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 },
  tableWrap: { overflowX: "auto", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)" },
  table: { width: "100%", borderCollapse: "collapse" as const, fontSize: "var(--text-xs)" },
  th: { padding: "var(--space-2) var(--space-3)", textAlign: "left" as const, background: "rgba(23,22,22,0.03)", borderBottom: "1px solid var(--color-border)", fontWeight: 600, color: "var(--color-text-secondary)", fontSize: "11px", letterSpacing: "0.04em", textTransform: "uppercase" as const },
  tr: { borderBottom: "1px solid var(--color-border)" },
  td: { padding: "var(--space-2) var(--space-3)", verticalAlign: "top" as const },
  empty: { padding: "var(--space-5)", textAlign: "center" as const, color: "var(--color-text-tertiary)" },
  ts: { fontFamily: "var(--font-mono)", fontSize: "11px", color: "var(--color-text-secondary)", whiteSpace: "nowrap" as const },
  levelBadge: { display: "inline-block", padding: "1px 6px", borderRadius: "4px", border: "1px solid", fontFamily: "var(--font-mono)", fontSize: "10px", fontWeight: 700, letterSpacing: "0.05em" },
  source: { fontFamily: "var(--font-mono)", fontSize: "11px", color: "var(--color-text)" },
  msg: { color: "var(--color-text)", lineHeight: "1.5" },
  meta: { display: "block", marginTop: "2px", fontFamily: "var(--font-mono)", fontSize: "10px", color: "var(--color-text-tertiary)", wordBreak: "break-all" as const },
};
