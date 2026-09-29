// The install step for castlabs' Electron.
//
// castlabs' package has no postinstall of its own, so without this nothing fetches the binary; and
// its install.js ignores ELECTRON_SKIP_BINARY_DOWNLOAD, which stock Electron honours and CI's
// frontend job sets so it does not pull ~100 MB it never launches. This puts that flag back.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";

if (process.env.ELECTRON_SKIP_BINARY_DOWNLOAD) {
  console.log("install-electron: ELECTRON_SKIP_BINARY_DOWNLOAD is set; not fetching the binary.");
  process.exit(0);
}

const installer = "node_modules/electron/install.js";
if (!existsSync(installer)) {
  // A partial install, or a workspace that does not have Electron; there is nothing to fetch.
  process.exit(0);
}

const run = spawnSync(process.execPath, [installer], { stdio: "inherit" });
process.exit(run.status ?? 1);
