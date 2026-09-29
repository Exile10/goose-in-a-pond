import { describe, expect, it } from "vitest";
import { isAppleSignInUrl, widevineComponents } from "./player";

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
