import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen, cleanup, fireEvent, waitFor, within } from "@testing-library/react";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn(),
    getActiveRoles: vi.fn(),
    getMemoryStatus: vi.fn(),
    getDownloadProgress: vi.fn(),
    downloadModel: vi.fn(),
    activateModel: vi.fn(),
    controlModelDownload: vi.fn(),
  },
}));

import { api } from "../../api/PondApiClient";
import { NoModelPicks } from "./NoModelPicks";
import { __resetDownloadAndUseForTests } from "../../state/downloadAndUse";
import { ApiError } from "../../api/types";
import type { DownloadEntry, ModelEntry } from "../../api/types";
import { DESKTOP_MEMORY, E4B_ORIN, e2b, e4b, entry, litertE4b, NO_ROLES, ORIN_MEMORY } from "../../sections/models/fixtures";

const MODEL_FILE = "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf";
const downloading = (over: Partial<DownloadEntry> = {}) =>
  entry({ filename: MODEL_FILE, model_id: e4b().id, part: "model", total_bytes: 4_215_695_776, downloaded_bytes: 1_000_000_000, ...over });

function setup(models: ModelEntry[] = [e4b(), e2b(), litertE4b()], downloads: DownloadEntry[] = [], memory = DESKTOP_MEMORY) {
  vi.mocked(api.listModels).mockResolvedValue(models);
  vi.mocked(api.getActiveRoles).mockResolvedValue(NO_ROLES);
  vi.mocked(api.getMemoryStatus).mockResolvedValue(memory);
  vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads });
}

async function pickCard(name: string): Promise<HTMLElement> {
  return screen.findByRole("article", { name });
}

beforeEach(() => {
  vi.clearAllMocks();
  __resetDownloadAndUseForTests();
  vi.mocked(api.downloadModel).mockResolvedValue({ status: "download_started", message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)" });
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.controlModelDownload).mockResolvedValue({ status: "ok" });
  setup();
});

afterEach(() => {
  cleanup();
  __resetDownloadAndUseForTests();
  vi.useRealTimers();
});

