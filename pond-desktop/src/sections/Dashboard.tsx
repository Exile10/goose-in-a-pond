import { useEffect, useState } from "react";
import { Button, Card } from "@heroui/react";
import { Mic, MessageSquare } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { AudioWaves } from "../components/AudioWaves";
import { api } from "../api/PondApiClient";
import type { SessionSummary } from "../api/types";

function abbreviateModel(name: string): string {
  // "org/model-name" → "model-name"; truncate to 32 chars
  const bare = name.includes("/") ? name.split("/").pop() ?? name : name;
  return bare.length > 32 ? bare.slice(0, 29) + "…" : bare;
}

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10000) return "~" + (n / 1000).toFixed(1) + "k";
  return "~" + Math.round(n / 1000) + "k";
}

function roleColor(role: string): React.CSSProperties {
  if (role === "think") return { background: "rgba(255,149,0,0.12)", color: "#b36200", borderColor: "rgba(255,149,0,0.3)" };
  if (role === "task")  return { background: "rgba(52,199,89,0.12)",  color: "#1a7a3a", borderColor: "rgba(52,199,89,0.3)" };
  // chat (default) — purple accent
  return { background: "rgba(140,75,255,0.10)", color: "var(--color-accent)", borderColor: "rgba(140,75,255,0.25)" };
}

