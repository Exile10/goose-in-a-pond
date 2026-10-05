import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor, within } from "@testing-library/react";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listModels: vi.fn(),
    getActiveRoles: vi.fn(),
    getSettings: vi.fn(),
    activateModel: vi.fn(),
    updateSettings: vi.fn(),
  },
}));

const dispatch = vi.hoisted(() => vi.fn());
vi.mock("../state/AppContext", () => ({
  useAppState: () => ({}),
  useAppDispatch: () => dispatch,
}));

import { api } from "../api/PondApiClient";
import { ModelSwitcher } from "./ModelSwitcher";
import type { ModelEntry } from "../api/types";
import {
  e2b, e4b, foundOnDisk, functionGemma, litertE4b, llamafile, NO_ROLES, ollama, rolesWith, voice,
  whisper,
} from "../sections/models/fixtures";

const HERE = { downloaded: true };

const CATALOGUE: ModelEntry[] = [
  e2b(HERE), foundOnDisk(), litertE4b(HERE), ollama(), llamafile(),
  e4b(), whisper("base.en", HERE), voice(), functionGemma(),
];

function setup(over: {
  models?: ModelEntry[];
  roles?: ReturnType<typeof rolesWith>;
  settings?: Record<string, unknown>;
} = {}) {
  vi.mocked(api.listModels).mockResolvedValue(over.models ?? CATALOGUE);
  vi.mocked(api.getActiveRoles).mockResolvedValue(over.roles ?? rolesWith({ provider: "local", model: e2b().name }));
  vi.mocked(api.getSettings).mockResolvedValue({ chat_provider: "local", mesh_enabled: false, ...over.settings } as never);
}

const trigger = () => screen.getByRole("button", { name: "Select model" });

async function open() {
  const onSwitched = vi.fn();
  render(<ModelSwitcher onSwitched={onSwitched} />);
  await waitFor(() => expect(trigger().textContent).not.toBe("Model"));
  fireEvent.click(trigger());
  return { onSwitched, menu: document.querySelector(".model-selector-dropdown") as HTMLElement };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.activateModel).mockResolvedValue(undefined as never);
  vi.mocked(api.updateSettings).mockResolvedValue({} as never);
  setup();
});

afterEach(cleanup);

describe("the composer's model chip", () => {
  it("names the model and the engine that runs it, not 'local'", async () => {
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("Gemma 4 E2B · llama.cpp"));
    expect(trigger().textContent).not.toMatch(/local/i);
  });

  it("names a LiteRT-LM model by its engine too, though the role reads local", async () => {
    setup({ roles: rolesWith({ provider: "local", model: litertE4b().name }) });
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("Gemma 4 E4B · LiteRT-LM"));
  });

  it("says Choose a model while none is chosen", async () => {
    setup({ roles: NO_ROLES, settings: { chat_provider: "", chat_model: "" } });
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("Choose a model"));
  });

  it("says the mesh when the mesh answers", async () => {
    setup({ settings: { chat_provider: "mesh", mesh_enabled: true } });
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("Mesh (trusted peer)"));
  });

  it("keeps the role's own name when the model's row has gone", async () => {
    setup({ roles: rolesWith({ provider: "local", model: "a-model-i-removed" }) });
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("a-model-i-removed"));
  });
});

