// Memory fit: will a model fit the device's LLM budget (GET /api/v1/models/memory-status) or spill
// to CPU? Decode is bandwidth-bound, so a spill drops to single-digit tok/s.

import type { ModelEntry, ModelMemoryStatus } from "./types";

export type FitVerdict = "fits" | "spills" | "unknown";

/** Headroom (MB) beyond the weights for KV cache, activations and system slack. */
export const DEFAULT_HEADROOM_MB = 1024;

/** "unknown" (render nothing) when budget or size is <= 0, e.g. NoopScheduler zeros on Mac/dev. */
export function modelFit(
  modelSizeMb: number | null | undefined,
  availableForLlmMb: number | null | undefined,
  headroomMb: number = DEFAULT_HEADROOM_MB,
): FitVerdict {
  if (availableForLlmMb == null || availableForLlmMb <= 0) return "unknown";
  if (modelSizeMb == null || modelSizeMb <= 0) return "unknown";

  const effectiveBudget = availableForLlmMb - Math.max(0, headroomMb);
  return modelSizeMb <= effectiveBudget ? "fits" : "spills";
}

type Sized = Pick<ModelEntry, "size_mb" | "ram_estimate_mb"> & Partial<Pick<ModelEntry, "companions">>;

/** MB a model could take: what is free now plus what leaving the model in use returns. */
export function availableMb(status: ModelMemoryStatus | null | undefined): number | null {
  if (!status || status.total_mb <= 0) return null;
  const free = status.available_for_llm_mb + Math.max(0, status.reclaimable_mb ?? 0);
  return free > 0 ? free : null;
}

/** What one model may weigh once headroom is set aside; null when the machine reports no budget. */
export function usableBudgetMb(
  status: ModelMemoryStatus | null | undefined,
  headroomMb: number = DEFAULT_HEADROOM_MB,
): number | null {
  const free = availableMb(status);
  return free === null ? null : Math.max(0, free - Math.max(0, headroomMb));
}

/**
 * Prefers `size_mb` (weights that must fit the GPU budget) over `ram_estimate_mb`. Picture support
 * loads on the GPU with the model, so it counts once it is on the device or on its way; one not yet
 * fetched counts only where `withPictures` says it will be.
 */
export function modelResidencyMb(m: Sized, opts: { withPictures?: boolean } = {}): number | null {
  const base =
    m.size_mb != null && m.size_mb > 0
      ? m.size_mb
      : m.ram_estimate_mb != null && m.ram_estimate_mb > 0
        ? m.ram_estimate_mb
        : null;
  if (base == null) return null;
  const pictures = m.companions?.find((c) => c.kind === "pictures");
  const resident =
    pictures &&
    (pictures.state === "installed" ||
      pictures.state === "verifying" ||
      pictures.state === "downloading" ||
      (pictures.state === "available" && opts.withPictures === true));
  return base + (resident ? pictures.size_bytes / 1_048_576 : 0);
}

/** Verdict for a `ModelEntry`; "unknown" when `status` is missing or `total_mb <= 0`. */
export function modelFitFor(
  m: Sized,
  status: ModelMemoryStatus | null | undefined,
  headroomMb: number = DEFAULT_HEADROOM_MB,
  opts: { withPictures?: boolean } = {},
): FitVerdict {
  return modelFit(modelResidencyMb(m, opts), availableMb(status), headroomMb);
}
