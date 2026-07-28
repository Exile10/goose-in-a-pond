import { X } from "lucide-react";
import { MAX_IMAGES_PER_TURN } from "../lib/imageAttach";
import type { PreparedImage } from "../lib/imageAttach";

interface AttachmentTrayProps {
  attachments: PreparedImage[];
  onRemove: (index: number) => void;
}

/** Pending-image strip shown above the chat input row. Renders nothing when empty. */
export function AttachmentTray({ attachments, onRemove }: AttachmentTrayProps) {
  if (attachments.length === 0) return null;

  return (
    <div className="attachment-tray" role="list" aria-label="Attached images">
      {attachments.map((att, i) => (
        <div key={i} className="attachment-tray__item" role="listitem">
          <img src={att.previewUrl} alt={`Attached image ${i + 1}`} className="attachment-tray__thumb" />
          <button
            type="button"
            className="attachment-tray__remove"
            onClick={() => onRemove(i)}
            aria-label={`Remove attached image ${i + 1}`}
          >
            <X size={12} />
          </button>
        </div>
      ))}
      <span className="attachment-tray__count">
        {attachments.length} of {MAX_IMAGES_PER_TURN}
      </span>
    </div>
  );
}
