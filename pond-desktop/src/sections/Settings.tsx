import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api } from "../api/PondApiClient";
import { useAppDispatch, useAppState } from "../state/AppContext";
import type { Settings as SettingsType } from "../api/types";

export function Settings() {
  const state    = useAppState();
  const dispatch = useAppDispatch();
  const [settings, setSettings] = useState<Partial<SettingsType>>({});
  const [loading, setLoading]   = useState(true);
  const [saving, setSaving]     = useState(false);
  const [saved, setSaved]       = useState(false);
  const [error, setError]       = useState<string | null>(null);
  const [hotkey, setHotkey]     = useState("CmdOrCtrl+Shift+V");

  useEffect(() => {
    api.getSettings()
      .then((s) => setSettings(s))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  async function save() {
    setSaving(true);
    setSaved(false);
    try {
      const updated = await api.updateSettings(settings);
      setSettings(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  }

  async function applyHotkey() {
    try { await invoke("set_hotkey", { hotkey }); } catch (e) { setError(String(e)); }
  }

  function patch(key: keyof SettingsType, value: unknown) {
    setSettings((prev) => ({ ...prev, [key]: value }));
  }

  if (loading) return <p style={hint}>Loading settings…</p>;

  return (
    <div style={styles.root}>
      {error && <p style={styles.error}>{error}</p>}

      <Group title="General">
        <Field label="Assistant name">
          <input style={styles.input} value={settings.assistant_name ?? ""} onChange={(e) => patch("assistant_name", e.target.value)} />
        </Field>
        <Field label="Your name">
          <input style={styles.input} value={settings.user_name ?? ""} onChange={(e) => patch("user_name", e.target.value)} />
        </Field>
        <Field label="Location">
          <input style={styles.input} value={settings.location ?? ""} onChange={(e) => patch("location", e.target.value)} placeholder="City, Country" />
        </Field>
        <Field label="Timezone">
          <input style={styles.input} value={settings.timezone ?? ""} onChange={(e) => patch("timezone", e.target.value)} placeholder="America/New_York" />
        </Field>
      </Group>

      <Group title="Voice">
        <Field label="Wake word">
          <input style={styles.input} value={settings.wake_word ?? ""} onChange={(e) => patch("wake_word", e.target.value)} placeholder="goose" />
        </Field>
        <Field label="Summon hotkey">
          <div style={{ display: "flex", gap: "var(--space-2)" }}>
            <input style={styles.input} value={hotkey} onChange={(e) => setHotkey(e.target.value)} />
            <button style={styles.inlineBtn} onClick={applyHotkey}>Apply</button>
          </div>
        </Field>
      </Group>

      <Group title="Server">
        <Field label="Server URL">
          <div style={{ display: "flex", gap: "var(--space-2)" }}>
            <input style={styles.input} value={state.serverUrl} onChange={(e) => dispatch({ type: "SET_SERVER_URL", payload: e.target.value })} />
          </div>
        </Field>
      </Group>

      <button style={styles.saveBtn} onClick={save} disabled={saving}>
        {saving ? "Saving…" : saved ? "Saved ✓" : "Save Settings"}
      </button>
    </div>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section style={grpStyles.root}>
      <h3 style={grpStyles.title}>{title}</h3>
      <div style={grpStyles.fields}>{children}</div>
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={fieldStyles.root}>
      <label style={fieldStyles.label}>{label}</label>
      <div style={fieldStyles.control}>{children}</div>
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };
const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-5)", maxWidth: "540px" },
  error: { color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 },
  input: { height: "36px", border: "1px solid var(--color-border-strong)", borderRadius: "var(--radius-md)", padding: "0 var(--space-3)", fontSize: "var(--text-base)", fontFamily: "var(--font-body)", background: "var(--color-bg)", color: "var(--color-text)", width: "100%", userSelect: "text" as const },
  inlineBtn: { height: "36px", padding: "0 var(--space-4)", borderRadius: "var(--radius-md)", border: "1px solid var(--color-border-strong)", background: "transparent", cursor: "pointer", fontSize: "var(--text-base)", fontFamily: "var(--font-body)", fontWeight: 500, color: "var(--color-text)", flexShrink: 0 },
  saveBtn: { height: "40px", padding: "0 var(--space-8)", borderRadius: "var(--radius-md)", background: "var(--color-accent)", color: "#fff", border: "none", cursor: "pointer", fontSize: "var(--text-base)", fontWeight: 600, fontFamily: "var(--font-body)", alignSelf: "flex-start" },
};
const grpStyles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-3)" },
  title: { fontFamily: "var(--font-display)", fontWeight: 700, fontSize: "var(--text-base)", color: "var(--color-text)", margin: 0, paddingBottom: "var(--space-2)", borderBottom: "1px solid var(--color-border)" },
  fields: { display: "flex", flexDirection: "column", gap: "var(--space-3)" },
};
const fieldStyles: Record<string, React.CSSProperties> = {
  root: { display: "flex", alignItems: "center", gap: "var(--space-4)" },
  label: { width: "140px", flexShrink: 0, fontSize: "var(--text-base)", color: "var(--color-text-secondary)", fontWeight: 500 },
  control: { flex: 1 },
};
