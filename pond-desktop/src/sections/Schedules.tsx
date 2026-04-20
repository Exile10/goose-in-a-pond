import { useState, useEffect } from "react";
import { Card, Button, Chip } from "@heroui/react";
import { Plus, Trash2, Play, Pause, ChevronDown, ChevronUp } from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type { Schedule } from "../api/types";

export function Schedules() {
  const state = useAppState();
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [actionMsg, setActionMsg] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // New schedule form
  const [showForm, setShowForm]   = useState(false);
  const [name, setName]           = useState("");
  const [cron, setCron]           = useState("");
  const [prompt, setPrompt]       = useState("");
  const [submitting, setSubmitting] = useState(false);

  function load() {
    setLoading(true);
    api.listSchedules()
      .then(setSchedules)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  function flashMsg(msg: string, isError = false) {
    if (isError) { setActionError(msg); setTimeout(() => setActionError(null), 4000); }
    else          { setActionMsg(msg);  setTimeout(() => setActionMsg(null), 3000); }
  }

  async function handleCreate() {
    if (!name.trim() || !cron.trim() || !prompt.trim()) return;
    setSubmitting(true);
    try {
      await api.createSchedule({ name: name.trim(), cron: cron.trim(), prompt: prompt.trim(), enabled: true });
      setName(""); setCron(""); setPrompt(""); setShowForm(false);
      flashMsg("Schedule created.");
      load();
    } catch (e) { flashMsg(String(e), true); }
    finally { setSubmitting(false); }
  }

  async function handleDelete(id: string, schedName: string) {
    if (!confirm(`Delete schedule "${schedName}"?`)) return;
    try {
      await api.deleteSchedule(id);
      flashMsg("Schedule deleted.");
      load();
    } catch (e) { flashMsg(String(e), true); }
  }

  async function handleToggle(s: Schedule) {
    try {
      if (s.enabled) {
        await api.pauseSchedule(s.id);
        flashMsg(`"${s.name}" paused.`);
      } else {
        await api.resumeSchedule(s.id);
        flashMsg(`"${s.name}" resumed.`);
      }
      load();
    } catch (e) { flashMsg(String(e), true); }
  }

  async function handleRunNow(s: Schedule) {
    try {
      await api.runScheduleNow(s.id);
      flashMsg(`"${s.name}" triggered.`);
    } catch (e) { flashMsg(String(e), true); }
  }

  return (
    <div style={styles.root}>
      {/* Header row */}
      <div style={styles.topRow}>
        <span style={styles.countLabel}>
          {loading ? "Loading…" : `${schedules.length} schedule${schedules.length !== 1 ? "s" : ""}`}
        </span>
        <Button
          variant={showForm ? "outline" : "primary"}
          isDisabled={!state.serverOnline}
          onPress={() => setShowForm((v) => !v)}
        >
          {showForm ? <><ChevronUp size={14} /> Cancel</> : <><Plus size={14} /> New Schedule</>}
        </Button>
      </div>

      {/* Feedback */}
      {actionMsg   && <p style={{ ...hint, color: "var(--color-success)" }}>{actionMsg}</p>}
      {actionError && <p style={{ ...hint, color: "var(--color-destructive)" }}>{actionError}</p>}

      {/* Create form */}
      {showForm && (
        <div style={styles.form}>
          <h4 style={styles.formTitle}>New Schedule</h4>
          <div style={styles.formRow}>
            <label style={styles.label}>Name</label>
            <input
              style={inp}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Daily briefing"
              aria-label="Schedule name"
            />
          </div>
          <div style={styles.formRow}>
            <label style={styles.label}>
              Cron expression
              <span style={styles.hint}> — sec min hr dom mon dow</span>
            </label>
            <input
              style={inp}
              value={cron}
              onChange={(e) => setCron(e.target.value)}
              placeholder="0 0 8 * * *"
              aria-label="Cron expression"
              spellCheck={false}
            />
          </div>
          <div style={styles.formRow}>
            <label style={styles.label}>Prompt</label>
            <textarea
              style={{ ...inp, height: "72px", resize: "vertical" }}
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="Give me a morning briefing: weather, calendar, and top news."
              aria-label="Schedule prompt"
            />
          </div>
          <div style={{ display: "flex", justifyContent: "flex-end", gap: "var(--space-2)" }}>
            <Button variant="ghost" onPress={() => setShowForm(false)}>Cancel</Button>
            <Button
              variant="primary"
              onPress={handleCreate}
              isDisabled={submitting || !name.trim() || !cron.trim() || !prompt.trim()}
            >
              {submitting ? "Creating…" : "Create Schedule"}
            </Button>
          </div>
        </div>
      )}

      {/* List */}
      {error && <p style={{ ...hint, color: "var(--color-destructive)" }}>{error}</p>}

      {!loading && !error && schedules.length === 0 && (
        <p style={hint}>No schedules yet. Create one to automate recurring tasks.</p>
      )}

      {schedules.map((s) => (
        <Card key={s.id}>
          <div style={styles.cardBody}>
            {/* Top row: name + cron + status */}
            <div style={styles.cardHeader}>
              <span style={styles.name}>{s.name}</span>
              <code style={styles.cron}>{s.cron}</code>
              <Chip color={s.enabled ? "success" : "default"} variant="soft" size="sm">
                {s.enabled ? "Active" : "Paused"}
              </Chip>
            </div>

            {/* Prompt preview */}
            {s.prompt && (
              <p style={styles.promptText}>
                {s.prompt.length > 120 ? s.prompt.slice(0, 120) + "…" : s.prompt}
              </p>
            )}

            {/* Actions */}
            <div style={styles.actions}>
              <Button
                variant="outline"
                size="sm"
                onPress={() => handleToggle(s)}
                isDisabled={!state.serverOnline}
                aria-label={s.enabled ? "Pause schedule" : "Resume schedule"}
              >
                {s.enabled ? <><Pause size={12} /> Pause</> : <><ChevronDown size={12} /> Resume</>}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onPress={() => handleRunNow(s)}
                isDisabled={!state.serverOnline}
                aria-label="Run now"
              >
                <Play size={12} /> Run now
              </Button>
              <Button
                variant="danger-soft"
                size="sm"
                onPress={() => handleDelete(s.id, s.name)}
                isDisabled={!state.serverOnline}
                aria-label="Delete schedule"
              >
                <Trash2 size={12} /> Delete
              </Button>
            </div>
          </div>
        </Card>
      ))}
    </div>
  );
}

const hint: React.CSSProperties = { color: "var(--color-text-tertiary)", fontSize: "var(--text-sm)", margin: 0 };

const inp: React.CSSProperties = {
  height: "36px",
  border: "1px solid var(--color-border-strong)",
  borderRadius: "var(--radius-md)",
  padding: "0 var(--space-3)",
  fontSize: "var(--text-base)",
  fontFamily: "var(--font-body)",
  background: "var(--color-bg)",
  color: "var(--color-text)",
  width: "100%",
  userSelect: "text" as const,
};

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-3)", maxWidth: "var(--content-max-width)" },
  topRow: { display: "flex", alignItems: "center", justifyContent: "space-between", gap: "var(--space-3)" },
  countLabel: { fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)" },

  form: {
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-3)",
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-md)",
    padding: "var(--space-4)",
  },
  formTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-base)",
    color: "var(--color-text)",
    margin: 0,
  },
  formRow: { display: "flex", flexDirection: "column" as const, gap: "var(--space-1)" },
  label: { fontSize: "var(--text-sm)", fontWeight: 500, color: "var(--color-text-secondary)" },
  hint: { fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", fontWeight: 400 },

  cardBody: { padding: "var(--space-4)", display: "flex", flexDirection: "column" as const, gap: "var(--space-2)" },
  cardHeader: { display: "flex", alignItems: "center", gap: "var(--space-3)", flexWrap: "wrap" as const },
  name: { fontWeight: 600, fontSize: "var(--text-base)", color: "var(--color-text)", flex: 1, minWidth: 0 },
  cron: {
    fontFamily: "var(--font-mono)",
    fontSize: "var(--text-sm)",
    color: "var(--color-text-tertiary)",
    background: "rgba(23,22,22,0.05)",
    padding: "1px 6px",
    borderRadius: "4px",
    flexShrink: 0,
  },
  promptText: {
    margin: 0,
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
    lineHeight: "1.5",
  },
  actions: { display: "flex", gap: "var(--space-2)", flexWrap: "wrap" as const },
};
