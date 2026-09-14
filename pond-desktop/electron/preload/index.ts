// The bridge.
//
// This is the only code that can see both the renderer's globals and
// ipcRenderer, and it runs sandboxed with context isolation on. The two Sets
// below are the security boundary, not decoration: without them the bridge
// forwards any string the renderer hands it to ipcRenderer.invoke, and the
// surface becomes "whatever channel main happens to have registered" rather
// than the five commands the contract declares.

import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import { SHELL_COMMANDS, SHELL_EVENTS } from "../../src/shell/contract";

const commands = new Set<string>(SHELL_COMMANDS);
const events = new Set<string>(SHELL_EVENTS);

const ARG_PREFIX = "--giap-server-url=";
const serverUrl =
  process.argv.find((a) => a.startsWith(ARG_PREFIX))?.slice(ARG_PREFIX.length) ??
  "http://127.0.0.1:4000";

// PondApiClient reads this at module load, before any of our code runs, so it
// has to exist before the first line of page script. Under Tauri this was an
// initialization_script; a preload runs at the same point.
contextBridge.exposeInMainWorld("__GIAP_SERVER_URL__", serverUrl);

contextBridge.exposeInMainWorld("giap", {
  serverUrl,

  invoke(command: string, args?: unknown): Promise<unknown> {
    if (!commands.has(command)) {
      return Promise.reject(new Error(`unknown shell command: ${command}`));
    }
    return ipcRenderer.invoke(`giap:${command}`, args);
  },

  /**
   * Synchronous, and returns the unsubscribe function directly.
   *
   * Tauri's listen() was async because registration round-tripped into Rust,
   * and two classes of bug followed from that: an unlisten resolving after
   * teardown, and events fired between mount and registration being lost.
   * ipcRenderer.on needs no round-trip, so neither is expressible.
   */
  listen(event: string, handler: (payload: unknown) => void): () => void {
    if (!events.has(event)) throw new Error(`unknown shell event: ${event}`);
    const channel = `giap:${event}`;
    const wrapped = (_e: IpcRendererEvent, payload: unknown) => handler(payload);
    ipcRenderer.on(channel, wrapped);
    return () => {
      ipcRenderer.off(channel, wrapped);
    };
  },
});
