import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { TurnStatsFooter } from "./TurnStatsFooter";
import type { TurnStats } from "../api/types";

afterEach(cleanup);

function stats(overrides: Partial<TurnStats> = {}): TurnStats {
  return {
    type: "turn_stats",
    ttft_ms: 412,
    prefill_ms: 3100,
    decode_tok_per_sec: 22.34,
    prefill_tok_per_sec: 594.0,
    prefilled_tokens: 1843,
    reused_prefix_tokens: 0,
    prompt_tokens: 1843,
    completion_tokens: 87,
    context_used_tokens: 1843,
    context_limit_tokens: 3072,
    context_pct: 60.0,
    model_load_ms: null,
    inference_count: 1,
    ...overrides,
  };
}

describe("TurnStatsFooter", () => {
  it("renders the full stats line with thousands separators", () => {
    render(<TurnStatsFooter stats={stats()} />);
    const el = document.querySelector(".turn-stats-footer");
    expect(el).not.toBeNull();
    const text = el!.textContent ?? "";
    expect(text).toContain("0.4s to first token");
    expect(text).toContain("22.3 tok/s");
    expect(text).toContain("ctx 1,843/3,072 (60%)");
  });

  it("omits null segments instead of rendering placeholders", () => {
    render(
      <TurnStatsFooter
        stats={stats({
          ttft_ms: null,
          decode_tok_per_sec: null,
          context_used_tokens: null,
          context_limit_tokens: null,
          context_pct: null,
        })}
      />,
    );
    // Every optional segment gone and inference_count is 1 -> nothing to show.
    expect(document.querySelector(".turn-stats-footer")).toBeNull();
  });

  it("shows model load only when present and positive", () => {
    render(<TurnStatsFooter stats={stats({ model_load_ms: 4200 })} />);
    expect(screen.getByText(/model load 4\.2s/)).toBeTruthy();
    cleanup();
    render(<TurnStatsFooter stats={stats({ model_load_ms: 0 })} />);
    expect(document.body.textContent).not.toContain("model load");
  });

  it("shows the inference count only for multi-inference turns", () => {
    render(<TurnStatsFooter stats={stats({ inference_count: 3 })} />);
    expect(screen.getByText(/3 inferences/)).toBeTruthy();
    cleanup();
    render(<TurnStatsFooter stats={stats({ inference_count: 1 })} />);
    expect(document.body.textContent).not.toContain("inferences");
  });

  it("separates segments with a middle dot", () => {
    render(<TurnStatsFooter stats={stats()} />);
    const text = document.querySelector(".turn-stats-footer")!.textContent ?? "";
    expect(text.split(" · ").length).toBeGreaterThanOrEqual(3);
  });
});
