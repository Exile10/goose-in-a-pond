// Memory-fit guard: will a model fit the device's LLM budget (GET /api/v1/models/memory-status)
// or spill to CPU? Decode is bandwidth-bound, so a spill drops to single-digit tok/s.

import type { ModelEntry, ModelMemoryStatus } from "./types";

/** Fit verdict for a model against a device's LLM memory budget. */
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

/**
 * Best residency-size estimate (MB) for a model.
 *
 * Prefers `size_mb` (on-disk weight residency — what actually has to fit in the
 * GPU budget) and falls back to `ram_estimate_mb`. Returns `null` when neither
 * is known.
 *
 * A declared-vision model keeps its encoder resident too (it loads eagerly,
 * on the GPU, at every model load — see `models/domain/vision_encoder.rs`),
 * so `image_support_bytes` is added on top whenever `reads_images` is true.
 */
export function modelResidencyMb(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "reads_images" | "image_support_bytes">,
): number | null {
  const base =
    m.size_mb != null && m.size_mb > 0
      ? m.size_mb
      : m.ram_estimate_mb != null && m.ram_estimate_mb > 0
        ? m.ram_estimate_mb
        : null;
  if (base == null) return null;
  const encoderMb =
    m.reads_images === true && m.image_support_bytes ? m.image_support_bytes / 1_048_576 : 0;
  return base + encoderMb;
}

/** Verdict for a `ModelEntry`; "unknown" when `status` is missing or `total_mb <= 0`. */
export function modelFitFor(
  m: Pick<ModelEntry, "size_mb" | "ram_estimate_mb" | "reads_images" | "image_support_bytes">,
  status: ModelMemoryStatus | null | undefined,
  headroomMb: number = DEFAULT_HEADROOM_MB,
): FitVerdict {
  if (!status || status.total_mb <= 0) return "unknown";
  return modelFit(modelResidencyMb(m), status.available_for_llm_mb, headroomMb);
}
