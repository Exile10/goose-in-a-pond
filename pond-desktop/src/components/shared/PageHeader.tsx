import type { ReactNode } from "react";

export function PageHeader({ title, action }: { title: string; action?: ReactNode }) {
  return (
    <div className="page-header">
      <h1 className="page-header__title">{title}</h1>
      {action && <div className="page-header__action">{action}</div>}
    </div>
  );
}
