import { useState, useEffect } from "react";
import { Card, Chip } from "@heroui/react";
import { api } from "../api/PondApiClient";
import type { ModelEntry } from "../api/types";

export function Models() {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]   = useState<string | null>(null);

  useEffect(() => {
    api.listModels().then(setModels).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }, []);

  if (loading) return <p style={hint}>Loading models…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (!models.length) return <p style={hint}>No models configured. Run <code>pond-server setup</code>.</p>;

  return (
    <div style={styles.root}>
      {models.map((m) => (
        <Card key={m.id}>
          <div style={styles.cardBody}>
            <div style={styles.cardHeader}>
              <span style={styles.modelName}>{m.display_name ?? m.name}</span>
              <span style={styles.provider}>{m.provider}</span>
              {m.recommended_role && (
                <Chip variant="primary" size="sm">{m.recommended_role}</Chip>
              )}
              {m.is_active && <Chip color="success" variant="soft" size="sm">Active</Chip>}
            </div>
            {m.ram_estimate_mb && (
              <p style={styles.ramText}>RAM: ~{m.ram_estimate_mb} MB</p>
            )}
          </div>
        </Card>
      ))}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-3)", maxWidth: "var(--content-max-width)" },
  cardBody: { padding: "var(--space-4)" },
  cardHeader: { display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" as const },
  modelName: { fontWeight: 600, fontSize: "var(--text-base)", color: "var(--color-text)" },
  provider: { fontSize: "var(--text-sm)", color: "var(--color-text-secondary)", marginLeft: "auto" },
  ramText: { margin: "var(--space-2) 0 0", fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)", fontFamily: "var(--font-mono)" },
};
