import { describe, expect, it, vi } from "vitest";
import { needsPerson, startSignIn } from "./signIn";
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
  it("begins the sign-in of a service that is waiting for one, without waiting for it to finish", async () => {
    let finish: () => void = () => undefined;
    const authorize = vi.fn<() => Promise<void>>(() => new Promise<void>((resolve) => (finish = resolve)));
    const apple = adapter("apple", "Apple Music", { need: "authorization" }, authorize);

    const result = await startSignIn([apple.adapter], "apple");

    expect(result).toEqual({ started: true });
    expect(authorize).toHaveBeenCalledOnce();
    finish();
  });

  it("does not ask a signed-in service to sign in again", async () => {
    const apple = adapter("apple", "Apple Music", { need: "none", ready: true });
    expect(await startSignIn([apple.adapter], "apple")).toEqual({
      started: false,
      message: "Apple Music is already signed in.",
    });
    expect(apple.authorize).not.toHaveBeenCalled();
  });

  it("says what is missing for a service that is not set up, and does not try", async () => {
    const apple = adapter("apple", "Apple Music", { need: "setup", message: "No shared credentials." });
    expect(await startSignIn([apple.adapter], "apple")).toEqual({
      started: false,
      message: "No shared credentials.",
    });
    expect(apple.authorize).not.toHaveBeenCalled();
    const bare = adapter("apple", "Apple Music", { need: "setup" });
    expect((await startSignIn([bare.adapter], "apple")).message).toContain("not set up");
  });

  it("names a service it has no player for", async () => {
    const apple = adapter("apple", "Apple Music", {});
    expect(await startSignIn([apple.adapter], "tidal")).toEqual({
      started: false,
      message: "There is no player for tidal.",
    });
  });

  it("picks the right service among several", async () => {
    const apple = adapter("apple", "Apple Music", {});
    const spotify = adapter("spotify", "Spotify", { need: "none", ready: true });
    await startSignIn([spotify.adapter, apple.adapter], "apple");
    expect(apple.authorize).toHaveBeenCalledOnce();
    expect(spotify.authorize).not.toHaveBeenCalled();
  });

  it("does not let a sign-in that rejects become an unhandled rejection", async () => {
    const authorize = vi.fn<() => Promise<void>>(async () => {
      throw new Error("popup closed");
    });
    const apple = adapter("apple", "Apple Music", {}, authorize);
    expect(await startSignIn([apple.adapter], "apple")).toEqual({ started: true });
    await Promise.resolve();
    await Promise.resolve();
  });

  it("wakes a sleeping service first, and only then opens its sign-in", async () => {
    const order: string[] = [];
    const apple = adapter("apple", "Apple Music", { need: "authorization", dormant: true });
    (apple.adapter as unknown as { prepare: () => Promise<void> }).prepare = async () => {
      order.push("prepare");
    };
    apple.authorize.mockImplementation(async () => {
      order.push("authorize");
    });

    expect(await startSignIn([apple.adapter], "apple")).toEqual({ started: true });
    await Promise.resolve();

    expect(order).toEqual(["prepare", "authorize"]);
  });

  it("tells the person now, and opens nothing, when waking the service fails", async () => {
    const apple = adapter("apple", "Apple Music", { need: "authorization", dormant: true });
    (apple.adapter as unknown as { prepare: () => Promise<void> }).prepare = async () => {
      throw new Error("Apple Music could not get its sign-in token: the credentials service could not be reached.");
    };

    const result = await startSignIn([apple.adapter], "apple");

    expect(result).toEqual({
      started: false,
      message: "Apple Music could not get its sign-in token: the credentials service could not be reached.",
    });
    expect(apple.authorize).not.toHaveBeenCalled();
  });

  it("still works for a service with nothing to wake", async () => {
    const spotify = adapter("spotify", "Spotify", { need: "authorization" });
    expect(await startSignIn([spotify.adapter], "spotify")).toEqual({ started: true });
  });
});

describe("needsPerson", () => {
  const base = { service: "apple", ready: false, status: "idle", track: null, position_ms: 0, volume: 100, shuffle: false, repeat: "off" } as const;

  it("raises the window only for a service that is actually asking", () => {
    expect(needsPerson({ ...base, need: "authorization" } as PlayerState)).toBe(true);
    expect(needsPerson({ ...base, need: "none", ready: true } as PlayerState)).toBe(false);
    expect(needsPerson({ ...base, need: "setup" } as PlayerState)).toBe(false);
  });

  it("does not raise it for a service that is asleep, so no window appears at launch", () => {
    expect(needsPerson({ ...base, need: "authorization", dormant: true } as PlayerState)).toBe(false);
  });
});
