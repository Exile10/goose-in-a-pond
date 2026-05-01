import React from "react";

interface ToggleRowProps {
  icon?: React.ReactNode;
  title: string;
  desc: string;
  checked: boolean;
  onChange: (val: boolean) => void;
  badge?: string;
}

export function ToggleRow({ icon, title, desc, checked, onChange, badge }: ToggleRowProps) {
  return (
    <div className="ob-toggle-row">
      {icon && <span className="ob-toggle-row__icon">{icon}</span>}
      <div className="ob-toggle-row__body">
        <div className="ob-toggle-row__header">
          <strong className="ob-toggle-row__title">{title}</strong>
          {badge && <span className="ob-toggle-row__badge">{badge}</span>}
        </div>
        <p className="ob-toggle-row__desc">{desc}</p>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={`ob-switch ${checked ? "ob-switch--on" : ""}`}
      >
        <span className="ob-switch__thumb" />
      </button>
    </div>
  );
}
