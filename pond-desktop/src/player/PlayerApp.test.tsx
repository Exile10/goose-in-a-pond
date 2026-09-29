import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { PlayerApp } from "./PlayerApp";
import type { PlayerAdapter, PlayerState } from "./types";

afterEach(cleanup);

function state(over: Partial<PlayerState> = {}): PlayerState {
  return {
    service: "apple",
    ready: true,
    need: "none",
    status: "idle",
    track: null,
    position_ms: 0,
    volume: 50,
    shuffle: false,
    repeat: "off",
    ...over,
  };
}

function adapterIn(initial: PlayerState) {
  let current = initial;
  const listeners = new Set<(s: PlayerState) => void>();
  const spies = {
    authorize: vi.fn(async () => undefined),
    resume: vi.fn(async () => undefined),
    pause: vi.fn(async () => undefined),
    next: vi.fn(async () => undefined),
    previous: vi.fn(async () => undefined),
  };
  const adapter = {
    service: "apple",
    label: "Apple Music",
    capabilities: { queue: true, playlists: true, library: true },
    state: () => current,
    onState: (l: (s: PlayerState) => void) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    ...spies,
  } as unknown as PlayerAdapter;
  return {
    adapter,
    spies,
    update(next: PlayerState) {
      current = next;
      act(() => listeners.forEach((l) => l(next)));
    },
  };
}

const track = {
  id: "1",
  kind: "song" as const,
  title: "Nairobi",
  artist: "Bensoul",
  album: "Qwarantunes",
  duration_ms: 210_000,
};

describe("PlayerApp", () => {
  it("offers one thing to do when nobody is signed in", () => {
    const { adapter, spies } = adapterIn(state({ ready: false, need: "authorization" }));
    render(<PlayerApp adapter={adapter} />);

    fireEvent.click(screen.getByRole("button", { name: "Connect Apple Music" }));

    expect(spies.authorize).toHaveBeenCalledOnce();
    expect(screen.queryByLabelText("Play")).toBeNull();
  });

  it("says what is missing when the player cannot be set up", () => {
    const { adapter } = adapterIn(
      state({
        ready: false,
        need: "setup",
        message: "Apple Music is not set up: add your Team ID.",
      }),
    );
    render(<PlayerApp adapter={adapter} />);
    expect(screen.getByText(/add your Team ID/)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("still points to the settings when the reason is not known", () => {
    const { adapter } = adapterIn(state({ ready: false, need: "setup" }));
    render(<PlayerApp adapter={adapter} />);
    expect(screen.getByText(/not set up yet/)).toBeTruthy();
  });

  it("invites a request when nothing is playing", () => {
    const { adapter } = adapterIn(state());
    render(<PlayerApp adapter={adapter} />);
    expect(screen.getByText(/Nothing is playing/)).toBeTruthy();
  });

  it("shows what is playing, with its clock", () => {
    const { adapter } = adapterIn(state({ status: "playing", track, position_ms: 65_000 }));
    render(<PlayerApp adapter={adapter} />);
    expect(screen.getByText("Nairobi")).toBeTruthy();
    expect(screen.getByText("Bensoul - Qwarantunes")).toBeTruthy();
    expect(screen.getByText("1:05 / 3:30")).toBeTruthy();
  });

  it("pauses while playing and resumes while paused", () => {
    const { adapter, spies, update } = adapterIn(state({ status: "playing", track }));
    render(<PlayerApp adapter={adapter} />);

    fireEvent.click(screen.getByLabelText("Pause"));
    expect(spies.pause).toHaveBeenCalledOnce();

    update(state({ status: "paused", track }));
    fireEvent.click(screen.getByLabelText("Play"));
    expect(spies.resume).toHaveBeenCalledOnce();
  });

  it("skips in both directions", () => {
    const { adapter, spies } = adapterIn(state({ status: "playing", track }));
    render(<PlayerApp adapter={adapter} />);
    fireEvent.click(screen.getByLabelText("Next"));
    fireEvent.click(screen.getByLabelText("Previous"));
    expect(spies.next).toHaveBeenCalledOnce();
    expect(spies.previous).toHaveBeenCalledOnce();
  });

  it("puts a refusal in words where it can be read", () => {
    const { adapter } = adapterIn(
      state({ status: "error", message: "Apple refused the playback license (MEDIA_LICENSE)." }),
    );
    render(<PlayerApp adapter={adapter} />);
    expect(screen.getByRole("status").textContent).toContain("Apple refused");
  });

  it("a control that fails does not crash the window", async () => {
    const { adapter, spies } = adapterIn(state({ status: "playing", track }));
    spies.pause.mockRejectedValueOnce(new Error("no"));
    render(<PlayerApp adapter={adapter} />);
    fireEvent.click(screen.getByLabelText("Pause"));
    await Promise.resolve();
    expect(screen.getByText("Nairobi")).toBeTruthy();
  });
});
