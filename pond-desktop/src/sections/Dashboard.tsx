import { useState, useEffect } from "react";
import { Button, Card, CardContent, Chip, Switch } from "@heroui/react";
import {
  Mic,
  Settings,
  MessageCircle,
  Box,
  PenLine,
  CalendarClock,
  Coins,
  RefreshCw,
  Cpu,
  Headphones,
  Wrench,
} from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { api } from "../api/PondApiClient";
import { NotificationsPanel } from "../components/NotificationsPanel";
import { PageHeader, Metric, QuickAction, RoleChip } from "../components/shared";
import type {
  SessionSummary,
  UsageSummary,
  ModelActiveRoles,
  ScheduleRunNotification,
} from "../api/types";

/* ── Helpers ───────────────────────────────────────────────────────────────── */

function abbreviateModel(name: string): string {
  const bare = name.includes("/") ? name.split("/").pop() ?? name : name;
  return bare.length > 28 ? bare.slice(0, 25) + "\u2026" : bare;
}

function formatTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10000) return `~${(n / 1000).toFixed(1)}k`;
  return `~${Math.round(n / 1000)}k`;
}

function formatDollars(n: number): string {
  if (n < 0.01) return "<$0.01";
  return `$${n.toFixed(2)}`;
}

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

/* ── VU Meter ──────────────────────────────────────────────────────────────── */

const VU_HEIGHTS = [8, 14, 20, 28, 20, 14, 8];

function VuMeter({ level, active }: { level: number; active: boolean }) {
  const lit = Math.round(level * VU_HEIGHTS.length);
  return (
    <div className="vu" data-active={active}>
      {VU_HEIGHTS.map((h, i) => (
        <span
          key={i}
          className={`vu__bar${i < lit ? " is-lit" : ""}`}
          style={{ height: h }}
        />
      ))}
    </div>
  );
}