export function Dashboard() {
  const state    = useAppState();
  const dispatch = useAppDispatch();

  const recentMessages = state.transcript.slice(-5).reverse();

  // Recent text-chat sessions. The voice transcript above only captures
  // wake-word / mic conversations; the typed Chat section persists into
  // the backend's session store. We pull the 5 most recent and render
  // them as clickable items that take the user straight to chat.
  const [recentSessions, setRecentSessions] = useState<SessionSummary[]>([]);
  useEffect(() => {
    // Wait for both the connection AND the handshake-issued bearer token —
    // listSessions is gated by the auth middleware, so calling it before
    // the token is set just produces a silent 401 and an empty Recents card.
    if (!state.serverOnline || !state.sessionToken) return;
    let cancelled = false;
    api.listSessions()
      .then((sessions) => {
        if (cancelled) return;
        const sorted = [...sessions]
          .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
          .slice(0, 5);
        setRecentSessions(sorted);
      })
      .catch((err) => {
        // Non-fatal — Recent card just stays hidden.
        console.warn("Dashboard: listSessions failed:", err);
      });
    return () => { cancelled = true; };
  }, [state.serverOnline, state.sessionToken]);

  function openSession(id: string) {
    dispatch({ type: "SET_SESSION_ID", payload: id });
    dispatch({ type: "SET_SECTION", payload: "chat" });
  }

  function relativeTime(iso: string): string {
    const then = Date.parse(iso);
    if (Number.isNaN(then)) return "";
    const diff = Date.now() - then;
    const min = Math.round(diff / 60_000);
    if (min < 1)  return "just now";
    if (min < 60) return `${min} min ago`;
    const h = Math.round(min / 60);
    if (h < 24) return `${h} h ago`;
    const d = Math.round(h / 24);
    return `${d} day${d === 1 ? "" : "s"} ago`;
  }

  return (
    <div style={styles.root}>

      {/* ── Voice — primary home action ───────────────────── */}
      <Card style={styles.voiceCard}>
        <div style={styles.voiceCardBody}>
          <div style={styles.voiceCardLeft}>
            <h3 style={styles.voiceCardTitle}>Voice Mode</h3>
            <p style={styles.voiceCardSub}>
              Talk to Pond hands-free. Uses your mic, whisper transcription, and a local TTS voice.
            </p>
            <p style={styles.voiceShortcut}>
              Press <kbd style={styles.kbd}>⌘⇧V</kbd> or <kbd style={styles.kbd}>Ctrl+Shift+V</kbd> from anywhere to activate
            </p>
            <Button
              variant="primary"
              isDisabled={!state.serverOnline}
              onPress={() => dispatch({ type: "SET_MODE", payload: "voice" })}
              style={styles.voiceBtn}
            >
              <Mic size={15} /> Open Voice Mode
            </Button>
          </div>
          <div style={styles.voiceWavePreview}>
            <AudioWaves
              state={state.voiceState}
              audioLevel={0}
              size="sm"
            />
            <span style={styles.voiceStateBadge}>{state.voiceState}</span>
          </div>
        </div>
      </Card>

      {/* ── Server status ─────────────────────────────────── */}
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

      {/* ── Last response metadata ────────────────────────── */}
      {state.lastResponseMeta && (
        <Card>
          <div style={styles.cardBody}>
            <h3 style={styles.cardTitle}>Last Response</h3>
            <div style={styles.metaRow}>
              <span style={styles.metaModel}>
                {abbreviateModel(state.lastResponseMeta.modelName)}
              </span>
              <span style={{ ...styles.metaRolePill, ...roleColor(state.lastResponseMeta.modelRole) }}>
                {state.lastResponseMeta.modelRole}
              </span>
              <span style={styles.metaTokens}>
                {formatTokens(state.lastResponseMeta.completionTokens)} tokens
              </span>
            </div>
          </div>
        </Card>
      )}

      {/* ── Quick actions ─────────────────────────────────── */}
      <Card>
        <div style={styles.cardBody}>
          <h3 style={styles.cardTitle}>Quick Actions</h3>
          <div style={styles.actionsRow}>
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

      {/* ── Recent activity ─────────────────────────────────
       * Always rendered when the server is online: shows recent typed-chat
       * sessions (clickable, jump back into the conversation) and any
       * voice transcript lines from the current session. Replaces the old
       * "only show when voice transcript is non-empty" behaviour, which
       * left the dashboard with no Recent card on a fresh launch. */}
      {state.serverOnline && (recentSessions.length > 0 || recentMessages.length > 0) && (
        <Card>
          <div style={styles.cardBody}>
            <h3 style={styles.cardTitle}>Recent</h3>

            {recentSessions.length > 0 && (
              <ul style={styles.activityList}>
                {recentSessions.map((s) => {
                  const title =
                    s.title?.trim() ||
                    `Conversation from ${new Date(s.created_at).toLocaleString(undefined, {
                      month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
                    })}`;
                  return (
                    <li
                      key={s.id}
                      style={{ ...styles.activityItem, cursor: "pointer" }}
                      onClick={() => openSession(s.id)}
                      role="button"
                      aria-label={`Open ${title}`}
                    >
                      <MessageSquare size={12} style={{ color: "var(--color-accent)", flexShrink: 0 }} />
                      <span style={{ ...styles.activityText, flex: 1, minWidth: 0 }}>
                        {title.length > 60 ? title.slice(0, 60) + "…" : title}
                      </span>
                      <span style={{ fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", flexShrink: 0 }}>
                        {relativeTime(s.updated_at)}
                      </span>
                    </li>
                  );
                })}
              </ul>
            )}

            {recentMessages.length > 0 && (
              <>
                {recentSessions.length > 0 && (
                  <div style={{ height: 1, background: "var(--color-border)", margin: "8px 0" }} />
                )}
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
              </>
            )}
          </div>
        </Card>
      )}
    </div>
  );
}

const styles: Record<string, React.CSSProperties> = {
  root: { display: "flex", flexDirection: "column", gap: "var(--space-4)", maxWidth: "var(--content-max-width)" },

  // Voice card
  voiceCard: {
    background: "linear-gradient(135deg, rgba(140,82,255,0.07) 0%, rgba(140,82,255,0.02) 100%)",
    border: "1px solid rgba(140,82,255,0.20)",
  },
  voiceCardBody: {
    padding: "var(--space-5)",
    display: "flex",
    alignItems: "center",
    gap: "var(--space-5)",
  },
  voiceCardLeft: {
    flex: 1,
    display: "flex",
    flexDirection: "column",
    gap: "var(--space-2)",
  },
  voiceCardTitle: {
    fontFamily: "var(--font-display)",
    fontWeight: 700,
    fontSize: "var(--text-lg)",
    color: "var(--color-text)",
    margin: 0,
  },
  voiceCardSub: {
    fontSize: "var(--text-sm)",
    color: "var(--color-text-secondary)",
    margin: 0,
    lineHeight: "1.5",
  },
  voiceShortcut: {
    fontSize: "var(--text-xs)",
    color: "var(--color-text-tertiary)",
    margin: 0,
  },
  voiceBtn: {
    marginTop: "var(--space-2)",
    alignSelf: "flex-start",
    display: "flex",
    alignItems: "center",
    gap: "6px",
  },
  voiceWavePreview: {
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    gap: "6px",
    width: "120px",
    flexShrink: 0,
  },
  voiceStateBadge: {
    fontSize: "10px",
    color: "var(--color-text-tertiary)",
    textTransform: "uppercase" as const,
    letterSpacing: "0.06em",
    fontFamily: "var(--font-mono)",
  },

  // Standard cards
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

  // Last response metadata
  metaRow: { display: "flex", alignItems: "center", gap: "var(--space-2)", flexWrap: "wrap" as const },
  metaModel: { fontFamily: "var(--font-mono)", fontSize: "var(--text-sm)", color: "var(--color-text)", fontWeight: 500 },
  metaRolePill: { fontSize: "var(--text-xs)", fontWeight: 600, padding: "2px 8px", borderRadius: "var(--radius-lg)", border: "1px solid", letterSpacing: "0.04em", textTransform: "uppercase" as const, fontFamily: "var(--font-mono)" },
  metaTokens: { fontSize: "var(--text-xs)", color: "var(--color-text-tertiary)", marginLeft: "auto", fontFamily: "var(--font-mono)" },
  kbd: {
    display: "inline-block",
    fontFamily: "var(--font-mono)",
    fontSize: "9px",
    background: "rgba(23,22,22,0.07)",
    border: "1px solid rgba(23,22,22,0.14)",
    borderRadius: "4px",
    padding: "0px 4px",
    color: "var(--color-text-secondary)",
  },
};