describe("the list", () => {
  it("groups by engine, under each engine's own name, in the order a household meets them", async () => {
    const { menu } = await open();
    const groups = within(menu).getAllByRole("group").map((g) => g.getAttribute("aria-label"));
    expect(groups).toEqual(["llama.cpp", "LiteRT-LM", "Ollama", "llamafile"]);
    expect(within(menu).queryByText(/^Gguf$|^Litert$|^Local$/)).toBeNull();
    expect(within(within(menu).getByRole("group", { name: "llama.cpp" })).getByText(".gguf")).toBeTruthy();
  });

  it("lists models that can hold a conversation, and nothing else", async () => {
    const { menu } = await open();
    const names = Array.from(menu.querySelectorAll(".model-selector-dropdown__item-name")).map((n) => n.textContent);
    expect(names).toEqual([
      "Gemma 4 E2B", "Llama-3.2-3B-Instruct-Q4_K_M", "Gemma 4 E4B", "qwen3:4b", "mistral-7b-instruct",
    ]);
    // Not on the device, not a helper, not speech, not a voice.
    for (const hidden of ["FunctionGemma 270M", "Whisper base.en (en)", "Af_Heart"]) {
      expect(within(menu).queryByText(hidden)).toBeNull();
    }
    // Ollama's models come from the catalogue now, so nothing asks Ollama separately.
    expect(within(menu).getByText("qwen3:4b")).toBeTruthy();
  });

  it("marks the model in use", async () => {
    const { menu } = await open();
    const active = menu.querySelector(".model-selector-dropdown__item.is-active");
    expect(active?.getAttribute("aria-label")).toBe("Gemma 4 E2B, llama.cpp, in use");
    expect(menu.querySelectorAll(".is-active")).toHaveLength(1);
  });

  it("gives two models of one title their engines to tell them apart", async () => {
    setup({ models: [litertE4b(HERE), e4b(HERE)] });
    const { menu } = await open();
    expect(within(menu).getByRole("button", { name: "Gemma 4 E4B, llama.cpp" })).toBeTruthy();
    expect(within(menu).getByRole("button", { name: "Gemma 4 E4B, LiteRT-LM" })).toBeTruthy();
  });

  it("offers the mesh as its own group where it is on, and switches to it by setting", async () => {
    setup({ settings: { chat_provider: "local", mesh_enabled: true } });
    const { menu, onSwitched } = await open();
    fireEvent.click(within(menu).getByRole("button", { name: "Mesh (trusted peer)" }));
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ chat_provider: "mesh" }));
    expect(api.activateModel).not.toHaveBeenCalled();
    await waitFor(() => expect(onSwitched).toHaveBeenCalled());
  });

  it("says so, with a way on, when no model is on the device", async () => {
    setup({ models: [e4b(), whisper()], roles: NO_ROLES });
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("Choose a model"));
    fireEvent.click(trigger());
    expect(screen.getByText(/No model is on this device yet/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open Models" }));
    expect(dispatch).toHaveBeenCalledWith({ type: "SET_SECTION", payload: "models" });
  });

  it("closes on Escape", async () => {
    const { menu } = await open();
    expect(menu).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(document.querySelector(".model-selector-dropdown")).toBeNull();
  });
});

describe("switching", () => {
  it("activates the model for conversation, then reads the chip from the pond again", async () => {
    const { menu, onSwitched } = await open();
    vi.mocked(api.getActiveRoles).mockResolvedValue(rolesWith({ provider: "local", model: litertE4b().name }));
    fireEvent.click(within(menu).getByRole("button", { name: "Gemma 4 E4B, LiteRT-LM" }));
    await waitFor(() => expect(api.activateModel).toHaveBeenCalledWith("litert", litertE4b().name, "chat"));
    expect(dispatch).toHaveBeenCalledWith({
      type: "SET_LAST_RESPONSE_META",
      payload: { modelName: litertE4b().name, modelRole: "chat", completionTokens: 0 },
    });
    await waitFor(() => expect(trigger().textContent).toBe("Gemma 4 E4B · LiteRT-LM"));
    expect(onSwitched).toHaveBeenCalled();
    expect(document.querySelector(".model-selector-dropdown")).toBeNull();
  });

  it("says why a switch failed, in the list, and does not leave it to the console", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.mocked(api.activateModel).mockRejectedValue(new Error("The file is not on this device"));
    const { menu, onSwitched } = await open();
    fireEvent.click(within(menu).getByRole("button", { name: "Gemma 4 E4B, LiteRT-LM" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Could not switch to Gemma 4 E4B: The file is not on this device");
    // The list stays open to try another, and the chip still names what is answering.
    expect(document.querySelector(".model-selector-dropdown")).toBeTruthy();
    expect(trigger().textContent).toBe("Gemma 4 E2B · llama.cpp");
    expect(onSwitched).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("says when the list of models could not be read, and still names what answers", async () => {
    vi.mocked(api.listModels).mockRejectedValue(new Error("offline"));
    vi.mocked(api.getActiveRoles).mockResolvedValue(rolesWith({ provider: "local", model: e2b().name }));
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe(e2b().name));
    fireEvent.click(trigger());
    expect((await screen.findByRole("alert")).textContent).toMatch(/Could not read the model list/);
  });

  it("copes with a pond that answers some reads and not others", async () => {
    vi.mocked(api.getSettings).mockRejectedValue(new Error("nope"));
    render(<ModelSwitcher />);
    await waitFor(() => expect(trigger().textContent).toBe("Gemma 4 E2B · llama.cpp"));
  });
});
