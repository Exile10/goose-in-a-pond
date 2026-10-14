import { beforeEach, describe, expect, it, vi } from "vitest";
import type { WebContents } from "electron";

const { openExternal } = vi.hoisted(() => ({ openExternal: vi.fn(async () => undefined) }));
vi.mock("electron", () => ({ shell: { openExternal } }));

import { appOrigins, devServerFromEnv, guardNavigation, isAppUrl, isWebUrl } from "./navigation";
import { APP_ORIGIN } from "./protocol";

describe("isAppUrl", () => {
  const packaged = appOrigins(undefined);

  it("accepts any page on app://giap", () => {
    expect(packaged).toEqual([APP_ORIGIN]);
    expect(isAppUrl("app://giap/index.html", packaged)).toBe(true);
    expect(isAppUrl("app://giap/settings?tab=remote#host=abc", packaged)).toBe(true);
  });

  it("rejects every other origin and anything unparseable", () => {
    for (const url of ["app://evil/index.html", "https://giap/", "http://localhost:1420/", "file:///index.html", "about:blank", "", "not a url"]) {
      expect(isAppUrl(url, packaged), url).toBe(false);
    }
  });

  it("accepts the dev server's exact origin only when it is configured", () => {
    const dev = appOrigins("http://localhost:1420/");
    expect(isAppUrl("http://localhost:1420/src/main.tsx", dev)).toBe(true);
    expect(isAppUrl("http://localhost:1421/", dev)).toBe(false);
    expect(isAppUrl("https://localhost:1420/", dev)).toBe(false);
    expect(isAppUrl("app://giap/index.html", dev)).toBe(true);
  });
});

describe("devServerFromEnv", () => {
  it("passes an http(s) dev server through, and says nothing when none is set", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    expect(devServerFromEnv("http://localhost:1420")).toBe("http://localhost:1420");
    expect(devServerFromEnv("https://dev.localhost:1420/")).toBe("https://dev.localhost:1420/");
    expect(devServerFromEnv(undefined)).toBeUndefined();
    expect(devServerFromEnv("")).toBeUndefined();
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("warns about a malformed value and ignores it instead of throwing at startup", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    // `localhost:1420` parses as a URL with the scheme `localhost:`; `file:` would allow any local page.
    for (const value of ["not a url", "localhost:1420", "file:///", "javascript:alert(1)", "http://"]) {
      warn.mockClear();
      expect(() => devServerFromEnv(value), value).not.toThrow();
      expect(devServerFromEnv(value), value).toBeUndefined();
      expect(warn, value).toHaveBeenCalledWith(expect.stringContaining("GIAP_DEV_SERVER"));
    }
    warn.mockRestore();
  });

  it("leaves the app with its normal origins when the value is ignored", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    expect(appOrigins(devServerFromEnv("not a url"))).toEqual([APP_ORIGIN]);
    warn.mockRestore();
  });
});

describe("isWebUrl", () => {
  it("is http(s) only", () => {
    expect(isWebUrl("https://example.com/")).toBe(true);
    expect(isWebUrl("http://192.168.1.5:4000/")).toBe(true);
    for (const url of ["file:///etc/passwd", "smb://host/share", "javascript:alert(1)", "not a url"]) {
      expect(isWebUrl(url), url).toBe(false);
    }
  });
});

describe("guardNavigation", () => {
  type Listener = (event: { url: string; isMainFrame: boolean; preventDefault: () => void }) => void;
  let listeners: Map<string, Listener>;
  let openHandler: (details: { url: string }) => { action: string };

  beforeEach(() => {
    vi.clearAllMocks();
    listeners = new Map();
    const contents = {
      on: (name: string, fn: Listener) => listeners.set(name, fn),
      setWindowOpenHandler: (fn: typeof openHandler) => { openHandler = fn; },
    };
    guardNavigation(contents as unknown as WebContents, appOrigins(undefined));
  });

  function navigate(kind: "will-navigate" | "will-redirect", url: string, isMainFrame = true) {
    const event = { url, isMainFrame, preventDefault: vi.fn() };
    listeners.get(kind)!(event);
    return event.preventDefault;
  }

  it("lets the main frame move between the app's own pages", () => {
    expect(navigate("will-navigate", "app://giap/index.html")).not.toHaveBeenCalled();
    expect(openExternal).not.toHaveBeenCalled();
  });

  it("keeps an outside http(s) page out of the window and opens it in the browser", () => {
    for (const kind of ["will-navigate", "will-redirect"] as const) {
      expect(navigate(kind, "https://evil.example/")).toHaveBeenCalledTimes(1);
    }
    expect(openExternal).toHaveBeenCalledTimes(2);
    expect(openExternal).toHaveBeenCalledWith("https://evil.example/");
  });

  it("cancels any other scheme without opening it anywhere", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    expect(navigate("will-navigate", "file:///etc/passwd")).toHaveBeenCalledTimes(1);
    expect(navigate("will-redirect", "smb://host/share")).toHaveBeenCalledTimes(1);
    expect(openExternal).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledTimes(2);
    warn.mockRestore();
  });

  it("leaves subframes alone, since they cannot call the shell", () => {
    expect(navigate("will-redirect", "https://maps.example/embed", false)).not.toHaveBeenCalled();
  });

  it("denies every new window and opens only http(s) ones in the browser", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    expect(openHandler({ url: "https://example.com/" })).toEqual({ action: "deny" });
    expect(openHandler({ url: "file:///etc/passwd" })).toEqual({ action: "deny" });
    expect(openExternal).toHaveBeenCalledTimes(1);
    expect(openExternal).toHaveBeenCalledWith("https://example.com/");
    warn.mockRestore();
  });
});
