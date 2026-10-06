import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    downloadModel: vi.fn(),
    getDownloadProgress: vi.fn(),
    listModels: vi.fn(),
    activateModel: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { conversationModelChosen, useConversationModel } from "./useConversationModel";
import { __resetDownloadAndUseForTests, downloadAndUse } from "../state/downloadAndUse";
import { e4b } from "../sections/models/fixtures";

describe("conversationModelChosen", () => {
  it("is the pond's own rule: an empty model is no choice, whatever the provider", () => {
    for (const provider of ["local", "gguf", "llamafile", "ollama"]) {
      expect(conversationModelChosen(provider, "")).toBe(false);
      expect(conversationModelChosen(provider, "  ")).toBe(false);
    }
    expect(conversationModelChosen("", "gemma-4-E4B-it-qat-UD-Q4_K_XL")).toBe(false);
    expect(conversationModelChosen("local", "gemma-4-E4B-it-qat-UD-Q4_K_XL")).toBe(true);
    expect(conversationModelChosen("ollama", "qwen3:4b")).toBe(true);
  });

  it("needs no model name for another pond or the offline echo", () => {
    expect(conversationModelChosen("mesh", "")).toBe(true);
    expect(conversationModelChosen("mock", "")).toBe(true);
  });
});

describe("useConversationModel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    __resetDownloadAndUseForTests();
  });
  afterEach(() => {
    __resetDownloadAndUseForTests();
  });

  it("reads none when the settings name no model", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "", chat_model: "" } as never);
    const { result } = renderHook(() => useConversationModel());
    expect(result.current.state).toBe("unknown");
    await waitFor(() => expect(result.current.state).toBe("none"));
  });

  it("reads chosen when they do", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", chat_model: "gemma" } as never);
    const { result } = renderHook(() => useConversationModel());
    await waitFor(() => expect(result.current.state).toBe("chosen"));
  });

  it("is no evidence either way when the settings carry neither field, or cannot be read", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ user_name: "Jerry" } as never);
    const { result, unmount } = renderHook(() => useConversationModel());
    await act(async () => {});
    expect(result.current.state).toBe("unknown");
    unmount();

    vi.mocked(api.getSettings).mockRejectedValue(new Error("offline"));
    const again = renderHook(() => useConversationModel());
    await act(async () => {});
    expect(again.result.current.state).toBe("unknown");
  });

  it("goes to none when the pond refuses a turn for want of a model", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", chat_model: "gemma" } as never);
    const { result } = renderHook(() => useConversationModel());
    await waitFor(() => expect(result.current.state).toBe("chosen"));
    act(() => result.current.markNone());
    expect(result.current.state).toBe("none");
  });

  it("goes to chosen when a model the person asked for arrives and is used", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "", chat_model: "" } as never);
    vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
    const { result } = renderHook(() => useConversationModel());
    await waitFor(() => expect(result.current.state).toBe("none"));

    await act(async () => {
      await downloadAndUse(e4b({ downloaded: true }), "Gemma 4 E4B", true);
    });
    expect(result.current.state).toBe("chosen");
  });
});
