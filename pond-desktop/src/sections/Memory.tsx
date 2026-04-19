import { useState, useEffect } from "react";
import { Button } from "@heroui/react";
import { Trash2 } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { MemoryFragment } from "../api/types";

export function Memory() {
  const [items, setItems]   = useState<MemoryFragment[]>([]);
  const [loading, setLoading] = useState(true);
  const [input, setInput]   = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError]   = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listMemories(30).then(setItems).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function add() {
    if (!input.trim()) return;
    setSaving(true);
    try {
      await api.addMemory(input.trim());
      setInput("");
      load();
    } catch (e) { setError(String(e)); } finally { setSaving(false); }
  }

  async function remove(id: string) {
    try { await api.deleteMemory(id); load(); } catch (e) { setError(String(e)); }
  }

  return (
    <div style={styles.root}>
      <div style={styles.addRow}>
        <div style={{ flex: 1 }}>
          <input
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Add a memory…"
            aria-label="New memory"
            onKeyDown={(e: React.KeyboardEvent) => e.key === "Enter" && add()}
            style={inputStyle}
          />
        </div>
        <Button
          variant="primary"
          onPress={add}
          isDisabled={!input.trim() || saving}
        >
          Add
        </Button>
      </div>
      {error && <p style={styles.error}>{error}</p>}
      {loading ? <p style={styles.hint}>Loading…</p> : items.length === 0 ? <p style={styles.hint}>No memories yet.</p> : (
        <ul style={styles.list}>
          {items.map((m) => (
            <li key={m.id} style={styles.item}>
              <span style={styles.content}>{m.content}</span>
              <span style={styles.date}>{new Date(m.created_at).toLocaleDateString()}</span>
              <Button
                variant="danger-soft"
                onPress={() => remove(m.id)}
                aria-label="Delete memory"
              >
                <Trash2 size={14} />
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const inputStyle: React.CSSProperties = { height: "36px", border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "0 var(--space-3)", fontSize: "var(--text-base)", background: "var(--color-bg)", color: "var(--color-text)", width: "100%" };

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  addRow: { display: "flex", gap: "var(--space-2)", alignItems: "flex-end" },
  error: { color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 },
  hint: { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 },
  list: { listStyle: "none", display: "flex", flexDirection: "column", gap: "4px" },
  item: { display: "flex", alignItems: "center", gap: "var(--space-3)", padding: "var(--space-3) var(--space-4)", background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", minHeight: "48px" },
  content: { flex: 1, fontSize: "var(--text-base)", color: "var(--color-text)", userSelect: "text" as const },
  date: { fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)", flexShrink: 0 },
};
