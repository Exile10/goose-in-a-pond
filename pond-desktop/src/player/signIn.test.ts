import { describe, expect, it, vi } from "vitest";
import { startSignIn } from "./signIn";
import type { PlayerAdapter, PlayerState } from "./types";

function adapter(
  service: string,
  label: string,
  over: Partial<PlayerState>,
  authorize = vi.fn<() => Promise<void>>(async () => undefined),
) {
  const state: PlayerState = {
    service,
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
  return { adapter: { service, label, state: () => state, authorize } as unknown as PlayerAdapter, authorize };
}

describe("startSignIn", () => {
  it("begins the sign-in of a service that is waiting for one, without waiting for it to finish", () => {
    let finish: () => void = () => undefined;
    const authorize = vi.fn<() => Promise<void>>(() => new Promise<void>((resolve) => (finish = resolve)));
    const apple = adapter("apple", "Apple Music", { need: "authorization" }, authorize);

    const result = startSignIn([apple.adapter], "apple");

    expect(result).toEqual({ started: true });
    expect(authorize).toHaveBeenCalledOnce();
    finish();
  });

  it("does not ask a signed-in service to sign in again", () => {
    const apple = adapter("apple", "Apple Music", { need: "none", ready: true });
    expect(startSignIn([apple.adapter], "apple")).toEqual({
      started: false,
      message: "Apple Music is already signed in.",
    });
    expect(apple.authorize).not.toHaveBeenCalled();
  });

  it("says what is missing for a service that is not set up, and does not try", () => {
    const apple = adapter("apple", "Apple Music", { need: "setup", message: "No shared credentials." });
    expect(startSignIn([apple.adapter], "apple")).toEqual({
      started: false,
      message: "No shared credentials.",
    });
    expect(apple.authorize).not.toHaveBeenCalled();
    const bare = adapter("apple", "Apple Music", { need: "setup" });
    expect(startSignIn([bare.adapter], "apple").message).toContain("not set up");
  });

  it("names a service it has no player for", () => {
    const apple = adapter("apple", "Apple Music", {});
    expect(startSignIn([apple.adapter], "tidal")).toEqual({
      started: false,
      message: "There is no player for tidal.",
    });
  });

  it("picks the right service among several", () => {
    const apple = adapter("apple", "Apple Music", {});
    const spotify = adapter("spotify", "Spotify", { need: "none", ready: true });
    startSignIn([spotify.adapter, apple.adapter], "apple");
    expect(apple.authorize).toHaveBeenCalledOnce();
    expect(spotify.authorize).not.toHaveBeenCalled();
  });

  it("does not let a sign-in that rejects become an unhandled rejection", async () => {
    const authorize = vi.fn<() => Promise<void>>(async () => {
      throw new Error("popup closed");
    });
    const apple = adapter("apple", "Apple Music", {}, authorize);
    expect(startSignIn([apple.adapter], "apple")).toEqual({ started: true });
    await Promise.resolve();
    await Promise.resolve();
  });
});
