import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import "../i18n";

const shell = vi.hoisted(() => ({ desktop: false }));
vi.mock("../shell", () => ({ isDesktopShell: () => shell.desktop }));

import { SignInRequired } from "./SignInRequired";

afterEach(() => cleanup());

describe("SignInRequired", () => {
  it("sends a browser to the link `pond-server dashboard` prints", () => {
    shell.desktop = false;
    render(<SignInRequired />);
    expect(screen.getByText("pond-server dashboard")).toBeTruthy();
    expect(screen.getByText(/open the link it prints/)).toBeTruthy();
  });

  it("gives the desktop shell no link to open, only what to check", () => {
    shell.desktop = true;
    render(<SignInRequired />);
    expect(screen.getByText(/does not match the Pond/)).toBeTruthy();
    expect(screen.getByText(/Restart the app/)).toBeTruthy();
    expect(screen.queryByText("pond-server dashboard")).toBeNull();
    expect(screen.queryByText(/open the link/)).toBeNull();
    expect(screen.queryByText(/browser/)).toBeNull();
  });
});
