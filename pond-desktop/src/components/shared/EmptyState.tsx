import type { ReactNode } from "react";

export function EmptyState({ icon, children, inline }: { icon: ReactNode; children: ReactNode; inline?: boolean }) {
  return (
    <div className={`empty-state${inline ? " empty-state--inline" : ""}`}>
      {icon}
      <span>{children}</span>
    </div>
  );
}
