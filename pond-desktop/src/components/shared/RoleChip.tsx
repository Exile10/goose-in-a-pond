import type { ReactNode } from "react";

export function RoleChip({ role, model, color, icon }: { role: string; model: string; color: string; icon: ReactNode }) {
  return (
    <div className={`role-chip role-chip--${color}`}>
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
