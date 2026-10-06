import { describe, it, expect } from "vitest";
import {
  availableMb,
  modelFit,
  modelFitFor,
  modelResidencyMb,
  usableBudgetMb,
  DEFAULT_HEADROOM_MB,
} from "./modelFit";
import type { ModelCompanion, ModelMemoryStatus } from "./types";

const PICTURES = (state: ModelCompanion["state"]): ModelCompanion[] => [
  { kind: "pictures", label: "Gemma 4 E2B", size_bytes: 986_833_728, state },
];

describe("modelFit", () => {
  // Budget mirrors the Jetson-class E2E mock: 4096 MB available for the LLM.
  const AVAIL = 4096;
  const EFFECTIVE = AVAIL - DEFAULT_HEADROOM_MB;

  it("fits when the model is well under the effective budget", () => {
    // gemma-2-2b (~1600 MB) — the safe fitting alternative.
    expect(modelFit(1600, AVAIL)).toBe("fits");
  });

  it("fits a ~2 GB 3B-Q4 model (the roofline-friendly choice)", () => {
    expect(modelFit(2000, AVAIL)).toBe("fits");
  });

  it("spills when the model exceeds the budget", () => {
    // gemma3n:e2b as actually downloaded (~5600 MB).
    expect(modelFit(5600, AVAIL)).toBe("spills");
  });

  it("fits exactly at the effective budget boundary", () => {
    expect(modelFit(EFFECTIVE, AVAIL)).toBe("fits");
  });

  it("spills just one MB over the effective budget boundary", () => {
    expect(modelFit(EFFECTIVE + 1, AVAIL)).toBe("spills");
  });

  it("returns unknown when the budget is zero (memory-status unavailable)", () => {
    // NoopScheduler (llamafile/ollama) reports zeros on Mac/dev.
    expect(modelFit(2000, 0)).toBe("unknown");
  });

  it("returns unknown when the budget is null or negative", () => {
    expect(modelFit(2000, null)).toBe("unknown");
    expect(modelFit(2000, -1)).toBe("unknown");
  });

  it("returns unknown when the model size is unknown", () => {
    expect(modelFit(null, AVAIL)).toBe("unknown");
    expect(modelFit(0, AVAIL)).toBe("unknown");
  });

  it("respects a custom headroom margin", () => {
    expect(modelFit(3500, AVAIL, 0)).toBe("fits");
    expect(modelFit(3500, AVAIL, 1024)).toBe("spills");
  });

  it("clamps a negative headroom to zero", () => {
    expect(modelFit(AVAIL, AVAIL, -500)).toBe("fits");
    expect(modelFit(AVAIL + 1, AVAIL, -500)).toBe("spills");
  });
});

describe("modelResidencyMb", () => {
  it("prefers size_mb (on-disk residency) over ram_estimate_mb", () => {
    expect(modelResidencyMb({ size_mb: 5600, ram_estimate_mb: 5600 })).toBe(5600);
    expect(modelResidencyMb({ size_mb: 3100, ram_estimate_mb: 4000 })).toBe(3100);
  });

  it("falls back to ram_estimate_mb when size_mb is missing", () => {
    expect(modelResidencyMb({ ram_estimate_mb: 3200 })).toBe(3200);
    expect(modelResidencyMb({ size_mb: 0, ram_estimate_mb: 3200 })).toBe(3200);
  });

  it("returns null when neither is known", () => {
    expect(modelResidencyMb({})).toBeNull();
    expect(modelResidencyMb({ size_mb: 0, ram_estimate_mb: 0 })).toBeNull();
  });

  it("counts the add-on once it is installed or on its way, because it loads on the GPU too", () => {
    const encoderMb = 986_833_728 / 1_048_576;
    for (const state of ["installed", "verifying", "downloading"] as const) {
      expect(modelResidencyMb({ size_mb: 2600, companions: PICTURES(state) })).toBeCloseTo(
        2600 + encoderMb,
        6,
      );
    }
  });

  it("counts an add-on not yet fetched only where the person is including it", () => {
    const m = { size_mb: 2600, companions: PICTURES("available") };
    expect(modelResidencyMb(m)).toBe(2600);
    expect(modelResidencyMb(m, { withPictures: true })).toBeCloseTo(2600 + 986_833_728 / 1_048_576, 6);
  });

  it("never counts an add-on this device will not carry", () => {
    expect(
      modelResidencyMb({ size_mb: 2600, companions: PICTURES("not_on_this_device") }, { withPictures: true }),
    ).toBe(2600);
    expect(modelResidencyMb({ size_mb: 2600, companions: [] })).toBe(2600);
  });
});

