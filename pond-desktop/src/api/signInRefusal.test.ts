import { afterEach, describe, expect, it } from "vitest";
import type { DesktopShellApi } from "../shell";
import {
  REFUSAL_RETRY_CAP_MS,
  REFUSAL_RETRY_MS,
  SHELL_REFUSAL_GRACE,
  describeRefusal,
  refusalCounter,
  signInRefusalKey,
} from "./signInRefusal";

describe("signInRefusalKey", () => {
  it("sends a browser to the sign-in link and the shell to a restart", () => {
    expect(signInRefusalKey(false)).toBe("signIn.hostOnly");
    expect(signInRefusalKey(true)).toBe("signIn.shellMismatch");
  });

  it("follows the shell detection when not told", () => {
    // Vitest has no `window.giap`, so this is a browser.
    expect(signInRefusalKey()).toBe("signIn.hostOnly");
  });
});

describe("describeRefusal in a browser", () => {
  it("tells the user at once and does not ask again", () => {
    for (const count of [1, 2, 10]) {
      expect(describeRefusal(count, false)).toEqual({
        key: "signIn.hostOnly",
        notice: true,
        retryAfterMs: null,
      });
    }
  });
});

describe("describeRefusal in the shell", () => {
  it("lets the first few refusals pass quietly at the normal pace", () => {
    for (let count = 1; count <= SHELL_REFUSAL_GRACE; count++) {
      expect(describeRefusal(count, true), `refusal ${count}`).toEqual({
        key: "signIn.shellMismatch",
        notice: false,
        retryAfterMs: REFUSAL_RETRY_MS,
      });
    }
  });

  it("then says the credential does not match, and doubles the wait up to the cap", () => {
    const waits: number[] = [];
    for (let count = SHELL_REFUSAL_GRACE + 1; count <= SHELL_REFUSAL_GRACE + 6; count++) {
      const refusal = describeRefusal(count, true);
      expect(refusal.notice, `refusal ${count}`).toBe(true);
      expect(refusal.key).toBe("signIn.shellMismatch");
      waits.push(refusal.retryAfterMs ?? -1);
    }
    expect(waits).toEqual([10_000, 20_000, 40_000, REFUSAL_RETRY_CAP_MS, REFUSAL_RETRY_CAP_MS, REFUSAL_RETRY_CAP_MS]);
  });

  it("never waits past the cap, however many times it is refused", () => {
    expect(describeRefusal(10_000, true).retryAfterMs).toBe(REFUSAL_RETRY_CAP_MS);
  });
});

describe("refusalCounter", () => {
  afterEach(() => {
    delete window.giap;
  });

  it("treats every refusal as final in a browser", () => {
    const counter = refusalCounter();
    expect(counter.refused()).toEqual(describeRefusal(1, false));
    expect(counter.refused()).toEqual(describeRefusal(2, false));
  });

  it("counts the shell's refusals in a row, and starts again after one is admitted", () => {
    window.giap = { serverUrl: "http://127.0.0.1:4000" } as unknown as DesktopShellApi;
    const counter = refusalCounter();
    expect(counter.refused()).toEqual(describeRefusal(1, true));
    expect(counter.refused()).toEqual(describeRefusal(2, true));
    expect(counter.refused()).toEqual(describeRefusal(3, true));
    expect(counter.refused().retryAfterMs).toBe(20_000);

    counter.clear();
    expect(counter.refused()).toEqual(describeRefusal(1, true));
  });

  it("keeps a count per screen", () => {
    window.giap = { serverUrl: "http://127.0.0.1:4000" } as unknown as DesktopShellApi;
    const a = refusalCounter();
    const b = refusalCounter();
    for (let i = 0; i < 4; i++) a.refused();
    expect(b.refused().notice).toBe(false);
  });
});
