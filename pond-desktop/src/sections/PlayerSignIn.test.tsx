import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { PlayerState } from "../player/types";

const shell = vi.hoisted(() => ({
  desktop: true,
  invoke: vi.fn(),
}));
const getPlayerState = vi.hoisted(() => vi.fn());

vi.mock("../shell", () => ({
  isDesktopShell: () => shell.desktop,
  invoke: shell.invoke,
}));
vi.mock("../api/PondApiClient", () => ({ api: { getPlayerState } }));

import { PlayerSignIn } from "./PlayerSignIn";

function state(over: Partial<PlayerState> = {}): PlayerState {
  return {
    service: "apple",
    ready: false,
    need: "authorization",
    status: "idle",
    track: null,
    position_ms: 0,
    volume: 100,
    shuffle: false,
    repeat: "off",
    ...over,
  };
}
const reports = (over: Partial<PlayerState> = {}) =>
  getPlayerState.mockResolvedValue({ attached: true, state: state(over) });

async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  shell.desktop = true;
  shell.invoke.mockReset();
  getPlayerState.mockReset();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("PlayerSignIn", () => {
  it("is one button when the player is waiting for a sign-in", async () => {
    reports({ need: "authorization" });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    expect(screen.getAllByRole("button")).toHaveLength(1);
    expect(screen.getByRole("button", { name: /sign in to apple music/i })).toBeTruthy();
  });

  it("starts the sign-in in the player window with one click, then follows it to signed in", async () => {
    reports({ need: "authorization" });
    shell.invoke.mockResolvedValue({ started: true });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: /sign in to apple music/i }));
    await settle();

    expect(shell.invoke).toHaveBeenCalledWith("player_authorize", { service: "apple" });
    expect(screen.getByText(/waiting for apple music's sign-in window/i)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();

    // The person finishes in Apple's popup; the player reports it.
    reports({ need: "none", ready: true });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_600);
    });
    expect(screen.getByText(/signed in to apple music/i)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("says why when the sign-in could not be started, and keeps the button", async () => {
    reports({ need: "authorization" });
    shell.invoke.mockResolvedValue({ started: false, message: "The music player is still starting." });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    fireEvent.click(screen.getByRole("button", { name: /sign in to apple music/i }));
    await settle();

    expect(screen.getByText("The music player is still starting.")).toBeTruthy();
    expect(screen.getByRole("button", { name: /sign in to apple music/i })).toBeTruthy();
  });

  it("shows why it cannot sign in, and no button, when Apple Music is not set up on this pond", async () => {
    reports({ need: "setup", message: "Apple Music sign-in is not available on this pond yet." });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    expect(screen.getByText("Apple Music sign-in is not available on this pond yet.")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
    expect(shell.invoke).not.toHaveBeenCalled();
  });

  it("shows signed in, and no button, when the player is already signed in", async () => {
    reports({ need: "none", ready: true });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    expect(screen.getByText(/signed in to apple music/i)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("waits quietly while the player has not come up", async () => {
    getPlayerState.mockResolvedValue({ attached: false, state: null });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    expect(screen.getByText(/music player is starting/i)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("does not offer a button outside the desktop app, and does not ask the player", async () => {
    shell.desktop = false;
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    expect(screen.getByText(/from the goose in a pond app/i)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
    expect(getPlayerState).not.toHaveBeenCalled();
  });

  it("gives up after three minutes with a reason instead of waiting for ever", async () => {
    reports({ need: "authorization" });
    shell.invoke.mockResolvedValue({ started: true });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: /sign in to apple music/i }));
    await settle();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3 * 60 * 1000 + 2_000);
    });

    expect(screen.getByText(/did not finish signing in/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /sign in to apple music/i })).toBeTruthy();
  });

  it("shows the reason the last sign-in failed, as the adapter reported it", async () => {
    reports({ need: "authorization", message: "Sign-in did not finish: the window was closed" });
    render(<PlayerSignIn service="apple" label="Apple Music" />);
    await settle();

    expect(screen.getByText("Sign-in did not finish: the window was closed")).toBeTruthy();
    expect(screen.getByRole("button", { name: /sign in to apple music/i })).toBeTruthy();
  });
});
