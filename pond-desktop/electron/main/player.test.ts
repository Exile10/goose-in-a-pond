import { describe, expect, it } from "vitest";
import { authorizeScript, isAppleSignInUrl, readAuthorizeReply, widevineComponents } from "./player";

describe("isAppleSignInUrl", () => {
  it("lets Apple's own https pages open as the sign-in popup", () => {
    expect(isAppleSignInUrl("https://authorize.music.apple.com/woa?x=1")).toBe(true);
    expect(isAppleSignInUrl("https://idmsa.apple.com/appleauth/auth")).toBe(true);
    expect(isAppleSignInUrl("https://apple.com/")).toBe(true);
  });

  it("refuses everything else, including look-alikes", () => {
    for (const url of [
      "http://authorize.music.apple.com/",
      "https://apple.com.evil.example/",
      "https://evilapple.com/",
      "https://example.com/?u=https://apple.com/",
      "javascript:alert(1)",
      "file:///etc/passwd",
      "not a url",
    ]) {
      expect(isAppleSignInUrl(url), url).toBe(false);
    }
  });
});

describe("widevineComponents", () => {
  it("is absent on stock Electron, where the player then says DRM is unavailable", () => {
    // Under Vitest there is no Electron runtime at all, which is the same "no components API" case.
    expect(widevineComponents()).toBeUndefined();
  });
});

describe("authorizeScript", () => {
  it("passes the service as data, so a hostile name cannot become code", () => {
    const script = authorizeScript('apple"); alert(1); ("');
    expect(script).toContain(JSON.stringify('apple"); alert(1); ("'));
    // The only text between the quotes of the argument is the escaped name.
    expect(script).toMatch(/authorize\("apple\\"\); alert\(1\); \(\\""\)/);
  });

  it("answers, rather than throwing, when the page has not defined its hook yet", () => {
    // The script runs in the page, where `window` exists; here it is given an empty one.
    const run = new Function("window", `return ${authorizeScript("apple")}`) as (w: unknown) => unknown;
    expect(run({})).toEqual({
      started: false,
      message: "The music player is still starting. Try again in a moment.",
    });
  });

  it("calls the page's hook with the service when it is there", () => {
    const seen: string[] = [];
    const run = new Function("window", `return ${authorizeScript("apple")}`) as (w: unknown) => unknown;
    const answer = run({ __giapPlayer: { authorize: (s: string) => (seen.push(s), { started: true }) } });
    expect(seen).toEqual(["apple"]);
    expect(answer).toEqual({ started: true });
  });
});

describe("readAuthorizeReply", () => {
  it("passes on a start, and only a start", () => {
    expect(readAuthorizeReply({ started: true, extra: "ignored" })).toEqual({ started: true });
    expect(readAuthorizeReply({ started: "yes" })).toEqual({
      started: false,
      message: "The sign-in could not be started.",
    });
  });

  it("carries the page's reason when it gave one, and a plain one when it did not", () => {
    expect(readAuthorizeReply({ started: false, message: "Apple Music is already signed in." })).toEqual({
      started: false,
      message: "Apple Music is already signed in.",
    });
    expect(readAuthorizeReply(undefined)).toEqual({
      started: false,
      message: "The sign-in could not be started.",
    });
    expect(readAuthorizeReply({ started: false, message: 42 }).message).toBe(
      "The sign-in could not be started.",
    );
  });
});
