import { useState, useEffect } from "react";
import { api } from "../api/PondApiClient";
import type { PromptTemplate } from "../api/types";

export function Prompts() {
  const [prompts, setPrompts] = useState<PromptTemplate[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving]   = useState(false);
  const [error, setError]     = useState<string | null>(null);

  useEffect(() => {
    api.listPrompts().then((p) => { setPrompts(p); if (p.length) select(p[0].name); })
      .catch((e) => setError(String(e))).finally(() => setLoading(false));
  }, []);

  function select(name: string) {
    setSelected(name);
    const p = prompts.find((x) => x.name === name);
    if (p) setContent(p.content);
  }

  async function save() {
    if (!selected) return;
    setSaving(true);
    try {
      const updated = await api.updatePrompt(selected, content);
      setPrompts((prev) => prev.map((p) => p.name === selected ? updated : p));
    } catch (e) { setError(String(e)); } finally { setSaving(false); }
  }

  if (loading) return <p style={hint}>Loading…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;

  return (
    <div style={styles.root}>
      <div style={styles.tabs}>
        {prompts.map((p) => (
          <button key={p.name} style={{ ...styles.tab, ...(selected === p.name ? styles.tabActive : {}) }} onClick={() => select(p.name)}>
            {p.name}
          </button>
        ))}
      </div>
      {selected && (
        <>
          <textarea style={styles.editor} value={content} onChange={(e) => setContent(e.target.value)} rows={16} spellCheck={false} />
          <button style={styles.saveBtn} onClick={save} disabled={saving}>{saving ? "Saving…" : "Save"}</button>
        </>
      )}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  tabs: { display: "flex", gap: "4px", flexWrap: "wrap" as const },
  tab: { height: "30px", padding: "0 var(--space-4)", borderRadius: "var(--radius-md)", border: "1px solid var(--color-border)", background: "transparent", cursor: "pointer", fontSize: "var(--text-sm)", fontFamily: "var(--font-body)", color: "var(--color-text-secondary)" },
  tabActive: { background: "var(--color-accent-soft)", color: "var(--color-accent)", borderColor: "var(--color-accent-soft)", fontWeight: 600 },
  editor: { border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "var(--space-4)", fontSize: "var(--text-sm)", fontFamily: "var(--font-mono)", background: "var(--color-bg)", color: "var(--color-text)", resize: "vertical" as const, lineHeight: "1.6", userSelect: "text" as const },
  saveBtn: { height: "36px", padding: "0 var(--space-6)", borderRadius: "var(--radius-md)", background: "var(--color-accent)", color: "#fff", border: "none", cursor: "pointer", fontSize: "var(--text-base)", fontWeight: 600, fontFamily: "var(--font-body)", alignSelf: "flex-start" },
};
