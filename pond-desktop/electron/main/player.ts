// The music player window: a small page the shell keeps alive, hidden, so music plays with the
// main window closed. It runs in its own session partition (its own storage, so it pairs with the
// server as its own device and MusicKit's sign-in never mixes with the app's) and every request it
// makes is judged by the server's network policy.

import * as electron from "electron";
import { BrowserWindow, session, type WebContents } from "electron";
import { createRequestPolicy } from "./playerPolicy";
import { APP_ORIGIN, serveRendererFrom } from "./protocol";

export const PLAYER_PARTITION = "persist:giap-player";

/** Apple's sign-in opens as a popup that posts its result back to the page that opened it. */
export function isAppleSignInUrl(rawUrl: string): boolean {
  try {
    const url = new URL(rawUrl);
    return (
      url.protocol === "https:" &&
      (url.hostname === "apple.com" || url.hostname.endsWith(".apple.com"))
    );
  } catch {
    return false;
  }
}

function hostOf(rawUrl: string): string {
  try {
    return new URL(rawUrl).host;
  } catch {
    return "(unparseable)";
  }
}

interface Components {
  whenReady(): Promise<unknown>;
  status(): unknown;
}

/** castlabs' Electron adds `components` (the Widevine module manager); stock Electron has none. */
export function widevineComponents(): Components | undefined {
  return (electron as unknown as { components?: Components }).components;
}

const WIDEVINE_WAIT_MS = 60_000;

export interface PlayerWindowOptions {
  preloadPath: string;
  /** Where the built renderer lives (dist), served at app://giap inside the player's partition. */
  distDir: string;
  /** Read per use: the server may bind a fallback port after the window exists. */
  serverUrl(): string;
  devServerUrl?: string | undefined;
  /** Narrows the window to some services, comma separated. Omitted, it runs every one it has. */
  service?: string;
  log(message: string): void;
}

export class PlayerWindow {
  private win: BrowserWindow | null = null;
  private quitting = false;

  constructor(private readonly opts: PlayerWindowOptions) {}

  /** Waits for the Widevine module, then opens the (hidden) window. Never throws. */
  async start(): Promise<void> {
    try {
      await this.waitForWidevine();
      this.open();
    } catch (error) {
      this.opts.log(
        `player: could not start: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  private async waitForWidevine(): Promise<void> {
    const components = widevineComponents();
    if (!components) {
      this.opts.log(
        "player: this Electron build has no Widevine support (it is not castlabs ECS); the player will say DRM is unavailable",
      );
      return;
    }
    try {
      await Promise.race([
        components.whenReady(),
        new Promise((_, reject) =>
          setTimeout(
            () => reject(new Error("timed out waiting for the module")),
            WIDEVINE_WAIT_MS,
          ),
        ),
      ]);
      this.opts.log(`player: Widevine ready ${JSON.stringify(components.status())}`);
    } catch (error) {
      this.opts.log(
        `player: Widevine module not ready: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  }

  private open(): void {
    const ses = session.fromPartition(PLAYER_PARTITION);
    // protocol.handle is per session; the default one does not reach this partition.
    serveRendererFrom(this.opts.distDir, ses.protocol);

    const decide = createRequestPolicy({ serverUrl: this.opts.serverUrl });
    ses.webRequest.onBeforeRequest({ urls: ["<all_urls>"] }, (details, callback) => {
      void decide(details.url, details.method).then(
        (verdict) => {
          if (verdict.cancel) this.opts.log(`player: blocked ${hostOf(details.url)}`);
          callback(verdict);
        },
        () => callback({ cancel: true }),
      );
    });
    // Why a request failed, by host only: a path or query can carry a song or an identifier.
    ses.webRequest.onErrorOccurred({ urls: ["<all_urls>"] }, (details) => {
      if (details.error !== "net::ERR_ABORTED") {
        this.opts.log(`player: request to ${hostOf(details.url)} failed: ${details.error}`);
      }
    });

    const win = new BrowserWindow({
      title: "Music player",
      width: 380,
      height: 320,
      show: false,
      resizable: false,
      maximizable: false,
      fullscreenable: false,
      webPreferences: {
        preload: this.opts.preloadPath,
        contextIsolation: true,
        sandbox: true,
        nodeIntegration: false,
        webSecurity: true,
        partition: PLAYER_PARTITION,
        // A hidden window's timers and media must keep running, or the music stops with it.
        backgroundThrottling: false,
        // The assistant starts songs, not a click in this window.
        autoplayPolicy: "no-user-gesture-required",
        additionalArguments: [`--giap-server-url=${this.opts.serverUrl()}`],
      },
    });

    // Only what needs attention: MusicKit chatters, and the shell's log is not the place for it.
    win.webContents.on("console-message", (event) => {
      if (event.level === "warning" || event.level === "error") {
        this.opts.log(`player page ${event.level}: ${event.message.slice(0, 300)}`);
      }
    });

    win.webContents.setWindowOpenHandler(({ url }) =>
      isAppleSignInUrl(url) ? { action: "allow" } : { action: "deny" },
    );

    // Closing the window hides it; the player has to outlive it.
    win.on("close", (event) => {
      if (this.quitting) return;
      event.preventDefault();
      win.hide();
    });

    const base = this.opts.devServerUrl ?? APP_ORIGIN;
    const query = this.opts.service ? `?service=${encodeURIComponent(this.opts.service)}` : "";
    void win.loadURL(`${base}/player.html${query}`);
    this.win = win;
  }

  /** For events the shell forwards, such as a changed server URL. */
  webContents(): WebContents | null {
    return this.win && !this.win.isDestroyed() ? this.win.webContents : null;
  }

  setVisible(visible: boolean): void {
    const win = this.win;
    if (!win || win.isDestroyed()) return;
    if (visible) {
      win.show();
      win.focus();
    } else {
      win.hide();
    }
  }

  destroy(): void {
    this.quitting = true;
    if (this.win && !this.win.isDestroyed()) this.win.destroy();
    this.win = null;
  }
}
