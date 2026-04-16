import { useState, useEffect } from "react";
import { api } from "../api/PondApiClient";
import type { AgentTool, AgentRecipe, PromptExtra } from "../api/types";

type Tab = "tools" | "extras" | "recipes";

export function Agent() {
  const [tab, setTab] = useState<Tab>("tools");

  return (
    <div style={styles.root}>
      <div style={styles.tabs}>
        {(["tools", "extras", "recipes"] as Tab[]).map((t) => (
          <button
            key={t}
            style={{ ...styles.tab, ...(tab === t ? styles.tabActive : {}) }}
            onClick={() => setTab(t)}
          >
            {t === "tools" ? "MCP Tools" : t === "extras" ? "Prompt Extras" : "Recipes"}
          </button>
        ))}
      </div>
      {tab === "tools"   && <ToolsPanel />}
      {tab === "extras"  && <ExtrasPanel />}
      {tab === "recipes" && <RecipesPanel />}
    </div>
  );
}

// ── MCP Tools ────────────────────────────────────────────────

function ToolsPanel() {
  const [tools, setTools]   = useState<AgentTool[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]   = useState<string | null>(null);

  useEffect(() => {
    api.listTools().then(setTools).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }, []);

  if (loading) return <p style={hint}>Loading tools…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (!tools.length) return <p style={hint}>No MCP tools loaded. Start pond-server with an extension enabled.</p>;

  // Group tools by extension
  const byExtension: Record<string, AgentTool[]> = {};
  for (const t of tools) {
    (byExtension[t.extension] ??= []).push(t);
  }

  return (
    <div style={styles.panelRoot}>
      {Object.entries(byExtension).map(([ext, extTools]) => (
        <div key={ext} style={styles.group}>
          <div style={styles.groupHeader}>
            <span style={styles.extBadge}>{ext}</span>
            <span style={styles.toolCount}>{extTools.length} tool{extTools.length !== 1 ? "s" : ""}</span>
          </div>
          <ul style={styles.toolList}>
            {extTools.map((t) => (
              <li key={t.name} style={styles.toolItem}>
                <code style={styles.toolName}>{t.name.replace(`${ext}__`, "")}</code>
                {t.description && <p style={styles.toolDesc}>{t.description}</p>}
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

// ── Prompt Extras ────────────────────────────────────────────

function ExtrasPanel() {
  const [extras, setExtras]   = useState<PromptExtra[]>([]);
  const [loading, setLoading] = useState(true);
  const [key, setKey]         = useState("");
  const [content, setContent] = useState("");
  const [saving, setSaving]   = useState(false);
  const [error, setError]     = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listExtras().then(setExtras).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function add() {
    if (!key.trim() || !content.trim()) return;
    setSaving(true);
    try {
      await api.addExtra(key.trim(), content.trim());
      setKey(""); setContent(""); load();
    } catch (e) { setError(String(e)); } finally { setSaving(false); }
  }

  async function remove(k: string) {
    try { await api.deleteExtra(k); load(); } catch (e) { setError(String(e)); }
  }

  return (
    <div style={styles.panelRoot}>
      <details style={styles.addCard}>
        <summary style={styles.summary}>+ Add Extra</summary>
        <div style={styles.addForm}>
          <input
            style={styles.input}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            placeholder="Key (e.g. context_note)"
          />
          <textarea
            style={styles.textarea}
            value={content}
            onChange={(e) => setContent(e.target.value)}
            placeholder="Content injected into system prompt…"
            rows={3}
          />
          <button
            style={styles.addBtn}
            onClick={add}
            disabled={saving || !key.trim() || !content.trim()}
          >
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </details>
      {error && <p style={styles.error}>{error}</p>}
      {loading ? <p style={hint}>Loading…</p> : extras.length === 0 ? (
        <p style={hint}>No prompt extras. Extras are injected into every agent system prompt.</p>
      ) : (
        <ul style={styles.extraList}>
          {extras.map((ex) => (
            <li key={ex.key} style={styles.extraItem}>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={styles.extraHeader}>
                  <code style={styles.extraKey}>{ex.key}</code>
                  {!ex.enabled && <span style={styles.disabledPill}>Disabled</span>}
                </div>
                <p style={styles.extraContent}>
                  {ex.content.length > 120 ? ex.content.slice(0, 120) + "…" : ex.content}
                </p>
              </div>
              <button style={styles.del} onClick={() => remove(ex.key)} title="Delete extra">×</button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ── Recipes ──────────────────────────────────────────────────

function RecipesPanel() {
  const [recipes, setRecipes]   = useState<AgentRecipe[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading]   = useState(true);
  const [error, setError]       = useState<string | null>(null);

  useEffect(() => {
    api.listRecipes()
      .then((r) => { setRecipes(r); if (r.length) setSelected(r[0].name); })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const current = recipes.find((r) => r.name === selected);

  if (loading) return <p style={hint}>Loading recipes…</p>;
  if (error)   return <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>;
  if (!recipes.length) return (
    <p style={hint}>
      No recipes found. Import one with{" "}
      <code style={inlineCode}>pond-server recipes import &lt;name&gt; &lt;file.yaml&gt;</code>.
    </p>
  );

  return (
    <div style={styles.panelRoot}>
      <div style={styles.recipeLayout}>
        <ul style={styles.recipeList}>
          {recipes.map((r) => (
            <li
              key={r.name}
              style={{ ...styles.recipeItem, ...(selected === r.name ? styles.recipeItemActive : {}) }}
              onClick={() => setSelected(r.name)}
            >
              <span style={styles.recipeName}>{r.name}</span>
              {r.description && <span style={styles.recipeDesc}>{r.description}</span>}
            </li>
          ))}
        </ul>
        <div style={styles.recipeContent}>
          {current ? (
            <>
              <div style={styles.recipeContentHeader}>
                <span style={styles.recipeName}>{current.name}</span>
                {current.description && <p style={styles.recipeFullDesc}>{current.description}</p>}
              </div>
              <pre style={styles.yaml}>{current.yaml}</pre>
            </>
          ) : (
            <p style={hint}>Select a recipe to view its YAML.</p>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Styles ───────────────────────────────────────────────────

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const inlineCode: React.CSSProperties = { fontFamily: "var(--font-mono)", fontSize: "0.85em", background: "rgba(23,22,22,0.06)", padding: "1px 5px", borderRadius: "4px" };

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },

  // Tabs
  tabs: { display: "flex", gap: "4px", flexWrap: "wrap" as const },
  tab: { height: "30px", padding: "0 var(--space-4)", borderRadius: "var(--radius-md)", border: "1px solid var(--color-border)", background: "transparent", cursor: "pointer", fontSize: "var(--text-sm)", fontFamily: "var(--font-body)", color: "var(--color-text-secondary)" },
  tabActive: { background: "var(--color-accent-soft)", color: "var(--color-accent)", borderColor: "var(--color-accent-soft)", fontWeight: 600 },

  // Panel
  panelRoot: { display: "flex", flexDirection: "column", gap: "var(--space-3)" },

  // Tools
  group: { background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", overflow: "hidden" },
  groupHeader: { display: "flex", alignItems: "center", gap: "var(--space-3)", padding: "var(--space-3) var(--space-4)", borderBottom: "1px solid var(--color-border)", background: "rgba(23,22,22,0.02)" },
  extBadge: { fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)", fontWeight: 600, color: "var(--color-accent)", background: "var(--color-accent-soft)", padding: "2px 8px", borderRadius: "var(--radius-pill)" },
  toolCount: { fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", marginLeft: "auto" },
  toolList: { listStyle: "none", display: "flex", flexDirection: "column", gap: "0" },
  toolItem: { padding: "var(--space-3) var(--space-4)", borderBottom: "1px solid var(--color-border)", display: "flex", flexDirection: "column", gap: "2px" },
  toolName: { fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)", color: "var(--color-text)", fontWeight: 500 },
  toolDesc: { margin: 0, fontSize: "var(--text-xs)", color: "var(--color-text-secondary)", lineHeight: "1.5" },

  // Extras
  addCard: { background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", padding: "var(--space-4)" },
  summary: { cursor: "pointer", fontWeight: 600, fontSize: "var(--text-base)", color: "var(--color-accent)", userSelect: "none" as const },
  addForm: { display: "flex", flexDirection: "column", gap: "var(--space-3)", marginTop: "var(--space-3)" },
  input: { height: "36px", border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "0 var(--space-3)", fontSize: "var(--text-base)", fontFamily: "var(--font-body)", background: "var(--color-bg)", color: "var(--color-text)", userSelect: "text" as const },
  textarea: { border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "var(--space-2) var(--space-3)", fontSize: "var(--text-sm)", fontFamily: "var(--font-mono)", background: "var(--color-bg)", color: "var(--color-text)", resize: "vertical" as const, lineHeight: "1.6", userSelect: "text" as const },
  addBtn: { height: "36px", padding: "0 var(--space-5)", borderRadius: "var(--radius-md)", background: "var(--color-accent)", color: "#fff", border: "none", cursor: "pointer", fontSize: "var(--text-base)", fontWeight: 600, fontFamily: "var(--font-body)", alignSelf: "flex-start" },
  error: { color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 },
  extraList: { listStyle: "none", display: "flex", flexDirection: "column", gap: "4px" },
  extraItem: { display: "flex", alignItems: "flex-start", gap: "var(--space-3)", padding: "var(--space-3) var(--space-4)", background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)" },
  extraHeader: { display: "flex", alignItems: "center", gap: "var(--space-2)", marginBottom: "2px" },
  extraKey: { fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)", color: "var(--color-accent)", fontWeight: 600 },
  disabledPill: { fontSize: "var(--text-xs)", fontWeight: 600, background: "rgba(23,22,22,0.06)", color: "var(--color-text-tertiary)", padding: "1px 6px", borderRadius: "var(--radius-pill)" },
  extraContent: { margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-secondary)", lineHeight: "1.5" },
  del: { background: "none", border: "none", cursor: "pointer", color: "var(--color-text-tertiary)", fontSize: "18px", padding: "0 4px", borderRadius: "4px", lineHeight: "1", flexShrink: 0 },

  // Recipes
  recipeLayout: { display: "flex", gap: "var(--space-3)", minHeight: "320px" },
  recipeList: { listStyle: "none", display: "flex", flexDirection: "column", gap: "2px", width: "180px", flexShrink: 0 },
  recipeItem: { padding: "var(--space-2) var(--space-3)", borderRadius: "var(--radius-md)", cursor: "pointer", display: "flex", flexDirection: "column", gap: "2px", border: "1px solid transparent" },
  recipeItemActive: { background: "var(--color-accent-soft)", borderColor: "var(--color-accent-soft)" },
  recipeName: { fontWeight: 600, fontSize: "var(--text-sm)", color: "var(--color-text)" },
  recipeDesc: { fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const },
  recipeContent: { flex: 1, display: "flex", flexDirection: "column", gap: "var(--space-2)", minWidth: 0 },
  recipeContentHeader: { display: "flex", flexDirection: "column", gap: "2px" },
  recipeFullDesc: { margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-secondary)" },
  yaml: { flex: 1, margin: 0, padding: "var(--space-4)", background: "rgba(23,22,22,0.03)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", fontFamily: "var(--font-mono)", fontSize: "var(--text-xs)", color: "var(--color-text)", lineHeight: "1.6", overflow: "auto", whiteSpace: "pre" as const },
};
