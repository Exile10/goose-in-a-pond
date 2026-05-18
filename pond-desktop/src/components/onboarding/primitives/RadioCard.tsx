import React from "react";

interface RadioCardProps {
  selected: boolean;
  onClick: () => void;
  children: React.ReactNode;
  className?: string;
}

export function RadioCard({ selected, onClick, children, className = "" }: RadioCardProps) {
  return (
    <div
      onClick={onClick}
      className={`ob-radio-card ${selected ? "ob-radio-card--selected" : ""} ${className}`}
      role="radio"
      aria-checked={selected}
      tabIndex={0}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onClick(); } }}
    >
      <div className="ob-radio-card__dot" />
      {children}
    </div>
  );
}
