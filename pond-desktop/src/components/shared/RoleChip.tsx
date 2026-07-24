import type { ReactNode } from "react";

// Callers only render this once a role actually has a model assigned (see
// Dashboard.tsx's empty-state branch), so it's always in the "set" visual
// state — missing that modifier left the header text stuck on the
// low-contrast placeholder color even though a real model was showing.
export function RoleChip({ role, model, color, icon }: { role: string; model: string; color: string; icon: ReactNode }) {
  return (
    <div className={`role-chip role-chip--${color} is-set`}>
      <div className="role-chip__bar" />
      <div className="role-chip__body">
        <div className="role-chip__head">
          {icon}
          <span className="role-chip__role">{role}</span>
        </div>
        <span className="role-chip__model role-chip__value role-chip__value--set">{model}</span>
      </div>
    </div>
  );
}
