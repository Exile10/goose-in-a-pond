import { Download, Trash2 } from "lucide-react";
import type { ModelEntry, ModelMemoryStatus } from "../../api/types";
import { AddOnLine, FitCell, PicturesChoice, SourceChip, TransferView } from "../../components/models/ModelMarks";
import type { ModelTransfer, TransferAction } from "./modelDownloads";
import {
  type RoleKey, ROLES, addOnOf, downloadSummary, fitReading, formatBytes, formatSize, modelFacts,
  modelFullName, modelLabel, offersPictures, picturesOf, sourceChip,
} from "./modelsView";

export interface ModelRowProps {
  model: ModelEntry;
  memory: ModelMemoryStatus | null;
  inUse: boolean;
  /** The jobs this row can take: one "Use" each. */
  roles: RoleKey[];
  transfer: ModelTransfer | null;
  /** The household's choice on the tick box, for a row not yet on the device. */
  withPictures: boolean;
  busy: boolean;
  onWithPictures: (next: boolean) => void;
  onUse: (role: RoleKey) => void;
  onDelete: () => void;
  onDownload: () => void;
  onAddPictures: () => void;
  onControl: (action: TransferAction) => void;
}

/** One model, on the device or still to fetch. It says what it is, what it costs and what it can do. */
export function ModelRow({
  model, memory, inUse, roles, transfer, withPictures, busy, onWithPictures, onUse, onDelete,
  onDownload, onAddPictures, onControl,
}: ModelRowProps) {
  const title = modelLabel(model);
  const full = modelFullName(model);
  const chip = sourceChip(model);
  const facts = modelFacts(model);
  const addOn = addOnOf(model);
  const onDevice = model.downloaded === true;
  const choice = offersPictures(model);
  const fit = fitReading(model, memory, { inUse, withPictures: choice && withPictures });
  const pictures = picturesOf(model);
  const canDownload = !onDevice && model.acquire !== "external" && model.acquire !== "unavailable";
  const managedElsewhere = model.acquire === "external";

  return (
    <div className="mdl-row" data-fit={fit.state} data-inuse={inUse ? "true" : undefined}>
      <div className="mdl-row__id">
        <div className="mdl-row__idline">
          <span className="mdl-row__name" title={title}>{title}</span>
          {chip && <SourceChip chip={chip} />}
        </div>
        {facts.length > 0 && <span className="mdl-row__facts">{facts.join(" · ")}</span>}
        {addOn.kind !== "none" && (
          <div className="mdl-row__addon">
            {choice && pictures ? (
              <PicturesChoice
                checked={withPictures}
                size={formatBytes(pictures.size_bytes)}
                onChange={onWithPictures}
                disabled={busy || transfer !== null}
              />
            ) : (
              <AddOnLine
                addOn={addOn}
                busy={busy}
                onAdd={onDevice && addOn.kind === "add" ? onAddPictures : undefined}
              />
            )}
          </div>
        )}
      </div>

      <span className="mdl-row__size">
        {onDevice
          ? formatSize(model.size_mb ?? model.ram_estimate_mb) || "—"
          : downloadSummary(model, choice && withPictures)}
      </span>

      <div className="mdl-row__fit">
        <FitCell fit={fit} />
      </div>

      {transfer ? (
        <div className="mdl-row__xfer">
          <TransferView transfer={transfer} title={full} busy={busy} onControl={onControl} />
        </div>
      ) : (
        <div className="mdl-row__acts">
          {onDevice &&
            !inUse &&
            roles.map((role) => (
              <button
                key={role}
                type="button"
                className="mm-btn"
                onClick={() => onUse(role)}
                disabled={busy}
                aria-label={`Use ${full} for ${ROLES.find((r) => r.key === role)?.label.toLowerCase()}`}
              >
                Use
              </button>
            ))}
          {onDevice && !managedElsewhere && (
            <button
              type="button"
              className="mm-btn mm-btn--danger"
              onClick={onDelete}
              disabled={busy || inUse}
              title={inUse ? "In use. Choose another model first." : undefined}
              aria-label={`Delete ${full}`}
            >
              <Trash2 size={15} aria-hidden="true" />
              <span>Delete</span>
            </button>
          )}
          {canDownload && (
            <button
              type="button"
              className="mm-btn"
              onClick={onDownload}
              disabled={busy}
              aria-label={`Download ${full}`}
            >
              <Download size={15} aria-hidden="true" />
              <span>Download</span>
            </button>
          )}
          {!onDevice && !canDownload && !managedElsewhere && (
            <span className="mdl-row__note">No source to download from</span>
          )}
        </div>
      )}
    </div>
  );
}
