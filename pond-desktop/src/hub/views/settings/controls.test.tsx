// ────────────────────────────────────────────────────────────
// Toggle — reflects the stored value, not just the last click.
//
// Settings load asynchronously, so a Toggle mounts before its stored value
// arrives. It also has to fall back when a save fails. Previously the switch
// seeded local state once and ignored the prop afterwards, so it silently
// misreported what was actually stored.
// ────────────────────────────────────────────────────────────

import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Toggle } from "./controls";

afterEach(() => cleanup());

const sw = () => screen.getByRole("button");

describe("Toggle", () => {
  it("adopts the stored value when settings arrive after mount", () => {
    // Mounts off (settings not loaded yet)…
    const { rerender } = render(<Toggle on={false} />);
    expect(sw().getAttribute("aria-pressed")).toBe("false");

    // …then the real value loads.
    rerender(<Toggle on={true} />);
    expect(sw().getAttribute("aria-pressed")).toBe("true");
  });

  it("returns to the previous value when a save fails and the parent reverts", () => {
    // Mirrors the real handler: optimistic update, then revert if the save
    // rejects. The switch must follow the parent both ways.
    const { rerender } = render(<Toggle on={false} />);

    rerender(<Toggle on={true} />);   // optimistic
    expect(sw().getAttribute("aria-pressed")).toBe("true");

    rerender(<Toggle on={false} />);  // save failed → reverted
    expect(sw().getAttribute("aria-pressed")).toBe("false");
  });

  it("never moves on its own — the stored value decides", () => {
    // A caller that does not update state must not see the switch drift out of
    // sync with what is actually stored.
    const onChange = vi.fn();
    render(<Toggle on={false} onChange={onChange} />);

    fireEvent.click(sw());
    expect(onChange).toHaveBeenCalledWith(true);
    expect(sw().getAttribute("aria-pressed")).toBe("false");
  });

  it("reports the new value on click", () => {
    const onChange = vi.fn();
    render(<Toggle on={false} onChange={onChange} />);

    fireEvent.click(sw());
    expect(onChange).toHaveBeenCalledWith(true);
  });

  it("ignores clicks while disabled", () => {
    const onChange = vi.fn();
    render(<Toggle on={false} onChange={onChange} disabled />);

    fireEvent.click(sw());
    expect(onChange).not.toHaveBeenCalled();
    expect(sw().getAttribute("aria-pressed")).toBe("false");
  });
});
