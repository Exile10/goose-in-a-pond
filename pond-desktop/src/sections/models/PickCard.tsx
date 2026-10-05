import { Download } from "lucide-react";
import type { ModelEntry, ModelMemoryStatus } from "../../api/types";
import { AddOnLine, EngineMark, FitCell, PicturesChoice, TransferView } from "../../components/models/ModelMarks";
import { engineOf } from "../../lib/modelProvider";
import "../../styles/model-pick.css";
import type { ModelTransfer, TransferAction } from "./modelDownloads";
import {
  addOnOf, downloadSummary, fitReading, formatBytes, formatSize, measuredOf, modelLabel,
  modelFullName, offersPictures, picturesOf,
} from "./modelsView";

export interface PickCardProps {
  model: ModelEntry;
  memory: ModelMemoryStatus | null;
  inUse: boolean;
  /** The one card the page raises on the ink offset. */
  raised: boolean;
  /** The page's one question is this card's button. */
  asking: boolean;
  transfer: ModelTransfer | null;
  withPictures: boolean;
  busy: boolean;
  onWithPictures: (next: boolean) => void;
  onUse: () => void;
  onDownload: () => void;
  onControl: (action: TransferAction) => void;
  /** What the download button says; "Download and use" where pressing it also switches to the model. */
  downloadLabel?: string;
}

/** One of GIAP's picks: why, how it did where it was measured, what it costs, and what to press. */
export function PickCard({
  model, memory, inUse, raised, asking, transfer, withPictures, busy, onWithPictures, onUse,
  onDownload, onControl, downloadLabel = "Download",
}: PickCardProps) {
  const title = modelLabel(model);
  const full = modelFullName(model);
  const engine = engineOf(model);
  const measured = measuredOf(model.recommended);
  const addOn = addOnOf(model);
  const choice = offersPictures(model);
  const pictures = picturesOf(model);
  const onDevice = model.downloaded === true;
  const fit = fitReading(model, memory, { inUse, withPictures: choice && withPictures });

  return (
    <article
      className="mm-pick"
      data-raised={raised ? "true" : undefined}
      data-inuse={inUse ? "true" : undefined}
      aria-label={full}
    >
      <div className="mm-pick__head">
        <h3 className="mm-pick__title">{title}</h3>
        {engine && <EngineMark engine={engine} label={engine.label} format={engine.file_format} />}
      </div>

      <p className="mm-pick__reason">{model.recommended?.reason}</p>

      {measured && (
        <p className="mm-pick__measured">
          <span className="mm-pick__numbers">{measured.text}</span>
          <span className="mm-pick__caption">{measured.caption}</span>
        </p>
      )}

      {addOn.kind !== "none" && (
        <div className="mm-pick__addon">
          {choice && pictures ? (
            <PicturesChoice
              checked={withPictures}
              size={formatBytes(pictures.size_bytes)}
              onChange={onWithPictures}
              disabled={busy || transfer !== null}
            />
          ) : (
            <AddOnLine addOn={addOn} />
          )}
        </div>
      )}

      <div className="mm-pick__foot">
        {transfer ? (
          <TransferView transfer={transfer} title={full} busy={busy} onControl={onControl} />
        ) : (
          <>
            <div className="mm-pick__cost">
              <span className="mm-pick__size">
                {onDevice
                  ? `On this device, ${formatSize(model.size_mb) || "size unknown"}`
                  : downloadSummary(model, choice && withPictures)}
              </span>
              <FitCell fit={fit} />
            </div>
            {inUse ? null : onDevice ? (
              <button
                type="button"
                className={asking ? "mm-btn mm-btn--ask" : "mm-btn"}
                onClick={onUse}
                disabled={busy}
                aria-label={`Use ${full}`}
              >
                Use
              </button>
            ) : (
              <button
                type="button"
                className={asking ? "mm-btn mm-btn--ask" : "mm-btn"}
                onClick={onDownload}
                disabled={busy}
                aria-label={`${downloadLabel} ${full}`}
              >
                <Download size={15} aria-hidden="true" />
                <span>{downloadLabel}</span>
              </button>
            )}
          </>
        )}
      </div>
    </article>
  );
}
