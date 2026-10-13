import type { ContextCorpusCoverage } from "../api/types";

// Retrieval-index coverage per corpus, as the Memory page's coverage panel states it.

/** A corpus's coverage as states, not a percentage: `vacant` (nothing written yet, fine)
 *  and `unindexed` (rows retrieval cannot reach) both read 0 and must never look alike. */
export type CoverageState = "vacant" | "excluded" | "unindexed" | "partial" | "complete";

export function coverageState(row: {
  rows: number;
  indexed_rows: number;
  source_rows?: number;
}): CoverageState {
  // Before "vacant": both have zero qualifying rows, but rows that all fail the filter are broken.
  if (row.rows === 0 && (row.source_rows ?? 0) > 0) return "excluded";
  if (row.rows === 0) return "vacant";
  if (row.indexed_rows === 0) return "unindexed";
  if (row.indexed_rows >= row.rows) return "complete";
  return "partial";
}

/** Coverage as a percentage. `null` (no qualifying rows; see `ContextCorpusCoverage`) is a dash;
 *  anything above zero floors at "<1%" so a few vectors never reads as none. */
export function formatCoverage(coverage: number | null | undefined): string {
  if (coverage === null || coverage === undefined) return "—";
  const pct = coverage * 100;
  if (pct > 0 && pct < 1) return "<1%";
  return `${Math.round(pct)}%`;
}

/** The row's state said in words, because a bar at zero is easy to read as a bar. */
export function coverageNote(row: ContextCorpusCoverage): string {
  const stale = row.mismatched > 0
    ? ` ${row.mismatched} carry a vector from another model.`
    : "";
  switch (coverageState(row)) {
    case "vacant":
      return "Nothing stored yet.";
    case "excluded":
      return `${row.source_rows} stored, none reachable — a filter excludes every one. Embedding will not fix this.`;
    case "unindexed":
      return `Not searchable — the assistant can only reach these by recency.${stale}`;
    case "partial":
      return `${row.missing_rows} still to embed.${stale}`;
    case "complete":
      return `Fully searchable.${stale}`;
  }
}
