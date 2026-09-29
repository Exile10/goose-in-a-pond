// Contract commands on `giap:` channels; names outside SHELL_COMMANDS don't compile.

import { ipcMain, shell } from "electron";
import type { ShellCommand } from "../../src/shell/contract";
import type { ServerProcess } from "./serverProcess";
import type { VoiceChildProcess } from "./voice/VoiceChildProcess";
import type { PlayerWindow } from "./player";

export interface IpcTargets {
  server: ServerProcess;
  voice: VoiceChildProcess;
  player: PlayerWindow | null;
}

/** Register a handler, with the channel name checked against the contract. */
function handle<C extends ShellCommand>(
  command: C,
  fn: (args: unknown) => Promise<unknown> | unknown,
): void {
  ipcMain.handle(`giap:${command}`, (_event, args) => fn(args));
}

export function registerIpc(t: IpcTargets): void {
  handle("server_health", () => t.server.healthCheck());

  handle("ensure_server_running", () => t.server.ensureRunning());

  handle("start_voice_session", (args) => {
    const sessionId =
      (args as { sessionId?: string | null } | undefined)?.sessionId ?? null;
    return t.voice.start(sessionId);
  });

  handle("stop_voice_session", () => t.voice.stop());

  handle("player_visibility", (args) => {
    const visible = (args as { visible?: unknown } | undefined)?.visible;
    if (typeof visible !== "boolean") {
      throw new Error("player_visibility needs { visible: boolean }");
    }
    t.player?.setVisible(visible);
  });

  handle("player_authorize", async (args) => {
    const service = (args as { service?: unknown } | undefined)?.service;
    // The name goes into a script the window runs, so it is held to the shape a service name has.
    if (typeof service !== "string" || !/^[a-z0-9_-]{1,32}$/.test(service)) {
      throw new Error("player_authorize needs { service: a short lowercase name }");
    }
    if (!t.player) {
      return { started: false, message: "The music player is turned off." };
    }
    return t.player.authorize(service);
  });

  handle("open_external", async (args) => {
    const url = (args as { url?: string } | undefined)?.url;
    if (typeof url !== "string") throw new Error("open_external needs a url");
    // Security: http(s) only, so a compromised renderer can't launch files or scheme handlers.
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      throw new Error(`refusing to open a ${parsed.protocol} URL externally`);
    }
    await shell.openExternal(url);
  });
}
