import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn(),
    getActiveRoles: vi.fn(),
    getMemoryStatus: vi.fn(),
    getDownloadProgress: vi.fn(),
    getDiskUsage: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { useModels } from "./useModels";
import { e4b, entry, NO_ROLES, ORIN_MEMORY } from "../sections/models/fixtures";
import type { DownloadEntry } from "../api/types";

const ID = e4b().id;
const file = (over: Partial<DownloadEntry> = {}) =>
  entry({ filename: "gemma.gguf", model_id: ID, part: "model", total_bytes: 100, ...over });
const progress = (...downloads: DownloadEntry[]) =>
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads });

/** Lets the mount's reads land. */
async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}
async function poll() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(1500);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  vi.mocked(api.listModels).mockResolvedValue([e4b()]);
  vi.mocked(api.getActiveRoles).mockResolvedValue(NO_ROLES);
  vi.mocked(api.getMemoryStatus).mockResolvedValue(ORIN_MEMORY);
  vi.mocked(api.getDiskUsage).mockResolvedValue({
    total_bytes: 1, by_category: {}, hf_cache_bytes: 0, incomplete_bytes: 0,
  });
  progress();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useModels", () => {
  it("reads the catalogue, the jobs, the budget and the downloads on mount", async () => {
    const { result } = renderHook(() => useModels());
    expect(result.current.loading).toBe(true);
    await settle();

    expect(result.current.loading).toBe(false);
    expect(result.current.models.map((m) => m.title)).toEqual(["Gemma 4 E4B"]);
    expect(result.current.roles).toEqual(NO_ROLES);
    expect(result.current.memory).toEqual(ORIN_MEMORY);
    expect(result.current.disk).toBeNull();
    expect(api.getDiskUsage).not.toHaveBeenCalled();
  });

  it("reads the disk only when asked to", async () => {
    const { result } = renderHook(() => useModels({ disk: true }));
    await settle();
    expect(result.current.disk?.total_bytes).toBe(1);
  });

  it("says why the list could not be read, and still stops loading", async () => {
    vi.mocked(api.listModels).mockRejectedValue(new Error("connection refused"));
    const { result } = renderHook(() => useModels());
    await settle();
    expect(result.current.error).toBe("connection refused");
    expect(result.current.loading).toBe(false);
  });

  it("copes with a partial server: a read that fails leaves the rest standing", async () => {
    vi.mocked(api.getMemoryStatus).mockRejectedValue(new Error("nope"));
    vi.mocked(api.getActiveRoles).mockImplementation(() => {
      throw new Error("not a function");
    });
    const { result } = renderHook(() => useModels());
    await settle();
    expect(result.current.models).toHaveLength(1);
    expect(result.current.memory).toBeNull();
    expect(result.current.roles).toBeNull();
    expect(result.current.error).toBeNull();
  });

  it("polls only while something is coming down, and re-reads the rest when it ends", async () => {
    progress(file({ downloaded_bytes: 10 }));
    const { result } = renderHook(() => useModels());
    await settle();
    expect(result.current.downloads).toHaveLength(1);
    const listed = vi.mocked(api.listModels).mock.calls.length;

    await poll();
    await poll();
    expect(vi.mocked(api.getDownloadProgress).mock.calls.length).toBeGreaterThanOrEqual(3);
    // Nothing finished yet, so the list is left alone.
    expect(vi.mocked(api.listModels).mock.calls.length).toBe(listed);

    progress(file({ status: "done", downloaded_bytes: 100 }));
    vi.mocked(api.listModels).mockResolvedValue([e4b({ downloaded: true })]);
    await poll();
    expect(vi.mocked(api.listModels).mock.calls.length).toBe(listed + 1);
    expect(result.current.models[0].downloaded).toBe(true);

    const calls = vi.mocked(api.getDownloadProgress).mock.calls.length;
    await poll();
    await poll();
    expect(vi.mocked(api.getDownloadProgress).mock.calls.length).toBe(calls);
  });

  it("reads a paused download as in flight without polling for it", async () => {
    progress(file({ status: "paused" }));
    const { result } = renderHook(() => useModels());
    await settle();
    expect(result.current.transferFor(e4b())?.state).toBe("paused");
    const calls = vi.mocked(api.getDownloadProgress).mock.calls.length;
    await poll();
    await poll();
    expect(vi.mocked(api.getDownloadProgress).mock.calls.length).toBe(calls);
  });

  it("calls a watched download finishing until its row reads downloaded", async () => {
    progress(file());
    const { result } = renderHook(() => useModels());
    await settle();
    expect(result.current.transferFor(e4b())?.state).toBe("downloading");

    // Every file has arrived, but the pond has not marked the row yet.
    progress(file({ status: "done", downloaded_bytes: 100 }));
    await poll();
    expect(result.current.transferFor(e4b())?.state).toBe("finishing");

    // It keeps asking until the row catches up.
    await poll();
    expect(result.current.transferFor(e4b())?.state).toBe("finishing");
    vi.mocked(api.listModels).mockResolvedValue([e4b({ downloaded: true })]);
    await poll();
    expect(result.current.transferFor(e4b({ downloaded: true }))).toBeNull();
  });

  it("does not call a leftover finished entry a download, for a model this page never watched", async () => {
    progress(file({ status: "done", downloaded_bytes: 100 }));
    const { result } = renderHook(() => useModels());
    await settle();
    expect(result.current.transferFor(e4b())).toBeNull();
  });

  it("starts watching when told a download was just set going", async () => {
    const { result } = renderHook(() => useModels());
    await settle();
    expect(vi.mocked(api.getDownloadProgress)).toHaveBeenCalledTimes(1);

    progress(file());
    await act(async () => {
      await result.current.reloadDownloads();
    });
    const after = vi.mocked(api.getDownloadProgress).mock.calls.length;
    await poll();
    expect(vi.mocked(api.getDownloadProgress).mock.calls.length).toBeGreaterThan(after);
  });

  it("stops polling when its page goes away", async () => {
    progress(file());
    const { unmount } = renderHook(() => useModels());
    await settle();
    unmount();
    const calls = vi.mocked(api.getDownloadProgress).mock.calls.length;
    await poll();
    await poll();
    expect(vi.mocked(api.getDownloadProgress).mock.calls.length).toBe(calls);
  });
});
