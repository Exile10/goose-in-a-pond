import { Bell } from "lucide-react";

interface BellShortcutProps {
  count: number;
  active?: boolean;
  onClick: () => void;
}

export function BellShortcut({ count, active = false, onClick }: BellShortcutProps) {
  return (
    <button
      className="bell-shortcut"
      data-active={active}
      onClick={onClick}
      aria-label={count > 0 ? `Notifications — ${count} unread` : "Notifications"}
    >
      <Bell size={20} color={active ? "var(--pp)" : "var(--color-text-tertiary)"} strokeWidth={2} />
      {count > 0 && (
        <span className="bell-shortcut__count" aria-hidden="true">
          {count > 99 ? "99+" : count}
        </span>
      )}
    </button>
  );
}
