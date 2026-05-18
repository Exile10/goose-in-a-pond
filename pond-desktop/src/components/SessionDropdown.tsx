import { useState, useRef, useEffect } from "react";
import { Button } from "@heroui/react";
import { Plus, MessageSquare } from "lucide-react";
import type { SessionSummary } from "../api/types";

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h`;
  const days = Math.floor(hrs / 24);
  return `${days}d`;
}

function groupSessions(sessions: SessionSummary[]) {
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterdayStart = todayStart - 86400000;

  const today: SessionSummary[] = [];
  const yesterday: SessionSummary[] = [];
  const older: SessionSummary[] = [];

  for (const s of sessions) {
    const t = new Date(s.updated_at).getTime();
    if (t >= todayStart) today.push(s);
    else if (t >= yesterdayStart) yesterday.push(s);
    else older.push(s);
  }

  return { today, yesterday, older };
}

interface Props {
  sessions: SessionSummary[];
  currentSessionId: string | null;
  onSelect: (id: string) => void;
  onNewChat: () => void;
  isOpen: boolean;
  onClose: () => void;
}

export function SessionDropdown({ sessions, currentSessionId, onSelect, onNewChat, isOpen, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        onClose();
      }
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const { today, yesterday, older } = groupSessions(sessions);

  function renderItem(s: SessionSummary) {
    const title = s.title?.trim() || `Session ${s.id.slice(0, 8)}`;
    const tokens = (s.total_prompt_tokens ?? 0) + (s.total_completion_tokens ?? 0);
    const isActive = s.id === currentSessionId;
    return (
      <button
        key={s.id}
        className={`session-dropdown__item${isActive ? " is-active" : ""}`}
        onClick={() => { onSelect(s.id); onClose(); }}
      >
        <MessageSquare size={13} style={{ flexShrink: 0, opacity: 0.5 }} />
        <span className="session-dropdown__title">{title}</span>
        <span className="session-dropdown__meta">
          {tokens > 0 && <span>{tokens > 1000 ? `${(tokens / 1000).toFixed(1)}k` : tokens}</span>}
          <span>{timeAgo(s.updated_at)}</span>
        </span>
      </button>
    );
  }

  function renderGroup(label: string, items: SessionSummary[]) {
    if (items.length === 0) return null;
    return (
      <div className="session-dropdown__group">
        <div className="session-dropdown__group-label">{label}</div>
        {items.map(renderItem)}
      </div>
    );
  }

  return (
    <div ref={ref} className="session-dropdown">
      <div className="session-dropdown__header">
        <span style={{ fontWeight: 600, fontSize: 13 }}>Conversations</span>
        <Button size="sm" variant="ghost" onPress={() => { onNewChat(); onClose(); }}>
          <Plus size={14} /> New
        </Button>
      </div>
      <div className="session-dropdown__list">
        {sessions.length === 0 ? (
          <div style={{ padding: "16px 12px", color: "var(--grey-400)", fontSize: 13, textAlign: "center" }}>
            No conversations yet
          </div>
        ) : (
          <>
            {renderGroup("Today", today)}
            {renderGroup("Yesterday", yesterday)}
            {renderGroup("Older", older)}
          </>
        )}
      </div>
    </div>
  );
}