describe("no conversation model", () => {
  it("offers the picks with their sizes and an explicit Download and use, and downloads nothing itself", async () => {
    render(<NoModelPicks />);
    expect(await screen.findByRole("heading", { name: "Pick a model to talk with" })).toBeTruthy();
    expect(screen.getByText("This pond has no conversation model yet. Nothing is downloaded until you choose one.")).toBeTruthy();

    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    expect(within(best).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(within(best).getByText("Best answers this pond can run")).toBeTruthy();
    expect(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" })).toBeTruthy();
    const lite = await pickCard("Gemma 4 E4B, LiteRT-LM");
    expect(within(lite).getByText("3.7 GB")).toBeTruthy();
    expect(within(lite).getByText("Text only")).toBeTruthy();

    await act(async () => {});
    expect(api.downloadModel).not.toHaveBeenCalled();
    expect(api.activateModel).not.toHaveBeenCalled();
  });

  it("starts the add-on unticked where only the model fits, says why, and downloads without it", async () => {
    setup([e4b(), e2b(), litertE4b()], [], ORIN_MEMORY);
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    const tick = within(best).getByRole("checkbox", { name: /Include picture support/ }) as HTMLInputElement;
    expect(tick.checked).toBe(false);
    const why = within(best).getByText("Left out: with pictures it would not fit this pond. Tick to include it anyway.");
    expect(tick.getAttribute("aria-describedby")).toBe(why.id);
    expect(within(best).getByText("4.2 GB")).toBeTruthy();
    expect(within(best).queryByText("Too big for this pond")).toBeNull();

    fireEvent.click(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false }));
  });

  it("downloads with the add-on when it is ticked anyway", async () => {
    setup([e4b(), e2b(), litertE4b()], [], ORIN_MEMORY);
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    fireEvent.click(within(best).getByRole("checkbox", { name: /Include picture support/ }));
    expect(within(best).getByText("4.2 GB + 945 MB for pictures")).toBeTruthy();
    expect(within(best).getByText("Too big for this pond")).toBeTruthy();
    fireEvent.click(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: true }));
  });

  it("asks nothing about picture support for a model that has none to bring", async () => {
    render(<NoModelPicks />);
    const lite = await pickCard("Gemma 4 E4B, LiteRT-LM");
    expect(within(lite).queryByRole("checkbox")).toBeNull();
    fireEvent.click(within(lite).getByRole("button", { name: "Download and use Gemma 4 E4B, LiteRT-LM" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("litert", litertE4b().name, undefined));
  });

  it("keeps its moving bars out of the thread's live region, so they are not read out each second", async () => {
    render(<NoModelPicks />);
    await pickCard("Gemma 4 E4B, llama.cpp");
    expect(document.querySelector(".nm")?.getAttribute("aria-live")).toBe("off");
  });

  it("raises one card, and asks with one button", async () => {
    render(<NoModelPicks />);
    await pickCard("Gemma 4 E4B, llama.cpp");
    expect(document.querySelectorAll(".mm-pick[data-raised='true']")).toHaveLength(1);
    expect(document.querySelectorAll(".mm-btn--ask")).toHaveLength(1);
  });

  it("shows measured numbers only where the server sent them", async () => {
    setup([e4b({ recommended: { ...e4b().recommended!, measured: E4B_ORIN } }), e2b()]);
    render(<NoModelPicks />);
    await pickCard("Gemma 4 E4B, llama.cpp");
    expect(screen.getAllByText(/tokens a second/)).toHaveLength(1);
  });

  it("downloads on the person's word, with the add-on they chose, and uses nothing until it arrives", async () => {
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    fireEvent.click(within(best).getByRole("checkbox", { name: /Include picture support/ }));
    fireEvent.click(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalledWith("gguf", e4b().name, { pictures: false }));
    expect(await screen.findByText("Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)")).toBeTruthy();
    expect(api.activateModel).not.toHaveBeenCalled();
  });

  it("shows a refused download in the pond's own words, and promises nothing", async () => {
    const refusal = "This pond's network setting does not allow it to reach huggingface.co. Change the setting, then try again.";
    vi.mocked(api.downloadModel).mockRejectedValue(new ApiError(403, refusal));
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    fireEvent.click(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" }));
    expect((await screen.findByRole("alert")).textContent).toBe(refusal);
    expect(screen.queryByText(/starts answering as soon as it arrives/)).toBeNull();
  });

  it("says a model already coming down is already on its way, and still uses it on arrival", async () => {
    vi.mocked(api.downloadModel).mockResolvedValue({
      status: "already_downloading",
      message: "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)",
    });
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    fireEvent.click(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" }));
    expect(await screen.findByText("Gemma 4 E4B is already on its way.")).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(await screen.findByText(/starts answering as soon as it arrives/)).toBeTruthy();
  });

  it("uses a pick already on the device when asked, and downloads nothing", async () => {
    setup([e4b(), e2b({ downloaded: true })]);
    render(<NoModelPicks />);
    const light = await pickCard("Gemma 4 E2B, llama.cpp");
    fireEvent.click(within(light).getByRole("button", { name: "Use Gemma 4 E2B, llama.cpp" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("gguf", e2b().name, "chat"));
    expect(api.downloadModel).not.toHaveBeenCalled();
  });

  it("shows the download on the pick, a bar per file, with one set of controls", async () => {
    setup([e4b(), e2b()], [
      downloading(),
      entry({ filename: "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf", category: "mmproj", model_id: e4b().id, part: "pictures", total_bytes: 991_552_320, downloaded_bytes: 100_000_000 }),
    ]);
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");
    await waitFor(() => expect(within(best).getAllByRole("progressbar")).toHaveLength(2));
    fireEvent.click(within(best).getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(api.controlModelDownload).toHaveBeenCalledWith(e4b().id, "cancel"));
  });

  it("uses the model when it arrives, because that was pressed, and not before", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(<NoModelPicks />);
    const best = await pickCard("Gemma 4 E4B, llama.cpp");

    vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: [downloading()] });
    fireEvent.click(within(best).getByRole("button", { name: "Download and use Gemma 4 E4B, llama.cpp" }));
    await waitFor(() => expect(api.downloadModel).toHaveBeenCalled());
    expect(await screen.findByText(/starts answering as soon as it arrives/)).toBeTruthy();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3200);
    });
    expect(api.activateModel).not.toHaveBeenCalled();

    vi.mocked(api.getDownloadProgress).mockResolvedValue({ downloads: [downloading({ status: "done", downloaded_bytes: 4_215_695_776 })] });
    vi.mocked(api.listModels).mockResolvedValue([e4b({ downloaded: true }), e2b()]);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3200);
    });
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("gguf", e4b().name, "chat"));
  });

  it("falls back to a line when the pond has no picks to show", async () => {
    setup([foundOnly()]);
    render(<NoModelPicks />);
    expect(await screen.findByText(/This pond has no picks to show/)).toBeTruthy();
  });
});

function foundOnly(): ModelEntry {
  return { ...e2b(), recommended: undefined };
}
