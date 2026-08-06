import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { Switch } from "@heroui/react";

/**
 * Guards the composition every toggle in the app uses.
 *
 * HeroUI 3 splits the switch into a field and a clickable `Switch.Content`
 * (React Aria's `SwitchButton`). Omitting Content renders a div with two spans:
 * no `switch` role, no accessible name, and clicks that reach nothing. Every
 * toggle in Settings, Skills and Extensions was in that state after the 3.x
 * upgrade, and no test noticed, because no test had ever operated a switch.
 */
afterEach(() => cleanup());

describe("Switch composition", () => {
  it("renders a control that is named and can be operated", () => {
    const onChange = vi.fn();
    render(
      <Switch aria-label="Enable the thing" isSelected={false} onChange={onChange}>
        <Switch.Content>
          <Switch.Control><Switch.Thumb /></Switch.Control>
        </Switch.Content>
      </Switch>,
    );

    // Named: without this a screen reader announces an unlabelled control.
    const control = screen.getByLabelText("Enable the thing");
    expect(control).toBeTruthy();
    expect(screen.getByRole("switch")).toBeTruthy();

    // Operable: the assertion that would have caught the regression.
    fireEvent.click(control);
    expect(onChange).toHaveBeenCalledTimes(1);
  });

  it("without Switch.Content there is nothing to click — the shape to avoid", () => {
    const onChange = vi.fn();
    render(
      <Switch aria-label="Inert" isSelected={false} onChange={onChange}>
        <Switch.Control><Switch.Thumb /></Switch.Control>
      </Switch>,
    );

    // Documented as a test so the difference is stated, not folklore.
    expect(screen.queryByRole("switch")).toBeNull();
    expect(screen.queryByLabelText("Inert")).toBeNull();
  });
});
