import type { DownloadEntry, DownloadPart, DownloadStarted } from "../../api/types";
import { formatBytes } from "./modelsView";

// A model comes down as one or two files (the model, and the picture add-on). The server tracks
// each file, so a row reads them together: one set of controls, one bar per file.

/** How far a transfer has got, or `null` when the total is not yet known. */
export function downloadPercent(
  d: Pick<DownloadEntry, "downloaded_bytes" | "total_bytes">,
): number | null {
  if (!d.total_bytes || d.total_bytes <= 0) return null;
  return Math.min(100, Math.round((d.downloaded_bytes / d.total_bytes) * 100));
}

export function isInFlight(d: Pick<DownloadEntry, "status">): boolean {
  return d.status === "downloading" || d.status === "paused";
}

export interface PartProgress {
  part: DownloadPart | "other";
  /** What the household reads: "Model" or "Pictures". */
  label: string;
  /** The tracker key, which pause, resume and stop name. */
  filename: string;
  status: DownloadEntry["status"];
  percent: number | null;
  /** "1.2 GB of 4.2 GB", or just what has arrived while the total is unknown. */
  bytes: string;
  error: string | null;
  /** Whether a pause keeps what has arrived; null when the pond did not say. */
  resumable: boolean | null;
}

export interface ModelTransfer {
  modelId: string;
  /** The model first, then its add-on. */
  parts: PartProgress[];
  /** `finishing`: every part has arrived and the row has not caught up yet. */
  state: "downloading" | "paused" | "error" | "finishing";
  error: string | null;
  /** Whether a pause keeps what has arrived: true when every file still coming down can resume,
   *  false when one cannot, null when the pond did not say. */
  resumable: boolean | null;
}

const PART_LABEL: Record<PartProgress["part"], string> = {
  model: "Model",
  pictures: "Pictures",
  other: "File",
};

const PART_ORDER: Record<PartProgress["part"], number> = { model: 0, pictures: 1, other: 2 };

function partOf(d: DownloadEntry): PartProgress {
  const part = d.part ?? "other";
  const total = d.total_bytes && d.total_bytes > 0 ? d.total_bytes : null;
  return {
    part,
    label: PART_LABEL[part],
    filename: d.filename,
    status: d.status,
    percent: d.status === "done" ? 100 : downloadPercent(d),
    bytes: total
      ? `${formatBytes(d.downloaded_bytes)} of ${formatBytes(total)}`
      : formatBytes(d.downloaded_bytes),
    error: d.status === "error" ? (d.error?.trim() || "The download did not finish.") : null,
    resumable: typeof d.resumable === "boolean" ? d.resumable : null,
  };
}

function resumableOf(parts: PartProgress[]): boolean | null {
  const coming = parts.filter((p) => p.status !== "done");
  if (coming.some((p) => p.resumable === false)) return false;
  return coming.length > 0 && coming.every((p) => p.resumable === true) ? true : null;
}

/** What is coming down for one model, or null when nothing is: a stopped transfer left nothing.
 *  `arriving`: this page watched it come down, so a finished transfer whose row is not yet marked
 *  downloaded is the pond still getting it ready, not a leftover. */
export function transferOf(
  modelId: string,
  downloads: DownloadEntry[],
  opts: { downloaded?: boolean; arriving?: boolean } = {},
): ModelTransfer | null {
  const entries = downloads.filter((d) => d.model_id === modelId && d.status !== "cancelled");
  if (entries.length === 0) return null;

  const parts = entries.map(partOf).sort((a, b) => PART_ORDER[a.part] - PART_ORDER[b.part]);
  const failed = parts.find((p) => p.status === "error");
  const state: ModelTransfer["state"] | null = parts.some((p) => p.status === "downloading")
    ? "downloading"
    : parts.some((p) => p.status === "paused")
      ? "paused"
      : failed
        ? "error"
        : opts.arriving && !opts.downloaded
          ? "finishing"
          : null;
  if (state === null) return null;
  return { modelId, parts, state, error: failed?.error ?? null, resumable: resumableOf(parts) };
}

export type TransferAction = "pause" | "resume" | "cancel";

/** What a pause means for what has arrived, said only once the pond says whether it can resume. */
export function pausedText(resumable: boolean | null, subject = ""): string {
  const kept =
    resumable === true
      ? " What has arrived so far is kept."
      : resumable === false
        ? " It will start again from the beginning."
        : "";
  return `Paused${subject ? ` ${subject}` : ""}.${kept}`;
}

/** What happened. A stop deletes what had arrived; a pause says what it keeps where the pond says. */
export function controlResult(action: TransferAction, title: string, resumable: boolean | null = null): string {
  switch (action) {
    case "pause":
      return pausedText(resumable, title);
    case "resume":
      return `Resuming ${title}.`;
    case "cancel":
      return `Stopped ${title}. Nothing was kept.`;
  }
}

/** What to say once the pond has answered a download request: its own sentence about what it will
 *  fetch, except that a file already coming down is not a new start. */
export function startedText(
  started: Pick<DownloadStarted, "status" | "message"> | null | undefined,
  title: string,
  fallback = `Downloading ${title}.`,
): string {
  if (started?.status === "already_downloading") return `${title} is already on its way.`;
  return started?.message ?? fallback;
}
