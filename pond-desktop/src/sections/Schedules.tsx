import { useState, useEffect } from "react";
import { Card, Chip } from "@heroui/react";
import { api } from "../api/PondApiClient";
import type { Schedule } from "../api/types";

export function Schedules() {
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  useEffect(() => {
    api.listSchedules().then(setSchedules).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }, []);

  if (loading) return <p style={hint}>Loading schedules…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (!schedules.length) return <p style={hint}>No schedules configured.</p>;

  return (
    <div style={styles.root}>
      {schedules.map((s) => (
        <Card key={s.id}>
          <div style={styles.cardBody}>
            <div style={styles.header}>
              <span style={styles.name}>{s.name}</span>
              <code style={styles.cron}>{s.cron}</code>
              <Chip
                color={s.enabled ? "success" : "default"}
                variant="soft"
                size="sm"
              >
                {s.enabled ? "Active" : "Paused"}
              </Chip>
            </div>
            <p style={styles.prompt}>{s.prompt}</p>
          </div>
        </Card>
      ))}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-3)", maxWidth: "var(--content-max-width)" },
  cardBody: { padding: "var(--space-4)", display: "flex", flexDirection: "column", gap: "var(--space-2)" },
  header: { display: "flex", alignItems: "center", gap: "var(--space-3)" },
  name: { fontWeight: 600, fontSize: "var(--text-base)", color: "var(--color-text)" },
  cron: { fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)", background: "rgba(23,22,22,0.05)", padding: "1px 6px", borderRadius: "4px" },
  prompt: { margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-secondary)" },
};
