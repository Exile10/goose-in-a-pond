import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useDialogFocusTrap } from "../../components/shared";

interface HubModalProps {
  /** Accessible name for the dialog (e.g. "Create a routine", "Camera feed"). */
  label: string;
  onClose: () => void;
  wide?: boolean;
  children: React.ReactNode;
}

export function HubModal({ label, onClose, wide, children }: HubModalProps) {
  const dialogRef = useDialogFocusTrap<HTMLDivElement>(true, onClose);

  return (
    <div className="hubmodal-backdrop" onClick={onClose}>
      <div
        ref={dialogRef}
        className={`hubmodal${wide ? " hubmodal--wide" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
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
