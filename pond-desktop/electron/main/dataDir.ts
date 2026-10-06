// Mirrors default_data_dir() in crates/pond-server/src/main.rs. Not Electron's
// app.getPath("userData"): on Linux that is ~/.config, the server uses ~/.local/share.

import { join } from "node:path";

/** The file pond-server writes its ACTUAL bound port to, after it binds. */
export const RUNTIME_PORT_FILE = ".runtime_api_port";

/** The 0600 file holding the credential that host-only routes require; rotated every start. */
export const HOST_CREDENTIAL_FILE = ".runtime_host_credential";

export interface DataDirEnv {
  env: NodeJS.ProcessEnv;
  home: string;
  platform: NodeJS.Platform;
}

/** pond-server's data directory (databases, logs, models), computed as the server does. */
export function resolveDataDir(opts: DataDirEnv): string {
  const override = opts.env["POND_DATA_DIR"];
  if (typeof override === "string" && override.trim() !== "") {
    return override.trim();
  }

  const app = "goose-in-a-pond";
  switch (opts.platform) {
    case "darwin":
      return join(opts.home, "Library", "Application Support", app);
    case "win32": {
      const appData = opts.env["APPDATA"];
      if (typeof appData === "string" && appData.trim() !== "") {
        return join(appData.trim(), app);
      }
      return join(opts.home, "AppData", "Roaming", app);
    }
    default: {
      // dirs::data_dir() on Linux: $XDG_DATA_HOME, else ~/.local/share.
      const xdg = opts.env["XDG_DATA_HOME"];
      if (typeof xdg === "string" && xdg.trim() !== "") {
        return join(xdg.trim(), app);
      }
      return join(opts.home, ".local", "share", app);
    }
  }
}

/** Port from the runtime port file; digits in TCP range only, so a junk file yields null. */
export function readRuntimePort(contents: string | null): number | null {
  if (contents === null) return null;
  const trimmed = contents.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const port = Number(trimmed);
  if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) return null;
  return port;
}

/** Credential from the host credential file: 43 base64url characters, so a junk file yields null. */
export function readHostCredential(contents: string | null): string | null {
  if (contents === null) return null;
  const trimmed = contents.trim();
  return /^[A-Za-z0-9_-]{43}$/.test(trimmed) ? trimmed : null;
}
