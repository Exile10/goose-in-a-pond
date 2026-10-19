import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { StateCtx } from "../state/useAppContext";
import type { AppState } from "../state/reducer";
import type { MemoryFragment } from "../api/types";
import { api } from "../api/PondApiClient";
import { Context } from "./Context";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listMemories: vi.fn(),
    getContextIndexHealth: vi.fn(),
  },
}));

const m = vi.mocked(api);

function memory(id: string, content: string, extra: Partial<MemoryFragment>): MemoryFragment {
  return { id, content, created_at: new Date().toISOString(), access_count: 0, ...extra };
}

async function cardFor(content: string): Promise<HTMLElement> {
  const body = await screen.findByText(content);
  const card = body.closest("article");
  if (!card) throw new Error(`no card holds "${content}"`);
  return card;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("a memory card names its category, not its tier", () => {
  it("shows the phone's words for the segment and a Kept marker only for the permanent tier", async () => {
    m.listMemories.mockResolvedValue([
      memory("1", "My name is Wanjiru", { segment: "identity", tier: "permanent" }),
      memory("2", "I run before work", { segment: "routine", tier: "long" }),
      memory("3", "Amina is my sister", { segment: "relationship", tier: "short" }),
      memory("4", "Something unsorted", { tier: "long" }),
    ]);
    m.getContextIndexHealth.mockResolvedValue(null as never);
    render(
      <StateCtx.Provider value={{ sessionId: "s1" } as AppState}>
        <Context />
      </StateCtx.Provider>,
    );

    const identity = await cardFor("My name is Wanjiru");
    expect(identity.textContent).toContain("About you");
    expect(identity.textContent).toContain("Kept");

    const routine = await cardFor("I run before work");
    expect(routine.textContent).toContain("Habit");
    expect(routine.textContent).not.toContain("Kept");
    expect(routine.textContent).not.toMatch(/long/i);

    const relationship = await cardFor("Amina is my sister");
    expect(relationship.textContent).toContain("People");
    expect(relationship.textContent).not.toMatch(/short/i);

    const unsorted = await cardFor("Something unsorted");
    expect(unsorted.querySelector(".ctx__category")).toBeNull();
    expect(unsorted.querySelector(".ctx__kept")).toBeNull();
    expect(unsorted.textContent).not.toMatch(/long/i);
  });
});
