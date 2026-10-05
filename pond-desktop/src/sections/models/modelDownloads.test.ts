import { describe, it, expect } from "vitest";
import { controlResult, downloadPercent, isInFlight, transferOf } from "./modelDownloads";
import { entry } from "./fixtures";

const ID = "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL";
const MODEL = "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf";
const PICTURES = "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf";

const model = (over = {}) =>
  entry({ filename: MODEL, model_id: ID, part: "model", total_bytes: 4_215_695_776, downloaded_bytes: 1_200_000_000, ...over });
const pictures = (over = {}) =>
  entry({ filename: PICTURES, category: "mmproj", model_id: ID, part: "pictures", total_bytes: 991_552_320, downloaded_bytes: 500_000_000, ...over });

describe("download progress", () => {
  it("reports a percentage only when the total is known", () => {
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: 200 })).toBe(25);
    expect(downloadPercent({ downloaded_bytes: 200, total_bytes: 200 })).toBe(100);
    // Unknown total: null, so the bar goes indeterminate rather than reading as stalled at zero.
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: null })).toBeNull();
    expect(downloadPercent({ downloaded_bytes: 50, total_bytes: 0 })).toBeNull();
  });

  it("never exceeds 100 when the server over-reports", () => {
    expect(downloadPercent({ downloaded_bytes: 300, total_bytes: 200 })).toBe(100);
  });

  it("counts paused as still in flight, and finished states as not", () => {
    expect(isInFlight({ status: "downloading" })).toBe(true);
    expect(isInFlight({ status: "paused" })).toBe(true);
    for (const status of ["done", "cancelled", "error"] as const) expect(isInFlight({ status })).toBe(false);
  });
});

describe("one model, read together", () => {
  it("lists the model first and its add-on second, whatever order the tracker holds them in", () => {
    const t = transferOf(ID, [pictures(), model()])!;
    expect(t.state).toBe("downloading");
    expect(t.parts.map((p) => p.label)).toEqual(["Model", "Pictures"]);
    expect(t.parts[0].bytes).toBe("1.2 GB of 4.2 GB");
    expect(t.parts[1].bytes).toBe("476 MB of 945 MB");
    expect(t.parts.map((p) => p.percent)).toEqual([28, 50]);
  });

  it("belongs to the row whose id the entries carry, and to no other", () => {
    expect(transferOf("gguf/another", [model(), pictures()])).toBeNull();
    expect(transferOf(ID, [entry({ filename: "x.gguf" })])).toBeNull();
  });

  it("is paused when nothing is moving and something is held", () => {
    const t = transferOf(ID, [model({ status: "paused" }), pictures({ status: "done" })])!;
    expect(t.state).toBe("paused");
    expect(t.parts[1].percent).toBe(100);
  });

  it("carries the reason a part failed, and says so plainly when the server gave none", () => {
    const failed = transferOf(ID, [model({ status: "error", error: "HTTP 503" }), pictures({ status: "done" })])!;
    expect(failed.state).toBe("error");
    expect(failed.error).toBe("HTTP 503");
    const unsaid = transferOf(ID, [model({ status: "error" })])!;
    expect(unsaid.error).toBe("The download did not finish.");
  });

  it("reads a stopped transfer as nothing: stopping deletes the partial file", () => {
    expect(transferOf(ID, [model({ status: "cancelled" }), pictures({ status: "cancelled" })])).toBeNull();
  });

  it("says it is finishing only for a download this page watched arrive, whose row is not marked yet", () => {
    const done = [model({ status: "done" }), pictures({ status: "done" })];
    expect(transferOf(ID, done, { arriving: true, downloaded: false })?.state).toBe("finishing");
    expect(transferOf(ID, done, { arriving: true, downloaded: true })).toBeNull();
    // A finished entry left over from before, for a model since deleted, is not a download.
    expect(transferOf(ID, done, { arriving: false, downloaded: false })).toBeNull();
  });

  it("keeps the bar honest while the total is unknown", () => {
    const t = transferOf(ID, [model({ total_bytes: null, downloaded_bytes: 3_000_000 })])!;
    expect(t.parts[0].percent).toBeNull();
    expect(t.parts[0].bytes).toBe("2 MB");
  });
});

describe("pause, resume and stop", () => {
  it("name the model, whose parts the pond then moves together", () => {
    const t = transferOf(ID, [model(), pictures()])!;
    expect(t.modelId).toBe(ID);
    expect(t.parts.map((p) => p.filename)).toEqual([MODEL, PICTURES]);
  });

  it("say what happened: a pause keeps the file, a stop deletes it", () => {
    expect(controlResult("pause", "Gemma 4 E4B")).toBe("Paused Gemma 4 E4B. What has arrived so far is kept.");
    expect(controlResult("cancel", "Gemma 4 E4B")).toBe("Stopped Gemma 4 E4B. Nothing was kept.");
    expect(controlResult("resume", "Gemma 4 E4B")).toBe("Resuming Gemma 4 E4B.");
  });
});
