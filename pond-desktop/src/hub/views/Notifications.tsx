import { useState, useEffect, useCallback } from "react";
import { Bell, Calendar, Shield, Camera, Sparkles, BatteryLow } from "lucide-react";
import { useAppState } from "../../state/AppContext";
import { api } from "../../api/PondApiClient";
import type { ScheduleRun } from "../../api/types";
import {
  MOCK_NOTIFICATIONS,
  CATEGORY_COLOR,
  groupNotifications,
  relativeTime,
} from "../data/notifications";
import type {
  Notification,
  NotificationCategory,
  NotificationGroup,
} from "../data/notifications";
import "./notifications.css";

// ─── Category icon map ────────────────────────────────────────────────────────
const CATEGORY_ICON: Record<NotificationCategory, React.ReactNode> = {
  schedule: <Calendar size={20} strokeWidth={2} />,
  security: <Shield size={20} strokeWidth={2} />,
  camera:   <Camera size={20} strokeWidth={2} />,
  routine:  <Sparkles size={20} strokeWidth={2} />,
  battery:  <BatteryLow size={20} strokeWidth={2} />,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────
function buildScheduleNotification(
  run: ScheduleRun & { schedule_name: string },
): Notification {
  return {
    id: `sched-run-${run.id}`,
    category: "schedule",
    title: `${run.schedule_name} ran`,
    body:
      run.status === "failed"
        ? `Failed${run.error ? ` — ${run.error.slice(0, 80)}` : ""}`
        : run.result
        ? run.result.slice(0, 100)
        : `Completed in ${run.duration_ms ? `${Math.round(run.duration_ms / 1000)}s` : "unknown time"}`,
    timestamp: run.started_at,
    read: true,
    action: { label: "View on Canvas", route: "canvas" },
  };
}

function countUnread(items: Notification[]): number {
  return items.filter((n) => !n.read).length;
}

// ─── Notification card ────────────────────────────────────────────────────────
interface NCardProps {
  notification: Notification;
  onAction?: (route: string) => void;
}

function NCard({ notification: n, onAction }: NCardProps) {
  const color = CATEGORY_COLOR[n.category];
  const now = new Date();

  return (
    <article
      className="ncard"
      data-read={String(n.read)}
      role="article"
      aria-label={n.title}
      onClick={() => n.action && onAction?.(n.action.route)}
      onKeyDown={(e) => {
        if ((e.key === "Enter" || e.key === " ") && n.action) {
          e.preventDefault();
          onAction?.(n.action.route);
        }
      }}
      tabIndex={0}
    >
      <span
        className="ncard__icon"
        style={{ background: color.bg, color: color.fg }}
        aria-hidden="true"
      >
        {CATEGORY_ICON[n.category]}
      </span>
      <div className="ncard__body">
        <div className="ncard__title">{n.title}</div>
        <div className="ncard__sub">{n.body}</div>
      </div>
      <div className="ncard__right">
        <span className="ncard__time">{relativeTime(n.timestamp, now)}</span>
        {n.action && (
          <button
            className="ncard__cta"
            onClick={(e) => {
              e.stopPropagation();
              n.action && onAction?.(n.action.route);
            }}
          >
            {n.action.label}
          </button>
        )}
      </div>
      {!n.read && <span className="ncard__dot" aria-hidden="true" />}
    </article>
  );
}

// ─── Notifications view ───────────────────────────────────────────────────────
interface NotificationsViewProps {
  go?: (route: string) => void;
}

export function NotificationsView({ go }: NotificationsViewProps) {
  const { serverOnline } = useAppState();

  // Merge mock + schedule-run notifications
  const [allItems, setAllItems] = useState<Notification[]>([...MOCK_NOTIFICATIONS]);
  const [scheduleOffline, setScheduleOffline] = useState(false);
  const [readIds, setReadIds] = useState<Set<string>>(new Set());

  // Fetch recent schedule runs when server is online
  useEffect(() => {
    if (!serverOnline) {
      setScheduleOffline(true);
      return;
    }
    setScheduleOffline(false);

    let cancelled = false;
    async function load() {
      try {
        const runs = await api.getAllRecentRuns(5);
        if (cancelled) return;
        const scheduleNotifs: Notification[] = runs.map(buildScheduleNotification);
        // Merge: scheduleNotifs + mock, sorted newest first
        setAllItems(
          [...scheduleNotifs, ...MOCK_NOTIFICATIONS].sort(
            (a, b) =>
              new Date(b.timestamp).getTime() - new Date(a.timestamp).getTime(),
          ),
        );
      } catch {
        if (!cancelled) setScheduleOffline(true);
      }
    }

    void load();
    return () => { cancelled = true; };
  }, [serverOnline]);

  // Derive effective items with local read state overlaid
  const effectiveItems: Notification[] = allItems.map((n) =>
    readIds.has(n.id) ? { ...n, read: true } : n,
  );

  const unreadCount = countUnread(effectiveItems);
  const allRead = unreadCount === 0;

  const markAllRead = useCallback(() => {
    setReadIds(new Set(allItems.map((n) => n.id)));
  }, [allItems]);

  const groups: NotificationGroup[] = groupNotifications(effectiveItems);

  function handleAction(route: string) {
    go?.(route);
  }

  return (
    <div className="nfeed">
      {/* head */}
      <div className="nfeed__head">
        <div>
          <div className="nfeed__title">Notifications</div>
          <div className="nfeed__sub">
            {unreadCount > 0
              ? `${unreadCount} unread`
              : "You're all caught up"}
          </div>
        </div>
        <button
          className="nfeed__mark-all"
          onClick={markAllRead}
          disabled={allRead}
        >
          Mark all read
        </button>
      </div>

      {/* schedule offline note */}
      {scheduleOffline && (
        <div className="nfeed__offline-note">
          Schedule history unavailable — connect to Goose server to see run debriefs
        </div>
      )}

      {/* empty state */}
      {groups.length === 0 && (
        <div className="nfeed__empty">
          <div className="nfeed__empty-icon">
            <Bell size={24} color="var(--pp)" strokeWidth={2} />
          </div>
          <div className="nfeed__empty-title">Nothing here yet</div>
          <div className="nfeed__empty-body">
            Notifications from Goose, your devices, and schedules will appear here.
          </div>
        </div>
      )}

      {/* groups */}
      {groups.map((group) => (
        <div key={group.label} className="nfeed__group">
          <div className="nfeed__group-label">{group.label}</div>
          {group.items.map((n) => (
            <NCard key={n.id} notification={n} onAction={handleAction} />
          ))}
        </div>
      ))}
    </div>
  );
}
