// The desktop shell.
//
// Replaces src-tauri/src/main.rs. What it owns: the pond-server sidecar's
// lifecycle, the voice child, one window, the tray, two global shortcuts, and
// the menu.
//
// Ordering that matters, and cost someone a debugging session in the Rust:
//
//   * The scheme is registered before `whenReady`, because
//     registerSchemesAsPrivileged has no effect afterwards.
//   * On quit the voice child is killed BEFORE the server is shut down, so the
//     microphone and speaker are released first.

import { app, BrowserWindow } from "electron";
import { join, resolve } from "node:path";
import { registerAppScheme, serveRendererFrom } from "./protocol";
import { createMainWindow, distRoot } from "./window";
import { ServerProcess, recoveryBackoffSeconds, resolveServerBinary } from "./serverProcess";
import { VoiceChildProcess } from "./voice/VoiceChildProcess";
import { registerIpc } from "./ipc";
import { installMenu } from "./menu";
import { createTray, setTrayStatus, destroyTray } from "./tray";
import { registerHotkeys, unregisterHotkeys } from "./hotkeys";
import type { ShellEvent, ShellEvents } from "../../src/shell/contract";

const log = {
  info: (m: string) => console.log(`[giap] ${m}`),
  warn: (m: string) => console.warn(`[giap] ${m}`),
  debug: (m: string) => {
    if (process.env["GIAP_DEBUG"]) console.debug(`[giap] ${m}`);
  },
};

let win: BrowserWindow | null = null;
/** Set on the way out, so the close handler stops hiding and lets us quit. */
let quitting = false;

/** The repo root, used only in dev to ground the sidecar's cwd. */
const repoRoot = resolve(app.getAppPath(), "..");

function emit<E extends ShellEvent>(name: E, payload?: ShellEvents[E]): void {
  if (!win || win.isDestroyed()) return;
  win.webContents.send(`giap:${name}`, payload);
}

const server = new ServerProcess({
  lookup: {
    isPackaged: app.isPackaged,
    resourcesPath: process.resourcesPath,
    repoRoot,
    platform: process.platform,
  },
  log,
});

const voice = new VoiceChildProcess({
  resolveBinary: () => serverBinaryForVoice(),
  cwd: app.isPackaged ? undefined : repoRoot,
  emit,
  log,
});

/**
 * The voice child runs the same binary as the sidecar. Resolved through the
 * server's own lookup so there is one answer to "where is pond-server", not
 * two that can disagree.
 */
function serverBinaryForVoice(): string | null {
  return resolveServerBinary({
    override: process.env["POND_SERVER_BIN"],
    isPackaged: app.isPackaged,
    resourcesPath: process.resourcesPath,
    repoRoot,
    platform: process.platform,
  });
}

function showWindow(): void {
  if (!win || win.isDestroyed()) return;
  if (!win.isVisible()) win.show();
  if (win.isMinimized()) win.restore();
  win.focus();
}

/**
 * Watch the server, and try to bring it back when it goes away.
 *
 * Backs off exponentially so a server that cannot start is not hammered, and
 * reports every transition to the renderer and the tray -- when the window is
 * hidden the tooltip is the only place this state is visible.
 */
function startHealthLoop(): void {
  let failures = 0;
  let online: boolean | null = null;

  const tick = async () => {
    const healthy = await server.healthCheck();
    if (healthy !== online) {
      online = healthy;
      emit("server-status", healthy);
      setTrayStatus(healthy);
    }

    if (healthy) {
      failures = 0;
      setTimeout(() => void tick(), 10_000);
      return;
    }

    failures += 1;
    const wait = recoveryBackoffSeconds(failures);
    log.warn(`pond-server is unreachable (attempt ${failures}); retrying in ${wait}s`);
    emit("server-starting");
    try {
      await server.ensureRunning();
    } catch (e) {
      log.warn(`recovery failed: ${(e as Error).message}`);
    }
    setTimeout(() => void tick(), wait * 1_000);
  };

  setTimeout(() => void tick(), 10_000);
}

// Must happen before the app is ready.
registerAppScheme();

// One instance only. A second launch -- from the Dock, or from
// `pond-server serve --native` -- should surface the window we already have
// rather than start a second shell fighting for the same port and microphone.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", showWindow);

  void app.whenReady().then(async () => {
    serveRendererFrom(distRoot(app.getAppPath()));

    // Reap a voice child orphaned by a hard kill of a previous run; it would
    // still be holding the microphone.
    voice.cleanupOrphans();

    installMenu({ emit });
    registerIpc({ server, voice });

    win = createMainWindow({
      preloadPath: join(__dirname, "../preload/index.cjs"),
      serverUrl: server.url,
      userDataDir: app.getPath("userData"),
      devServerUrl: process.env["GIAP_DEV_SERVER"],
      onCloseRequested: (w) => {
        if (quitting) {
          w.destroy();
          return;
        }
        w.hide();
      },
    });

    createTray({
      emit,
      showWindow,
      iconPath: join(app.getAppPath(), "build", "trayTemplate.png"),
    });

    registerHotkeys({ emit, focusWindow: showWindow, log: log.info });

    // Kick the server, but do not block the window on it: the startup screen
    // exists precisely to render while the server is still coming up.
    server
      .ensureRunning()
      .then((url) => log.info(`pond-server ready at ${url}`))
      .catch((e: Error) => log.warn(`pond-server did not start: ${e.message}`));

    startHealthLoop();
  });

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) return;
    showWindow();
  });

  // Closing the window hides to the tray, so this must NOT quit -- on any
  // platform, because the tray is the app's resting state.
  app.on("window-all-closed", () => {});

  app.on("before-quit", () => {
    quitting = true;
    // Order matters: release the microphone and speaker before taking the
    // server down.
    voice.killNow();
    server.shutdown();
    unregisterHotkeys();
    destroyTray();
  });

  // Last resort. `kill()` is a synchronous syscall, so it is legal here, and
  // this is the path that runs when an uncaught exception takes the app down.
  process.on("exit", () => voice.killNow());
}