/* ── Component ─────────────────────────────────────────────────────────────── */

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();

  const [recentSessions, setRecentSessions] = useState<SessionSummary[]>([]);
  const [usageData, setUsageData] = useState<UsageSummary | null>(null);
  const [activeRoles, setActiveRoles] = useState<ModelActiveRoles | null>(null);
  const [rolesLoading, setRolesLoading] = useState(false);

  useEffect(() => {
    if (!state.serverOnline || !state.sessionToken) return;
    let cancelled = false;

    api.listSessions()
      .then((sessions) => {
        if (cancelled) return;
        setRecentSessions(
          [...sessions]
            .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
            .slice(0, 4),
        );
      })
      .catch(() => {});

    const fetchUsage = () => {
      api.getUsageSummary()
        .then((data) => { if (!cancelled) setUsageData(data); })
        .catch(() => {});
      api.listSessions()
        .then((sessions) => {
          if (cancelled) return;
          setRecentSessions(
            [...sessions]
              .sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1))
              .slice(0, 4),
          );
        })
        .catch(() => {});
    };
    fetchUsage();

    // Poll every 15s so usage stays live while Dashboard is visible
    const pollId = setInterval(fetchUsage, 15_000);

    setRolesLoading(true);
    api.getActiveRoles()
      .then((roles) => { if (!cancelled) setActiveRoles(roles); })
      .catch(() => {})
      .finally(() => { if (!cancelled) setRolesLoading(false); });

    return () => { cancelled = true; clearInterval(pollId); };
  }, [state.serverOnline, state.sessionToken]);

  function openSession(id: string) {
    dispatch({ type: "SET_SESSION_ID", payload: id });
    dispatch({ type: "SET_SECTION", payload: "chat" });
  }

  function handleOpenDebrief(run: ScheduleRunNotification) {
    dispatch({ type: "MARK_RUN_READ", payload: run.id });
    dispatch({ type: "SET_DEBRIEF_CONTEXT", payload: { type: "debrief", run } });
    dispatch({ type: "SET_SECTION", payload: "canvas" });
  }

  /* VU meter — animates only while voice is active */
  const [vuLevel, setVuLevel] = useState(0);
  const voiceActive =
    state.voiceState === "recording" ||
    state.voiceState === "wait" ||
    state.voiceState === "thinking" ||
    state.voiceState === "speaking";

  useEffect(() => {
    if (!voiceActive) { setVuLevel(0); return; }
    const id = setInterval(
      () => setVuLevel(Math.random() * 0.5 + 0.15),
      220,
    );
    return () => clearInterval(id);
  }, [voiceActive]);

  const voiceStateLabel = voiceActive
    ? state.voiceState.toUpperCase()
    : "IDLE";

  /* Usage calculations */
  const inputPrice  = usageData?.cloud_input_price_per_million  ?? 2.50;
  const outputPrice = usageData?.cloud_output_price_per_million ?? 10.00;
  const saved = usageData
    ? (usageData.total_prompt_tokens / 1_000_000) * inputPrice
      + (usageData.total_completion_tokens / 1_000_000) * outputPrice
    : 0;

  /* Active model roles — derive display list */
  const roleItems = activeRoles
    ? [
        { role: "Chat",  model: activeRoles.chat?.model    ?? "—", color: "secondary", icon: <MessageCircle size={13} /> },
        { role: "Think", model: activeRoles.tool?.model    ?? "—", color: "warning",   icon: <Cpu size={13} /> },
        { role: "Task",  model: activeRoles.tool?.model    ?? "—", color: "primary",   icon: <Wrench size={13} /> },
        { role: "ASR",   model: activeRoles.asr?.model     ?? "—", color: "success",   icon: <Mic size={13} /> },
        { role: "TTS",   model: activeRoles.tts?.model     ?? "—", color: "danger",    icon: <Headphones size={13} /> },
      ]
    : [];

  /* ── Render ── */
  return (
    <div className="screen screen--dashboard">
      {/* ── Page header ── */}
      <PageHeader title="Dashboard" />

      {/* ── Notifications (card--notifs) ── */}
      <NotificationsPanel onOpenDebrief={handleOpenDebrief} />

      {/* ── Voice Mode (card--voice) ── */}
      <Card className="card card--voice">
        <CardContent>
          <div className="voice-card">
            <div className="voice-card__main">
              <div className="voice-card__icon">
                <Mic size={20} />
              </div>
              <div className="voice-card__text">
                <h3>Voice Mode</h3>
                <p>
                  Talk to Pond hands-free. Uses your mic, Whisper transcription,
                  and a local TTS voice.
                </p>
                <div className="voice-card__hint">
                  Press <kbd>&#8984;&#8679;V</kbd> or <kbd>Ctrl+Shift+V</kbd>{" "}
                  from anywhere to activate.
                </div>
              </div>
            </div>

            <div className="voice-card__right">
              <VuMeter level={vuLevel} active={voiceActive} />
              <span className="voice-card__state">{voiceStateLabel}</span>
              <label className="voice-card__toggle">
                <span className="voice-card__switch-label">Hands-free</span>
                <Switch
                  size="sm"
                  // @ts-expect-error HeroUI Switch accepts color but types don't expose it
                  color="secondary"
                  isSelected={voiceActive}
                  onValueChange={() => {
                    if (voiceActive)
                      dispatch({ type: "SET_VOICE_STATE", payload: "idle" });
                    else dispatch({ type: "VOICE_ACTIVATE" });
                  }}
                >
                  <Switch.Control>
                    <Switch.Thumb />
                  </Switch.Control>
                </Switch>
              </label>
            </div>
          </div>

          <div className="voice-card__cta">
            <Button
              size="sm"
              variant="secondary"
              isDisabled={!state.serverOnline}
              onPress={() => dispatch({ type: "SET_MODE", payload: "voice" })}
            >
              <Mic size={14} /> Open Voice Mode
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onPress={() =>
                dispatch({ type: "SET_SECTION", payload: "settings" })
              }
            >
              <Settings size={14} /> Voice settings
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* ── Usage & Savings (usage-card classes) ── */}
      <div className="usage-card">
        <div className="usage-card__head">
          <span className="card__label">Usage &amp; Savings</span>
          <Chip size="sm" variant="soft">This session</Chip>
        </div>

        <div className="usage-card__stats">
          <div className="usage-stat">
            <div className="usage-stat__num">
              {formatTokens(usageData?.total_tokens ?? 0)}
            </div>
            <div className="usage-stat__label">Tokens total</div>
          </div>
          <div className="usage-stat">
            <div className="usage-stat__num">{usageData?.session_count ?? 0}</div>
            <div className="usage-stat__label">Sessions</div>
          </div>
        </div>

        <div className="usage-savings">
          <div className="usage-savings__icon">
            <Coins size={16} />
          </div>
          <div>
            <div className="usage-savings__amount">
              ~{formatDollars(saved)} saved
            </div>
            <div className="usage-savings__sub">
              vs cloud API (${inputPrice}/M input + ${outputPrice}/M output)
            </div>
          </div>
        </div>

        {recentSessions.length > 0 && (
          <div className="dash-recent">
            <div className="card__label dash-recent__label">
              Recent conversations
            </div>
            <div className="quick-actions">
              {recentSessions.map((s) => (
                <button
                  key={s.id}
                  className="quick-action"
                  onClick={() => openSession(s.id)}
                >
                  <span className="quick-action__icon">
                    <MessageCircle size={14} />
                  </span>
                  <span className="quick-action__label quick-action__label--ellipsis">
                    {s.title || `Session ${s.id.slice(0, 8)}`}
                  </span>
                  <span className="quick-action__time">
                    {timeAgo(s.updated_at)}
                  </span>
                  <span className="quick-action__arrow" />
                </button>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* ── dash-grid: Server Status + Quick Actions ── */}
      <div className="dash-grid">
        {/* Server Status */}
        <Card className="card card--server">
          <CardContent>
            <div className="server-row">
              <div>
                <div className="card__label">Server status</div>
                <div className="server-row__state">
                  <span
                    className="server-row__dot"
                    style={{
                      background: state.serverOnline
                        ? "var(--color-success, #17c964)"
                        : state.serverStarting
                        ? "#f5a524"
                        : "var(--grey-400)",
                      boxShadow: state.serverOnline
                        ? "0 0 0 3px rgba(23,201,100,0.18)"
                        : "none",
                    }}
                  />
                  <span>
                    {state.serverOnline
                      ? "Connected"
                      : state.serverStarting
                      ? "Starting\u2026"
                      : "Offline"}
                  </span>
                </div>
              </div>
              <code className="server-row__url">
                {state.serverUrl || "http://127.0.0.1:4000"}
              </code>
            </div>

            <div className="card__divider" />

            <div className="metrics">
              <Metric
                label="Uptime"
                value={state.serverOnline ? "Online" : "--"}
                trend={state.serverOnline ? "ok" : undefined}
              />
              <Metric
                label="Latency"
                value={state.serverOnline ? "<10ms" : "--"}
                trend={state.serverOnline ? "ok" : undefined}
              />
              <Metric
                label="Memory"
                value={state.serverOnline ? "Normal" : "--"}
              />
              <Metric
                label="Active model"
                value={
                  state.lastResponseMeta
                    ? abbreviateModel(state.lastResponseMeta.modelName)
                    : state.serverOnline
                    ? "gemma-4-E4B"
                    : "--"
                }
              />
            </div>
          </CardContent>
        </Card>

        {/* Quick Actions */}
        <Card className="card card--actions">
          <CardContent>
            <div className="card__label">Quick actions</div>
            <div className="quick-actions">
              <QuickAction
                icon={<MessageCircle size={16} />}
                label="Open chat"
                onPress={() =>
                  dispatch({ type: "SET_SECTION", payload: "chat" })
                }
              />
              <QuickAction
                icon={<Box size={16} />}
                label="Manage models"
                onPress={() =>
                  dispatch({ type: "SET_SECTION", payload: "models" })
                }
              />
              <QuickAction
                icon={<PenLine size={16} />}
                label="Edit prompts"
                onPress={() =>
                  dispatch({ type: "SET_SECTION", payload: "prompts" })
                }
              />
              <QuickAction
                icon={<CalendarClock size={16} />}
                label="Schedules"
                onPress={() =>
                  dispatch({ type: "SET_SECTION", payload: "schedules" })
                }
              />
            </div>
          </CardContent>
        </Card>
      </div>

      {/* ── Active model roles ── */}
      <Card className="card card--roles">
        <div className="card-header">
          <div className="card__label">Active model roles</div>
          <Button
            size="sm"
            variant="ghost"
            isIconOnly
            onPress={() => {
              setRolesLoading(true);
              api.getActiveRoles()
                .then((r) => setActiveRoles(r))
                .catch(() => {})
                .finally(() => setRolesLoading(false));
            }}
            aria-label="Refresh model roles"
          >
            <RefreshCw size={14} />
          </Button>
        </div>
        <CardContent>
          {rolesLoading ? (
            <p className="dash-role-text">Loading\u2026</p>
          ) : roleItems.length > 0 ? (
            <div className="role-grid">
              {roleItems.map((r) => (
                <RoleChip
                  key={r.role}
                  role={r.role}
                  model={r.model}
                  color={r.color}
                  icon={r.icon}
                />
              ))}
            </div>
          ) : (
            <p className="dash-role-text">
              {state.serverOnline
                ? "No roles assigned yet. Configure models in Settings."
                : "Connect to the server to see active roles."}
            </p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
