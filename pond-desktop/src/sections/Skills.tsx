import { useState, useEffect } from "react";
import { Button, Switch, TextArea } from "@heroui/react";
import { Trash2, Plus } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { UserSkill } from "../api/types";

export function Skills() {
  const [skills, setSkills] = useState<UserSkill[]>([]);
  const [loading, setLoading] = useState(true);
  const [name, setName]     = useState("");
  const [content, setContent] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [error, setError]   = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listSkills(true).then(setSkills).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function add() {
    if (!name.trim() || !content.trim()) return;
    try {
      await api.addSkill(name.trim(), content.trim());
      setName(""); setContent(""); setShowForm(false); load();
    } catch (e) { setError(String(e)); }
  }

  async function toggle(id: string, currentActive: boolean) {
    try { await api.toggleSkill(id, currentActive); load(); } catch (e) { setError(String(e)); }
  }

  async function remove(id: string) {
    try { await api.removeSkill(id); load(); } catch (e) { setError(String(e)); }
  }

  return (
    <div style={styles.root}>
      {/* Add form toggle */}
      <div>
        <Button
          variant="outline"
          onPress={() => setShowForm((v) => !v)}
        >
          <Plus size={14} /> Add Skill
        </Button>
      </div>

      {showForm && (
        <div style={styles.addCard}>
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Skill name"
            aria-label="Skill name"
            style={inputStyle}
          />
          <TextArea
            value={content}
            onChange={(e) => setContent(e.target.value)}
            placeholder="Skill instructions…"
            aria-label="Skill instructions"
            rows={3}
          />
          <Button
            variant="primary"
            onPress={add}
            isDisabled={!name.trim() || !content.trim()}
          >
            Save
          </Button>
        </div>
      )}

      {error && <p style={styles.error}>{error}</p>}
      {loading ? <p style={styles.hint}>Loading…</p> : skills.length === 0 ? <p style={styles.hint}>No skills yet.</p> : (
        <ul style={styles.list}>
          {skills.map((s) => (
            <li key={s.id} style={styles.item}>
              <Switch
                isSelected={s.active}
                onChange={() => toggle(s.id, s.active)}
                size="sm"
                aria-label={`Enable ${s.name}`}
              />
              <div style={{ flex: 1 }}>
                <p style={styles.skillName}>{s.name}</p>
                <p style={styles.skillContent}>{s.content.length > 80 ? s.content.slice(0, 80) + "…" : s.content}</p>
              </div>
              <Button
                variant="danger-soft"
                onPress={() => remove(s.id)}
                aria-label={`Delete ${s.name}`}
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
  addCard: { display: "flex", flexDirection: "column", gap: "var(--space-3)", background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", padding: "var(--space-4)" },
  error: { color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 },
  hint: { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 },
  list: { listStyle: "none", display: "flex", flexDirection: "column", gap: "4px" },
  item: { display: "flex", alignItems: "center", gap: "var(--space-3)", padding: "var(--space-3) var(--space-4)", background: "var(--color-bg)", border: "1px solid var(--color-border)", borderRadius: "var(--radius-md)", minHeight: "48px" },
  skillName: { margin: 0, fontSize: "var(--text-base)", fontWeight: 600, color: "var(--color-text)" },
  skillContent: { margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-secondary)" },
};
