import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { McpAppHost } from "./McpAppHost";

afterEach(() => cleanup());

/**
 * The MCP App frame runs untrusted HTML and must not share the host's origin.
 *
 * `html` comes from `api.getMcpResource(...)`, i.e. whatever an MCP server chose
 * to serve — attacker-controlled the moment any third-party server ships a
 * `ui://` resource. This used to render with
 * `sandbox="allow-scripts allow-same-origin"`, and a `srcdoc` iframe inherits the
 * embedder's origin, so `allow-same-origin` handed that origin back: the guest
 * could read `parent.localStorage` (where `PondApiClient` keeps
 * `giap-session-token` and the refresh token) and strip this very `sandbox`
 * attribute off the parent DOM.
 *
 * Omitting `allow-same-origin` gives the frame an opaque origin, which is also
 * what MCP Apps (SEP-1865) requires: "the Host and the Sandbox MUST have
 * different origins".
 */
describe("McpAppHost sandbox", () => {
  const APP_HTML = "<!doctype html><title>t</title><body>hi</body>";

  function frame(): HTMLIFrameElement {
    const { container } = render(<McpAppHost html={APP_HTML} toolName="giap-weather__get_current_weather" />);
    const el = container.querySelector("iframe");
    if (!el) throw new Error("no iframe rendered — the host did not mount");
    return el as HTMLIFrameElement;
  }

  it("never grants allow-same-origin to the app frame", () => {
    const sandbox = frame().getAttribute("sandbox") ?? "";
    // Vacuity control: an absent or empty attribute is NOT a pass. A missing
    // `sandbox` attribute is the least sandboxed state there is.
    expect(sandbox).toContain("allow-scripts");
    expect(sandbox).not.toContain("allow-same-origin");
  });

  it("grants nothing beyond allow-scripts", () => {
    // Enumerated rather than spot-checked, so a future `allow-popups` or
    // `allow-top-navigation` has to be argued for here rather than appearing.
    const tokens = (frame().getAttribute("sandbox") ?? "").split(/\s+/).filter(Boolean);
    expect(tokens).toEqual(["allow-scripts"]);
  });

  it("renders the app HTML into srcdoc, never into src", () => {
    const el = frame();
    expect(el.getAttribute("srcdoc")).toBe(APP_HTML);
    // A `src` would be a navigation the sandbox reasoning above does not cover.
    expect(el.getAttribute("src")).toBeNull();
  });
});

/**
 * Messages into the frame are addressed to the opaque origin, not to `"*"`.
 *
 * An opaque origin serialises to the literal string `"null"`. `"*"` also
 * delivers, but it means "whatever origin this frame has now", which stops being
 * safe the moment the frame navigates or the sandbox is loosened.
 */
describe("McpAppHost postMessage targeting", () => {
  it("addresses the app frame as the null origin", () => {
    const { container } = render(<McpAppHost html="<p>x</p>" toolName="t" />);
    const el = container.querySelector("iframe") as HTMLIFrameElement;
    const frameWindow = el.contentWindow;
    expect(frameWindow, "jsdom gave the frame no contentWindow to spy on").not.toBeNull();

    const posted: string[] = [];
    // The component posts to the FRAME's window, not the host's — spying on
    // `window.postMessage` captures nothing, which is how the first version of
    // this test passed against a `"*"` targetOrigin.
    const spy = vi
      .spyOn(frameWindow!, "postMessage")
      .mockImplementation(((_msg: unknown, origin: string) => {
        posted.push(origin);
      }) as typeof window.postMessage);

    // Drive the handshake the way a real app does.
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { jsonrpc: "2.0", id: 1, method: "ui/initialize" },
        source: frameWindow,
      } as MessageEventInit),
    );

    spy.mockRestore();

    // Unconditional, and asserted BEFORE the origin check. A handshake that
    // never reached the component would otherwise make every assertion below
    // vacuously true — proven by mutation: with `if (posted.length > 0)`
    // wrapping the loop, flipping the constant back to `"*"` still passed.
    expect(posted.length, "the ui/initialize handshake produced no reply").toBeGreaterThan(0);
    for (const origin of posted) {
      expect(origin).toBe("null");
    }
  });
});
