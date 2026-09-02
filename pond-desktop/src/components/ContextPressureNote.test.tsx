import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ContextPressureNote } from "./ContextPressureNote";
import { api } from "../api/PondApiClient";
import { TURNS_REMAINING_UNKNOWN } from "../api/types";
import type { CompactionReport, ContextWarning } from "../api/types";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const SRC_DIR = dirname(dirname(fileURLToPath(import.meta.url)));

/**
 * The `context_warning` frame verbatim as `routes.rs` serialises it. Keys are
 * copied from the `json!` literal in the chat-stream generator, not invented
 * here: a fixture that drifts from the wire tests a system that does not exist.
 */
function frame(overrides: Partial<ContextWarning> = {}): ContextWarning {
  return {
    type: "context_warning",
    utilization_pct: 82.4,
    turns_remaining: 2,
    avg_growth_rate: 640,
    warning: "Context is 82% full - consider starting a new session",
    ...overrides,
  };
}

function report(overrides: Partial<CompactionReport> = {}): CompactionReport {
  return {
    session_id: "s-1",
    status: "skipped",
    reason: "cooling_down",
    outcome: null,
    context: {
      utilization_pct: 82.4,
      turns_remaining: 2,
      avg_growth_rate: 640,
      should_compact: true,
      warning: "Context is 82% full - consider starting a new session",
    },
    ...overrides,
  };
}

describe("ContextPressureNote", () => {
  it("renders a utilisation line when the server sent no warning sentence", () => {
    // `should_compact` also fires through the `estimated_turns_remaining < 3`
    // limb, which is reachable BELOW the 60% threshold that populates
    // `warning`. So `warning: null` on this frame is a producible state and
    // the component must still say something useful.
    render(
      <ContextPressureNote
        warning={frame({ warning: null, utilization_pct: 41.6, turns_remaining: 2 })}
        sessionId="s-1"
      />,
    );
    const text = document.querySelector(".ctx-pressure")!.textContent ?? "";
    expect(
      text,
      "a context_warning with warning:null must still render a utilisation line - " +
        "the below-threshold should_compact path produces exactly that frame",
    ).toContain("42%");
    expect(text).toContain("2 turns left");
  });

  it("never prints the u32::MAX turns-remaining sentinel", () => {
    render(
      <ContextPressureNote
        warning={frame({ turns_remaining: TURNS_REMAINING_UNKNOWN })}
        sessionId="s-1"
      />,
    );
    const text = document.querySelector(".ctx-pressure")!.textContent ?? "";
    expect(
      text,
      "the SSE frame serialises 'growth unknown' as the raw u32::MAX; printing " +
        "4294967295 turns to a user is worse than saying nothing",
    ).not.toContain("4294967295");
    expect(text).not.toContain("turns left");
  });

  it("renders a refusal as a neutral note, not an error", async () => {
    vi.spyOn(api, "compactSession").mockResolvedValue(report());
    render(<ContextPressureNote warning={frame()} sessionId="s-1" />);

    fireEvent.click(screen.getByRole("button", { name: /compact now/i }));

    await waitFor(() => {
      expect(document.querySelector(".ctx-pressure__result")).not.toBeNull();
    });
    const result = document.querySelector(".ctx-pressure__result")!;
    // The three assertions that name the defect come first, so a mutation that
    // turns a refusal into a failure reports the reason rather than a diff.
    expect(
      /error/i.test(document.body.textContent ?? ""),
      "a refusal rendered as an error: 'cooling_down' is information, not a " +
        "fault - the endpoint answers 200 with a status/reason pair and being " +
        "refused is the COMMON path, because it deliberately does not bypass " +
        "the pressure axis's rate limiter",
    ).toBe(false);
    expect(
      document.querySelector('[role="alert"]'),
      "a refusal rendered as an error: an alert role interrupts a screen " +
        "reader for a normal answer",
    ).toBeNull();
    expect(
      result.getAttribute("aria-live"),
      "the refusal note must be announced politely, not as an alert",
    ).toBe("polite");
    expect(result.textContent).toContain("A compaction ran recently");
  });

  it("disables the control until a session id exists", () => {
    render(<ContextPressureNote warning={frame()} sessionId={null} />);
    expect(
      screen.getByRole("button", { name: /compact now/i }).hasAttribute("disabled"),
      "the frame carries no session_id and on a first turn the id only arrives " +
        "with `done`, so the button must not post to /sessions/undefined/compact",
    ).toBe(true);
  });

  it("reports a compacted pass", async () => {
    vi.spyOn(api, "compactSession").mockResolvedValue(
      report({ status: "compacted", reason: null, outcome: "refreshed" }),
    );
    render(<ContextPressureNote warning={frame()} sessionId="s-1" />);
    fireEvent.click(screen.getByRole("button", { name: /compact now/i }));
    await waitFor(() => {
      expect(document.querySelector(".ctx-pressure__result")?.textContent).toContain(
        "Compacted",
      );
    });
  });
});

/**
 * A CHEAP TRIPWIRE, not coverage — and the distinction is on the record.
 *
 * Round 1 called this "the assertion that would go red if PAI-4 P7b were
 * reverted". Synthesis corrected that: it goes red only for a revert done with
 * a delete key. Two semantic mutations leave both substrings in place and pass
 * — adding `showTurnStats &&` to the render guard (which ships the note
 * invisible on every default install, `show_turn_stats` being false in Rust),
 * and attaching the frame to a message id that does not exist.
 *
 * The coverage now lives in `sections/Chat.test.tsx`, which mounts the real
 * component, drives a real `context_warning` frame through it and asserts a
 * real `.ctx-pressure` node; both mutations above go red there. This stays
 * because it is free and it does catch a deletion, and because `ChatHub.tsx`
 * still has no mount harness of its own — for that surface a grep is all there
 * is, and saying so is better than implying otherwise.
 *
 * The two halves are now checked in two places, because they live in two.
 * Reading the frame moved to `state/chatRunStore.ts` when the turn was hoisted
 * out of the components so it could survive leaving the section; rendering the
 * note is still each surface's own. Asserting them together against a surface
 * file would only prove the two had been copied back into it, which is the
 * arrangement that let the surfaces drift in the first place.
 */
describe("context_warning has a consumer", () => {
  it("the shared turn driver reads the frame", () => {
    const src = readFileSync(join(SRC_DIR, "state/chatRunStore.ts"), "utf8");
    expect(
      src.includes('ev.type === "context_warning"'),
      "the frame has no consumer in the turn driver - the server emits " +
        "context_warning under a default-true setting and every surface drops it",
    ).toBe(true);
  });

  for (const surface of ["hub/views/ChatHub.tsx", "sections/Chat.tsx"]) {
    it(`${surface} renders the note`, () => {
      const src = readFileSync(join(SRC_DIR, surface), "utf8");
      expect(
        src.includes("<ContextPressureNote"),
        `${surface} is handed context_warning on the message but renders ` +
          "nothing for it",
      ).toBe(true);
      expect(
        src.includes("contextWarning"),
        `${surface} never reads the warning off the message, so the note it ` +
          "renders cannot be this turn's",
      ).toBe(true);
    });
  }

  it("the frame type is in the ChatEventType union", () => {
    const src = readFileSync(join(SRC_DIR, "api/types.ts"), "utf8");
    const union = src.match(/export type ChatEventType =[^;]+;/)?.[0] ?? "";
    expect(
      union.includes('"context_warning"'),
      "context_warning is absent from ChatEventType, so every consumer is " +
        "reading an untyped frame",
    ).toBe(true);
  });
});
