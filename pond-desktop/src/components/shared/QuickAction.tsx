import { ArrowRight } from "lucide-react";
import type { ReactNode } from "react";

export function QuickAction({ icon, label, onPress }: { icon: ReactNode; label: string; onPress: () => void }) {
  return (
    <button className="quick-action" onClick={onPress}>
      <span className="quick-action__icon">{icon}</span>
      <span className="quick-action__label">{label}</span>
      <span className="quick-action__arrow"><ArrowRight size={14} /></span>
    </button>
  );
}
