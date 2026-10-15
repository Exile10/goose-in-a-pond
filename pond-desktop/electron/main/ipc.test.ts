import { beforeEach, describe, expect, it, vi } from "vitest";
import type { IpcMainInvokeEvent } from "electron";

type Handler = (event: IpcMainInvokeEvent, args?: unknown) => Promise<unknown>;

const { handlers, openExternal } = vi.hoisted(() => ({
  handlers: new Map<string, Handler>(),
  openExternal: vi.fn(async () => undefined),
}));

vi.mock("electron", () => ({
  ipcMain: { handle: (channel: string, fn: Handler) => handlers.set(channel, fn) },
  shell: { openExternal },
}));

import { registerIpc, type IpcTargets } from "./ipc";
import { appOrigins } from "./navigation";

const hostCredential = vi.fn(() => "credential");
const targets = {
  server: { healthCheck: vi.fn(), ensureRunning: vi.fn() },
  voice: { start: vi.fn(), stop: vi.fn(), interrupt: vi.fn() },
  hostCredential,
} as unknown as IpcTargets;

/** An invoke from `frameUrl`, in the main frame unless `subframe`, or from a frame already gone. */
function invokeFrom(frameUrl: string | null, subframe = false): IpcMainInvokeEvent {
  const mainFrame = { url: frameUrl ?? "app://giap/index.html" };
  const senderFrame = frameUrl === null ? null : subframe ? { url: frameUrl } : mainFrame;
  return { senderFrame, sender: { mainFrame } } as unknown as IpcMainInvokeEvent;
}

function call(channel: string, event: IpcMainInvokeEvent, args?: unknown): Promise<unknown> {
  const handler = handlers.get(`giap:${channel}`);
  if (!handler) throw new Error(`no handler for ${channel}`);
  return handler(event, args);
}

describe("IPC sender check", () => {
  beforeEach(() => {
    handlers.clear();
    vi.clearAllMocks();
  });

  it("answers the app's own page in the main frame", async () => {
    registerIpc(targets, appOrigins(undefined));
    await expect(call("host_credential", invokeFrom("app://giap/index.html"))).resolves.toBe("credential");
    expect(hostCredential).toHaveBeenCalledTimes(1);
  });

  it("refuses a page from any other origin", async () => {
    registerIpc(targets, appOrigins(undefined));
    await expect(call("host_credential", invokeFrom("https://evil.example/"))).rejects.toThrow(/refused/);
    await expect(call("host_credential", invokeFrom("app://evil/index.html"))).rejects.toThrow(/refused/);
    expect(hostCredential).not.toHaveBeenCalled();
  });

  it("refuses a subframe, even one on the app's origin", async () => {
    registerIpc(targets, appOrigins(undefined));
    await expect(call("host_credential", invokeFrom("app://giap/index.html", true))).rejects.toThrow(/main frame/);
    expect(hostCredential).not.toHaveBeenCalled();
  });

  it("refuses a sender whose frame is gone", async () => {
    registerIpc(targets, appOrigins(undefined));
    await expect(call("host_credential", invokeFrom(null))).rejects.toThrow(/refused/);
    expect(hostCredential).not.toHaveBeenCalled();
  });

  it("answers the Vite dev server only when one is configured", async () => {
    const dev = invokeFrom("http://localhost:1420/");
    registerIpc(targets, appOrigins(undefined));
    await expect(call("host_credential", dev)).rejects.toThrow(/refused/);

    registerIpc(targets, appOrigins("http://localhost:1420"));
    await expect(call("host_credential", dev)).resolves.toBe("credential");
    await expect(call("host_credential", invokeFrom("http://localhost:1421/"))).rejects.toThrow(/refused/);
  });

  it("routes an interrupt to the voice child and nothing else", async () => {
    registerIpc(targets, appOrigins(undefined));
    await call("interrupt_voice_turn", invokeFrom("app://giap/index.html"));
    expect(targets.voice.interrupt).toHaveBeenCalledTimes(1);
    expect(targets.voice.stop).not.toHaveBeenCalled();
  });

  it("refuses an interrupt from a page that is not the app", async () => {
    registerIpc(targets, appOrigins(undefined));
    await expect(call("interrupt_voice_turn", invokeFrom("https://evil.example/"))).rejects.toThrow(/refused/);
    expect(targets.voice.interrupt).not.toHaveBeenCalled();
  });

  it("still opens only http(s) URLs externally", async () => {
    registerIpc(targets, appOrigins(undefined));
    const app = invokeFrom("app://giap/index.html");
    await expect(call("open_external", app, { url: "file:///etc/passwd" })).rejects.toThrow(/non-http/);
    await call("open_external", app, { url: "https://example.com/" });
    expect(openExternal).toHaveBeenCalledTimes(1);
    expect(openExternal).toHaveBeenCalledWith("https://example.com/");
  });
});