describe("the budget", () => {
  const status = (over: Partial<ModelMemoryStatus> = {}): ModelMemoryStatus => ({
    total_mb: 7620,
    available_for_llm_mb: 2000,
    loaded_model: null,
    ...over,
  });

  it("is what is free plus what leaving the model in use returns", () => {
    expect(availableMb(status())).toBe(2000);
    expect(availableMb(status({ reclaimable_mb: 2600 }))).toBe(4600);
  });

  it("sets headroom aside, and never reads below zero", () => {
    expect(usableBudgetMb(status({ reclaimable_mb: 2600 }))).toBe(4600 - DEFAULT_HEADROOM_MB);
    expect(usableBudgetMb(status({ available_for_llm_mb: 500 }))).toBe(0);
  });

  it("is unknown, not zero, when the machine reports no budget", () => {
    expect(availableMb(null)).toBeNull();
    expect(usableBudgetMb(undefined)).toBeNull();
    expect(usableBudgetMb(status({ total_mb: 0, available_for_llm_mb: 0 }))).toBeNull();
    expect(usableBudgetMb(status({ available_for_llm_mb: 0 }))).toBeNull();
  });
});

describe("modelFitFor", () => {
  const status: ModelMemoryStatus = {
    total_mb: 8192,
    available_for_llm_mb: 4096,
    loaded_model: null,
  };

  it("spills the corrected 5.6 GB gemma3n:e2b on an 8 GB device", () => {
    expect(modelFitFor({ size_mb: 5600, ram_estimate_mb: 5600 }, status)).toBe("spills");
  });

  it("fits the 1.6 GB llamafile alternative on an 8 GB device", () => {
    expect(modelFitFor({ size_mb: 1600, ram_estimate_mb: 1800 }, status)).toBe("fits");
  });

  it("returns unknown when memory-status is null (Mac/dev)", () => {
    expect(modelFitFor({ size_mb: 5600 }, null)).toBe("unknown");
  });

  it("returns unknown when the scheduler reports no budget (total_mb 0)", () => {
    const noop: ModelMemoryStatus = { total_mb: 0, available_for_llm_mb: 0, loaded_model: null };
    expect(modelFitFor({ size_mb: 5600 }, noop)).toBe("unknown");
  });

  it("counts what a switch frees, so the model in use never blocks its replacement", () => {
    // The Orin with a 2.6 GB model loaded: 1000 MB free, 2600 MB returned by switching.
    const loaded: ModelMemoryStatus = {
      total_mb: 7620,
      available_for_llm_mb: 1000,
      loaded_model: null,
      reclaimable_mb: 2600,
    };
    expect(modelFitFor({ size_mb: 2400 }, loaded)).toBe("fits");
    expect(modelFitFor({ size_mb: 2400 }, { ...loaded, reclaimable_mb: 0 })).toBe("spills");
  });

  it("counts a picture add-on that will come with the download", () => {
    const m = { size_mb: 2600, companions: PICTURES("available") };
    const near: ModelMemoryStatus = { total_mb: 7620, available_for_llm_mb: 4000, loaded_model: null };
    expect(modelFitFor(m, near)).toBe("fits");
    expect(modelFitFor(m, near, DEFAULT_HEADROOM_MB, { withPictures: true })).toBe("spills");
  });
});
