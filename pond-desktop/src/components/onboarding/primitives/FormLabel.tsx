import React from "react";

interface FormLabelProps {
  children: React.ReactNode;
  optional?: boolean;
  sub?: string;
}

export function FormLabel({ children, optional, sub }: FormLabelProps) {
  return (
    <div className="ob-form-label">
      <span className="ob-form-label__text">{children}</span>
      {optional && <span className="ob-form-label__hint">(optional)</span>}
      {sub && <span className="ob-form-label__hint">{sub}</span>}
    </div>
  );
}
