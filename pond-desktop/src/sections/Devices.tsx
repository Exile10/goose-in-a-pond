import { useState, useEffect } from "react";
import { api } from "../api/PondApiClient";
import type { Device } from "../api/types";

export function Devices() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  useEffect(() => {
    api.listDevices()
      .then(setDevices)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  if (loading) return <p style={hint}>Loading devices…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (!devices.length) return <p style={hint}>No devices registered yet.</p>;

  return (
    <div style={styles.root}>
      <table style={styles.table}>
        <thead>
          <tr>
            <th style={styles.th}>Name</th>
            <th style={styles.th}>Room</th>
            <th style={styles.th}>Status</th>
            <th style={styles.th}>Last seen</th>
          </tr>
        </thead>
        <tbody>
          {devices.map((d) => (
            <tr key={d.id} style={styles.tr}>
              <td style={styles.td}>{d.name}</td>
              <td style={{ ...styles.td, color: "var(--color-text-secondary)" }}>{d.room ?? "—"}</td>
              <td style={styles.td}>
                <span style={{ display: "flex", alignItems: "center", gap: "6px" }}>
                  <span style={{ width: "7px", height: "7px", borderRadius: "50%", background: d.is_online ? "var(--color-success)" : "var(--color-neutral)", flexShrink: 0 }} />
                  {d.is_online ? "Online" : "Offline"}
                </span>
              </td>
              <td style={{ ...styles.td, color: "var(--color-text-tertiary)", fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)" }}>
                {d.last_seen ? new Date(d.last_seen).toLocaleString() : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const styles: Record<string, React.CSSProperties> = {
  root: { maxWidth: "var(--content-max-width)" },
  table: { width: "100%", borderCollapse: "collapse" as const },
  th: { textAlign: "left" as const, fontFamily: "var(--font-display)", fontWeight: 600, fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", textTransform: "uppercase" as const, letterSpacing: "0.05em", padding: "var(--space-2) var(--space-3)", borderBottom: "1px solid var(--color-border)" },
  tr: { borderBottom: "1px solid var(--color-border)" },
  td: { padding: "var(--space-3) var(--space-4)", fontSize: "var(--text-base)", color: "var(--color-text)", height: "48px", verticalAlign: "middle" as const },
};
