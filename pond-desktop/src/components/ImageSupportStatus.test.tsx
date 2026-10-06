import { describe, it, expect, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { ATTACH_BLOCKED_COPY, ImageSupportStatus, NOT_DECLARED_COPY } from "./ImageSupportStatus";
import type { VisionStatus } from "../api/types";

afterEach(cleanup);

describe("what a model that cannot look at pictures says", () => {
  it("words the refusal exactly as the pond does", () => {
    expect(NOT_DECLARED_COPY).toBe(
      "This model cannot look at pictures. To send one, choose a model marked Pictures included on the Models page.",
    );
    expect(ATTACH_BLOCKED_COPY).toBe(
      "The active model cannot read images. Switch to a model marked Pictures included on the Models page.",
    );
  });

  it("names the label the Models page shows, never one it does not", () => {
    for (const copy of [NOT_DECLARED_COPY, ATTACH_BLOCKED_COPY]) {
      expect(copy).toContain("marked Pictures included");
      expect(copy).not.toMatch(/Reads pictures/i);
    }
  });

  it("shows it under the composer once they reach for the paperclip, and not before", () => {
    const status: VisionStatus = { model: "x", state: { kind: "not_declared" }, size_bytes: null, message: null };
    const { rerender } = render(<ImageSupportStatus status={status} revealed={false} />);
    expect(screen.queryByRole("status")).toBeNull();
    rerender(<ImageSupportStatus status={status} revealed />);
    expect(screen.getByRole("status").textContent).toBe(NOT_DECLARED_COPY);
  });
});
