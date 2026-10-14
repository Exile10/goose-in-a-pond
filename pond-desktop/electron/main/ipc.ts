// Contract commands on `giap:` channels; names outside SHELL_COMMANDS don't compile.

import { ipcMain, shell, type IpcMainInvokeEvent } from "electron";
import type { ShellCommand } from "../../src/shell/contract";
import { isAppUrl, isWebUrl } from "./navigation";
import type { ServerProcess } from "./serverProcess";
import type { VoiceChildProcess } from "./voice/VoiceChildProcess";

export interface IpcTargets {
  server: ServerProcess;
  voice: VoiceChildProcess;
  /** Reads the server's host credential file; null when it is missing or malformed. */
  hostCredential: () => string | null;
}

/**
 * Why the sender may not call the shell, or null if it may. Security: only the app's own page in
 * the main frame may; a subframe or an outside page that reached the window could otherwise read
 * the host credential.
 */
function senderRefusal(
  event: IpcMainInvokeEvent,
  allowedOrigins: readonly string[],
): string | null {
  const frame = event.senderFrame;
  if (frame === null) return "its frame is gone";
  if (frame !== event.sender.mainFrame) return "it is not the main frame";
  if (!isAppUrl(frame.url, allowedOrigins)) return "its page is not the app";
  return null;
}

/**
 * Register every contract command. A call `senderRefusal` objects to is rejected without running
 * its handler; Electron logs the rejection and the renderer's invoke sees it.
 */
export function registerIpc(t: IpcTargets, allowedOrigins: readonly string[]): void {
  /** Register a handler, with the channel name checked against the contract. */
  const handle = <C extends ShellCommand>(
    command: C,
    fn: (args: unknown) => Promise<unknown> | unknown,
  ): void => {
    ipcMain.handle(`giap:${command}`, async (event, args) => {
      const refusal = senderRefusal(event, allowedOrigins);
      if (refusal !== null) throw new Error(`giap:${command} refused: ${refusal}`);
      return fn(args);
    });
  };

  handle("server_health", () => t.server.healthCheck());

  handle("ensure_server_running", () => t.server.ensureRunning());

  handle("start_voice_session", (args) => {
    const sessionId =
      (args as { sessionId?: string | null } | undefined)?.sessionId ?? null;
    return t.voice.start(sessionId);
  });

  handle("stop_voice_session", () => t.voice.stop());

  handle("host_credential", () => t.hostCredential());

  handle("open_external", async (args) => {
    const url = (args as { url?: string } | undefined)?.url;
    if (typeof url !== "string") throw new Error("open_external needs a url");
    // Security: http(s) only, so a compromised renderer can't launch files or scheme handlers.
    if (!isWebUrl(url)) throw new Error("refusing to open a non-http(s) URL externally");
    await shell.openExternal(url);
  });
}
