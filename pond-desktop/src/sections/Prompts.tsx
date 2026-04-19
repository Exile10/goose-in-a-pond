import { useState, useEffect } from "react";
import { Tabs, Button } from "@heroui/react";
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
    api.listPrompts().then((p) => { const list = Array.isArray(p) ? p : []; setPrompts(list); if (list.length) select(list[0].name); })
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
  if (!prompts.length) return <p style={hint}>No prompt templates found. Prompts are created automatically when the agent runs for the first time.</p>;

  return (
    <div style={styles.root}>
      {prompts.length > 0 && (
        <Tabs
          selectedKey={selected ?? undefined}
          onSelectionChange={(k) => select(String(k))}
        >
          <Tabs.ListContainer>
            <Tabs.List aria-label="Prompt templates">
              {prompts.map((p) => (
                <Tabs.Tab key={p.name} id={p.name}>
                  <Tabs.Indicator />
                  {p.name}
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>
        </Tabs>
      )}
      {selected && (
        <>
          <textarea style={styles.editor} value={content} onChange={(e) => setContent(e.target.value)} rows={16} spellCheck={false} />
          <Button variant="primary" onPress={save} isDisabled={saving}>{saving ? "Saving…" : "Save"}</Button>
        </>
      )}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  editor: { border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "var(--space-4)", fontSize: "var(--text-sm)", fontFamily: "var(--font-mono)", background: "var(--color-bg)", color: "var(--color-text)", resize: "vertical" as const, lineHeight: "1.6", userSelect: "text" as const },
};
