import { describe, expect, it } from "vitest";
import { signInView, splitAdvanced, type PlayerReply } from "./signInView";
import type { PlayerState } from "../player/types";

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
const reply = (over: Partial<PlayerState> = {}, attached = true): PlayerReply => ({
  attached,
  state: state(over),
});
const view = (
  r: PlayerReply | null,
  over: { desktop?: boolean; pending?: boolean } = {},
) => signInView({ desktop: true, pending: false, label: "Apple Music", reply: r, ...over });

describe("signInView", () => {
  it("offers the one button when the player is waiting for a sign-in", () => {
    expect(view(reply({ need: "authorization" }))).toEqual({ kind: "ready" });
  });

  it("shows signed in, and no button, once it is", () => {
    expect(view(reply({ need: "none", ready: true }))).toEqual({ kind: "signed_in" });
  });

  it("says a sign-in is under way while one is pending", () => {
    expect(view(reply(), { pending: true })).toEqual({ kind: "signing_in" });
  });

  it("ends the wait as soon as the player reports signed in", () => {
    expect(view(reply({ need: "none", ready: true }), { pending: true })).toEqual({
      kind: "signed_in",
    });
  });

  it("carries the reason a sign-in did not finish, so it is not a silent failure", () => {
    const failed = reply({ message: "Sign-in did not finish: the window was closed" });
    expect(view(failed)).toEqual({
      kind: "ready",
      note: "Sign-in did not finish: the window was closed",
    });
    expect(view(failed, { pending: true })).toMatchObject({
      kind: "signing_in",
      note: "Sign-in did not finish: the window was closed",
    });
  });

  it("does not offer a button that cannot work: setup comes first, in the adapter's own words", () => {
    const v = view(reply({ need: "setup", message: "Apple Music sign-in is not available on this pond yet." }));
    expect(v).toEqual({
      kind: "unavailable",
      note: "Apple Music sign-in is not available on this pond yet.",
    });
    expect(view(reply({ need: "setup" }))).toMatchObject({ kind: "unavailable" });
  });

  it("waits, rather than offering anything, until the player is there", () => {
    expect(view(null).kind).toBe("starting");
    expect(view(reply({}, false)).kind).toBe("starting");
    expect(view({ attached: true, state: null }).kind).toBe("starting");
  });

  it("sends a browser or a phone to the desktop app instead of a button that would do nothing", () => {
    const v = view(reply(), { desktop: false });
    expect(v.kind).toBe("not_in_shell");
    expect((v as { note: string }).note).toContain("Goose In A Pond app");
  });
});

describe("splitAdvanced", () => {
  it("separates advanced fields and keeps each group's order", () => {
    const fields = [
      { key: "A" },
      { key: "B", advanced: true },
      { key: "C", advanced: false },
      { key: "D", advanced: true },
    ];
    const { ordinary, advanced } = splitAdvanced(fields);
    expect(ordinary.map((f) => f.key)).toEqual(["A", "C"]);
    expect(advanced.map((f) => f.key)).toEqual(["B", "D"]);
  });

  it("treats a field with no flag as ordinary, which is what every other extension has", () => {
    expect(splitAdvanced<{ key: string; advanced?: boolean }>([{ key: "X" }]).ordinary).toHaveLength(1);
  });
});
