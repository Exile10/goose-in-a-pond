import type { ReactNode } from "react";

interface PanelHeadProps {
  title: string;
  action?: ReactNode;
}

export function PanelHead({ title, action }: PanelHeadProps) {
  return (
    <div className="phead">
      <span className="phead__title">{title}</span>
      {action}
    </div>
  );
}
