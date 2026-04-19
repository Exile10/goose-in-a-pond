import { Button, Card } from "@heroui/react";
import { Mic, MessageSquare } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";

export function Dashboard() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  const recentMessages = state.transcript.slice(-5).reverse();

  return (
    <div style={styles.root}>
      {/* Server status card */}
      <Card>
        <div style={styles.cardBody}>
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
        </div>
      </Card>

      {/* Quick actions */}
      <Card>
        <div style={styles.cardBody}>
          <h3 style={styles.cardTitle}>Quick Actions</h3>
          <div style={styles.actionsRow}>
            <Button
              variant="outline"
              isDisabled={!state.serverOnline}
              onPress={() => dispatch({ type: "SET_MODE", payload: "voice" })}
            >
              <Mic size={14} /> Start Voice
            </Button>
            <Button
              variant="outline"
              isDisabled={!state.serverOnline}
              onPress={() => dispatch({ type: "SET_SECTION", payload: "chat" })}
            >
              <MessageSquare size={14} /> Open Chat
            </Button>
          </div>
        </div>
      </Card>

      {/* Recent activity */}
      {recentMessages.length > 0 && (
        <Card>
          <div style={styles.cardBody}>
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
          </div>
        </Card>
      )}
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },
  cardBody: {
    padding: "var(--space-4)",
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
  activityList: { listStyle: "none", display: "flex", flexDirection: "column", gap: "var(--space-2)" },
  activityItem: { display: "flex", gap: "var(--space-2)", alignItems: "baseline" },
  activityRole: { fontSize: "var(--text-xs)", fontWeight: 600, fontFamily: "var(--font-display)", textTransform: "uppercase" as const, flexShrink: 0, letterSpacing: "0.04em" },
  activityText: { fontSize: "var(--text-sm)", color: "var(--color-text-secondary)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" as const },
};
