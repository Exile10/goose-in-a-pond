import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    downloadModel: vi.fn(),
    getDownloadProgress: vi.fn(),
    listModels: vi.fn(),
    activateModel: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import {
  __resetDownloadAndUseForTests, dismissPending, downloadAndUse, onUsed, usePendingUse,
} from "./downloadAndUse";
import { e4b } from "../sections/models/fixtures";
import type { DownloadEntry } from "../api/types";

const ID = "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL";
const file = (over: Partial<DownloadEntry>): DownloadEntry => ({
  filename: "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
  category: "gguf",
  downloaded_bytes: 0,
  total_bytes: 100,
  status: "downloading",
  model_id: ID,
  part: "model",
  ...over,
});

const progress = (...downloads: DownloadEntry[]) =>
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads });

/** One poll, and the promises it started. */
async function tick() {
  await vi.advanceTimersByTimeAsync(1500);
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  __resetDownloadAndUseForTests();
  vi.mocked(api.downloadModel).mockResolvedValue({ status: "download_started", message: "Downloading Gemma 4 E4B (4.2 GB)" });
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.listModels).mockResolvedValue([e4b({ downloaded: false })]);
});

afterEach(() => {
  __resetDownloadAndUseForTests();
  vi.useRealTimers();
});

describe("Download and use", () => {
  it("downloads at the person's word, with the add-on they chose, and uses nothing yet", async () => {
    progress(file({ downloaded_bytes: 10 }));
    const started = await downloadAndUse(e4b(), "Gemma 4 E4B", false);

    expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false });
    expect(started?.message).toBe("Downloading Gemma 4 E4B (4.2 GB)");
    await tick();
    expect(api.activateModel).not.toHaveBeenCalled();
  });

  it("uses the model once its file has arrived and its row reads downloaded", async () => {
    progress(file({ status: "downloading" }));
    const { result } = renderHook(() => usePendingUse());
    await downloadAndUse(e4b(), "Gemma 4 E4B", true);
    expect(result.current).toEqual([{ id: ID, title: "Gemma 4 E4B", state: "downloading", message: null }]);

    await tick();
    expect(api.activateModel).not.toHaveBeenCalled();

    progress(file({ status: "done", downloaded_bytes: 100 }));
    vi.mocked(api.listModels).mockResolvedValue([e4b({ downloaded: true })]);
    const used = vi.fn();
    onUsed(used);
    await tick();

    expect(api.activateModel).toHaveBeenCalledWith("gguf", e4b().name, "chat");
    expect(used).toHaveBeenCalledWith({ id: ID, title: "Gemma 4 E4B" });
    expect(result.current).toEqual([]);
  });

  it("waits while the file is done but the pond has not marked the row, then gives up with a reason", async () => {
    progress(file({ status: "done" }));
    await downloadAndUse(e4b(), "Gemma 4 E4B", true);
    const { result } = renderHook(() => usePendingUse());

    for (let i = 0; i < 29; i += 1) await tick();
    expect(api.activateModel).not.toHaveBeenCalled();
    expect(result.current[0]?.state).toBe("downloading");

    await tick();
    expect(result.current[0]).toMatchObject({ state: "failed" });
    expect(result.current[0]?.message).toMatch(/has not registered it yet/);
  });

  it("stops promising when the person stops the download", async () => {
    progress(file({ status: "downloading" }));
    await downloadAndUse(e4b(), "Gemma 4 E4B", true);
    const { result } = renderHook(() => usePendingUse());

    progress(file({ status: "cancelled" }));
    await tick();

    expect(api.activateModel).not.toHaveBeenCalled();
    expect(result.current).toEqual([]);
  });

  it("keeps the promise through a pause, and says why when a part fails", async () => {
    progress(file({ status: "paused" }));
    await downloadAndUse(e4b(), "Gemma 4 E4B", true);
    const { result } = renderHook(() => usePendingUse());
    await tick();
    expect(result.current[0]?.state).toBe("downloading");

    progress(file({ status: "error", error: "The download site is having trouble (error 503). Try again later." }));
    await tick();
    expect(result.current[0]).toMatchObject({
      state: "failed",
      message: "The download site is having trouble (error 503). Try again later.",
    });
    expect(api.activateModel).not.toHaveBeenCalled();

    act(() => dismissPending(ID));
    expect(result.current).toEqual([]);
  });

  it("says why when the pond will not activate it", async () => {
    progress(file({ status: "done" }));
    vi.mocked(api.listModels).mockResolvedValue([e4b({ downloaded: true })]);
    vi.mocked(api.activateModel).mockRejectedValue(new Error("Category 'gguf' cannot be assigned"));
    await downloadAndUse(e4b(), "Gemma 4 E4B", true);
    const { result } = renderHook(() => usePendingUse());
    await tick();
    expect(result.current[0]).toMatchObject({ state: "failed", message: "Category 'gguf' cannot be assigned" });
  });

  it("promises nothing when the pond refuses the download", async () => {
    vi.mocked(api.downloadModel).mockRejectedValue(new Error("blocked by network mode"));
    const { result } = renderHook(() => usePendingUse());
    await expect(downloadAndUse(e4b(), "Gemma 4 E4B", true)).rejects.toThrow("blocked");
    expect(result.current).toEqual([]);
    await tick();
    expect(api.getDownloadProgress).not.toHaveBeenCalled();
  });

  it("only uses what is already on the device when asked to, and downloads nothing", async () => {
    await downloadAndUse(e4b({ downloaded: true }), "Gemma 4 E4B", true);
    expect(api.downloadModel).not.toHaveBeenCalled();
    expect(api.activateModel).toHaveBeenCalledWith("gguf", e4b().name, "chat");
  });
});
