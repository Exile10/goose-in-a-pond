import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { AttachmentTray } from "./AttachmentTray";
import type { PreparedImage } from "../lib/imageAttach";

afterEach(cleanup);

function image(overrides: Partial<PreparedImage> = {}): PreparedImage {
  return {
    data: "",
    mime_type: "image/jpeg",
    previewUrl: "blob:test-preview",
    width: 100,
    height: 100,
    byteSize: 1000,
    ...overrides,
  };
}

describe("AttachmentTray", () => {
  it("renders nothing when there are no attachments", () => {
    const { container } = render(<AttachmentTray attachments={[]} onRemove={() => {}} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders one thumbnail per attachment", () => {
    render(<AttachmentTray attachments={[image(), image(), image()]} onRemove={() => {}} />);
    expect(screen.getAllByRole("img")).toHaveLength(3);
  });

  it("shows a count label", () => {
    render(<AttachmentTray attachments={[image(), image()]} onRemove={() => {}} />);
    expect(screen.getByText("2 of 4")).toBeTruthy();
  });

  it("fires onRemove with the index of the clicked item", () => {
    const onRemove = vi.fn();
    render(<AttachmentTray attachments={[image(), image(), image()]} onRemove={onRemove} />);
    const removeButtons = screen.getAllByRole("button");
    fireEvent.click(removeButtons[1]);
    expect(onRemove).toHaveBeenCalledWith(1);
    expect(onRemove).toHaveBeenCalledTimes(1);
  });

  it("gives each remove button an accessible label naming its image", () => {
    render(<AttachmentTray attachments={[image(), image()]} onRemove={() => {}} />);
    expect(screen.getByLabelText("Remove attached image 1")).toBeTruthy();
    expect(screen.getByLabelText("Remove attached image 2")).toBeTruthy();
  });
});
