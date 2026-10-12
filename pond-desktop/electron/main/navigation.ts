// Which pages count as the app. They alone may call the shell over IPC, and the window never
// navigates to any other page: an outside page in the main frame could read the host credential.

import { shell, type WebContents } from "electron";
import { APP_ORIGIN } from "./protocol";

/** The renderer's origins: app://giap, plus the Vite dev server only when one is configured. */
export function appOrigins(devServerUrl: string | undefined): string[] {
  return devServerUrl ? [APP_ORIGIN, originOf(devServerUrl)] : [APP_ORIGIN];
}

/**
 * `scheme://host[:port]`. Not `URL.origin`, which is the opaque "null" for a non-special
 * scheme such as app://, so it could never match APP_ORIGIN. For http(s) the two agree.
 */
function originOf(url: string): string {
  const parsed = new URL(url);
  return `${parsed.protocol}//${parsed.host}`;
}

/** Whether `url` is a page of the app itself. Anything unparseable is not. */
export function isAppUrl(url: string, allowedOrigins: readonly string[]): boolean {
  try {
    return allowedOrigins.includes(originOf(url));
  } catch {
    return false;
  }
}

/** Whether `url` may go to the real browser: http(s) only, never files or scheme handlers. */
export function isWebUrl(url: string): boolean {
  try {
    const { protocol } = new URL(url);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

/** Hand an outside link to the real browser, or drop it, with a warning, if it is not http(s). */
function openOutside(url: string): void {
  if (isWebUrl(url)) {
    void shell.openExternal(url);
    return;
  }
  console.warn(`[giap] refusing to open a non-http(s) URL: ${url.slice(0, 64)}`);
}

/**
 * Keep `contents` on the app's own pages. Leaving them, by a link, a script or a server
 * redirect, is cancelled and an http(s) destination opens in the real browser instead; new
 * windows likewise never open in-app. Subframes are left alone: they cannot call the shell.
 */
export function guardNavigation(
  contents: WebContents,
  allowedOrigins: readonly string[],
): void {
  const keepInApp = (event: Electron.Event<{ url: string; isMainFrame: boolean }>) => {
    if (!event.isMainFrame || isAppUrl(event.url, allowedOrigins)) return;
    event.preventDefault();
    openOutside(event.url);
  };
  contents.on("will-navigate", keepInApp);
  contents.on("will-redirect", keepInApp);
  contents.setWindowOpenHandler(({ url }) => {
    openOutside(url);
    return { action: "deny" };
  });
}
