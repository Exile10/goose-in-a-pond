import { useAppState, useAppDispatch } from "../state/AppContext";

export function Dashboard() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  const recentMessages = state.transcript.slice(-5).reverse();

  return (
    <div style={styles.root}>
      {/* Server status card */}
      <section style={styles.card}>
        <h3 style={styles.cardTitle}>Server Status</h3>
        <div style={styles.statusRow}>
          <span style={{
            ...styles.statusDot,
            background: state.serverOnline
              ? "var(--color-success)"
              : state.serverStarting
              ? "var(--color-warning)"
              : "var(--color-neutral)",
          }} />
          <span style={styles.statusLabel}>
            {state.serverOnline ? "Connected" : state.serverStarting ? "Starting…" : "Offline"}
          </span>
          <span style={styles.statusUrl}>{state.serverUrl}</span>
        </div>
      </section>

      {/* Quick actions */}
      <section style={styles.card}>
        <h3 style={styles.cardTitle}>Quick Actions</h3>
        <div style={styles.actionsRow}>
          <button
            style={styles.actionBtn}
            disabled={!state.serverOnline}
            onClick={() => dispatch({ type: "SET_MODE", payload: "voice" })}
          >
            🎙 Start Voice
          </button>
          <button
            style={styles.actionBtn}
            disabled={!state.serverOnline}
            onClick={() => dispatch({ type: "SET_SECTION", payload: "chat" })}
          >
            💬 Open Chat
          </button>
        </div>
      </section>

      {/* Recent activity */}
      {recentMessages.length > 0 && (
        <section style={styles.card}>
          <h3 style={styles.cardTitle}>Recent</h3>
          <ul style={styles.activityList}>
            {recentMessages.map((msg) => (
              <li key={msg.id} style={styles.activityItem}>
                <span style={{
                  ...styles.activityRole,
                  color: msg.role === "agent" ? "var(--color-accent)" : "var(--color-text-secondary)",
                }}>
                  {msg.role === "user" ? "You" : "Pond"}
                </span>
                <span style={styles.activityText}>
                  {msg.text.length > 80 ? msg.text.slice(0, 80) + "…" : msg.text}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  card: {
    background: "var(--color-bg)",
    border: "1px solid var(--color-border)",
    borderRadius: "var(--radius-lg)",
    padding: "var(--space-5)",
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-3)",
  },
  cardTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-base)",
    color: "var(--color-text)",
    margin: 0,
  },
  statusRow: { display: "flex", alignItems: "center", gap: "var(--space-2)" },
  statusDot: { width: "8px", height: "8px", borderRadius: "50%", flexShrink: 0 },
  statusLabel: { fontSize: "var(--text-base)", color: "var(--color-text)", fontWeight: 500 },
  statusUrl: { fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)", marginLeft: "auto", fontFamily: "var(--font-mono)" },
  actionsRow: { display: "flex", gap: "var(--space-3)" },
  actionBtn: {
    height: "36px",
    padding: "0 var(--space-5)",
    borderRadius: "var(--radius-md)",
    border: "1px solid var(--color-border-strong)",
    background: "transparent",
    fontFamily: "var(--font-body)",
    fontSize: "var(--text-base)",
    fontWeight: 500,
    color: "var(--color-text)",
    cursor: "pointer",
    display: "inline-flex",
    alignItems: "center",
    gap: "var(--space-2)",
    transition: "background var(--transition-fast)",
  },
  activityList: { listStyle: "none", display: "flex", flexDirection: "column", gap: "var(--space-2)" },
  activityItem: { display: "flex", gap: "var(--space-2)", alignItems: "baseline" },
  activityRole: { fontSize: "var(--text-xs)", fontWeight: 600, fontFamily: "var(--font-display)", textTransform: "uppercase" as const, flexShrink: 0, letterSpacing: "0.04em" },
  activityText: { fontSize: "var(--text-sm)", color: "var(--color-text-secondary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const },
};
