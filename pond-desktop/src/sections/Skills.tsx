import { useState, useEffect } from "react";
import { api } from "../api/PondApiClient";
import type { UserSkill } from "../api/types";

export function Skills() {
  const [skills, setSkills] = useState<UserSkill[]>([]);
  const [loading, setLoading] = useState(true);
  const [name, setName]     = useState("");
  const [content, setContent] = useState("");
  const [error, setError]   = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listSkills(true).then(setSkills).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function add() {
    if (!name.trim() || !content.trim()) return;
    try { await api.addSkill(name.trim(), content.trim()); setName(""); setContent(""); load(); }
    catch (e) { setError(String(e)); }
  }

  async function toggle(id: string) {
    try { await api.toggleSkill(id); load(); } catch (e) { setError(String(e)); }
  }

  async function remove(id: string) {
    try { await api.removeSkill(id); load(); } catch (e) { setError(String(e)); }
  }

  return (
    <div style={styles.root}>
      <details style={styles.addCard}>
        <summary style={styles.summary}>+ Add Skill</summary>
        <div style={styles.addForm}>
          <input style={styles.input} value={name} onChange={(e) => setName(e.target.value)} placeholder="Skill name" />
          <textarea style={styles.textarea} value={content} onChange={(e) => setContent(e.target.value)} placeholder="Skill instructions…" rows={3} />
          <button style={styles.addBtn} onClick={add} disabled={!name.trim() || !content.trim()}>Save</button>
        </div>
      </details>
      {error && <p style={styles.error}>{error}</p>}
      {loading ? <p style={styles.hint}>Loading…</p> : skills.length === 0 ? <p style={styles.hint}>No skills yet.</p> : (
        <ul style={styles.list}>
          {skills.map((s) => (
            <li key={s.id} style={styles.item}>
              <button style={{ ...styles.toggleBtn, background: s.enabled ? "var(--color-accent-soft)" : "transparent", color: s.enabled ? "var(--color-accent)" : "var(--color-text-tertiary)" }} onClick={() => toggle(s.id)}>
                {s.enabled ? "●" : "○"}
              </button>
              <div style={{ flex: 1 }}>
                <p style={styles.skillName}>{s.name}</p>
                <p style={styles.skillContent}>{s.content.length > 80 ? s.content.slice(0, 80) + "…" : s.content}</p>
              </div>
              <button style={styles.del} onClick={() => remove(s.id)}>×</button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  addCard: { background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", padding: "var(--space-4)" },
  summary: { cursor: "pointer", fontWeight: 600, fontSize: "var(--text-base)", color: "var(--color-accent)", userSelect: "none" as const },
  addForm: { display: "flex", flexDirection: "column", gap: "var(--space-3)", marginTop: "var(--space-3)" },
  input: { height: "36px", border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "0 var(--space-3)", fontSize: "var(--text-base)", fontFamily: "var(--font-body)", background: "var(--color-bg)", color: "var(--color-text)", userSelect: "text" as const },
  textarea: { border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "var(--space-2) var(--space-3)", fontSize: "var(--text-base)", fontFamily: "var(--font-body)", background: "var(--color-bg)", color: "var(--color-text)", resize: "vertical" as const, userSelect: "text" as const },
  addBtn: { height: "36px", padding: "0 var(--space-5)", borderRadius: "var(--radius-md)", background: "var(--color-accent)", color: "#fff", border: "none", cursor: "pointer", fontSize: "var(--text-base)", fontWeight: 600, fontFamily: "var(--font-body)", alignSelf: "flex-start" },
  error: { color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 },
  hint: { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 },
  list: { listStyle: "none", display: "flex", flexDirection: "column", gap: "4px" },
  item: { display: "flex", alignItems: "center", gap: "var(--space-3)", padding: "var(--space-3) var(--space-4)", background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)" },
  toggleBtn: { width: "28px", height: "28px", borderRadius: "50%", border: "1px solid var(--color-border-strong)", cursor: "pointer", fontSize: "16px", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0 },
  skillName: { margin: 0, fontSize: "var(--text-base)", fontWeight: 600, color: "var(--color-text)" },
  skillContent: { margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-secondary)" },
  del: { background: "none", border: "none", cursor: "pointer", color: "var(--color-text-tertiary)", fontSize: "18px", padding: "0 4px", borderRadius: "4px", lineHeight: "1", flexShrink: 0 },
};
