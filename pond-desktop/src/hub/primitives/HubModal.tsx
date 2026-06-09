import { useEffect } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";

interface HubModalProps {
  onClose: () => void;
  wide?: boolean;
  children: React.ReactNode;
}

export function HubModal({ onClose, wide, children }: HubModalProps) {
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);

  return (
    <div className="hubmodal-backdrop" onClick={onClose}>
      <div
        className={`hubmodal${wide ? " hubmodal--wide" : ""}`}
        onClick={(e) => e.stopPropagation()}
      >
        <button className="hubmodal__close" onClick={onClose} aria-label="Close">
          <HubIco d={HP_PATHS.x} size={18} color="currentColor" />
        </button>
        {children}
      </div>
    </div>
  );
}
