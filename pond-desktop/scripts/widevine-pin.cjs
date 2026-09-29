// Use a newer Widevine module than the one castlabs' Electron downloads, taken from Chrome on this
// computer, and keep it. A local developer workaround, not a product feature:
//
//   npm run widevine:pin      close the app first; copies Chrome's module in and turns updates off
//   npm run widevine:unpin    turns updates back on and lets the app fetch what Google serves
//
// Why it exists: the module Google's updater serves castlabs' Electron, 4.10.3050.0, is refused by
// Apple Music and Spotify (castlabs/electron-releases#237): licenses are revoked, or a song plays a
// few seconds and dies. A fixed build, 4.10.3050.1, had not reached that updater when this was
// written, but Chrome bundles a newer one (4.10.3112.0 here), and the shell will use it if the
// updater is told to leave it alone. Turning updates off means nothing replaces it later, so unpin
// when Google's fix arrives.
//
// This copies a file from one app you have installed into another on the same computer. It is for
// trying things here and must never be shipped: Widevine's licence does not let us redistribute it.
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");

// ── pure parts, exported for the tests ────────────────────────────────────────

/** Negative, zero or positive, like a comparator, for dotted numeric versions such as 4.10.3112.0. */
function compareVersions(a, b) {
  const x = String(a).split(".").map(Number);
  const y = String(b).split(".").map(Number);
  for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
    const d = (x[i] || 0) - (y[i] || 0);
    if (d !== 0) return d;
  }
  return 0;
}

const isVersion = (s) => /^\d+(\.\d+)+$/.test(s);

/** The modules already in a profile's WidevineCdm folder, newest first. */
function installedVersions(names) {
  return names.filter(isVersion).sort((a, b) => compareVersions(b, a));
}

/** The best module among candidates `{ version, dir }`, or undefined. Newest wins; ties keep the first. */
function pickModule(candidates) {
  let best;
  for (const c of candidates) {
    if (!isVersion(c.version)) continue;
    if (!best || compareVersions(c.version, best.version) > 0) best = c;
  }
  return best;
}

/**
 * The pid that holds a Chromium profile, from its SingletonLock (a symlink whose target reads
 * "host-pid"), or undefined when it names none. Enough to tell whether the app is open on it.
 */
function lockOwner(target) {
  const m = /-(\d+)$/.exec(String(target ?? ""));
  return m ? Number(m[1]) : undefined;
}

const arch = () => (process.arch === "arm64" ? "mac_arm64" : "mac_x64");

/** Widevine modules Chrome carries on this computer: bundled with the app, and component-updated. */
function chromeModules(fsLike = fs, home = os.homedir(), platformDir = arch()) {
  const out = [];
  const add = (dir) => {
    try {
      const version = JSON.parse(fsLike.readFileSync(path.join(dir, "manifest.json"), "utf8")).version;
      const lib = path.join(dir, "_platform_specific", platformDir, "libwidevinecdm.dylib");
      if (isVersion(version) && fsLike.existsSync(lib)) out.push({ version, dir });
    } catch {
      /* not a module folder */
    }
  };
  for (const root of ["/Applications", path.join(home, "Applications")]) {
    let apps = [];
    try {
      apps = fsLike.readdirSync(root).filter((n) => /^Google Chrome.*\.app$/.test(n));
    } catch {
      continue;
    }
    for (const a of apps) {
      const versions = path.join(root, a, "Contents/Frameworks/Google Chrome Framework.framework/Versions");
      let names = [];
      try {
        names = fsLike.readdirSync(versions);
      } catch {
        continue;
      }
      for (const v of names) add(path.join(versions, v, "Libraries/WidevineCdm"));
    }
  }
  // Chrome's own component updater keeps newer ones here, one folder per version.
  const updated = path.join(home, "Library/Application Support/Google/Chrome/WidevineCdm");
  try {
    for (const v of fsLike.readdirSync(updated)) if (isVersion(v)) add(path.join(updated, v));
  } catch {
    /* Chrome has not updated it */
  }
  return out;
}

// ── the command ───────────────────────────────────────────────────────────────

function main() {
  const { app, components } = require("electron");
  const pkg = require("../package.json");
  const revert = process.argv.includes("--revert");
  const profile = process.env.WV_PROFILE || path.join(app.getPath("appData"), pkg.name);
  app.setPath("userData", profile);
  const say = (s) => console.log(s);
  const cdmDir = path.join(profile, "WidevineCdm");
  const exit = (code) => app.exit(code);

  // The app must not be open on this profile: it holds the module, and would put it back.
  try {
    const pid = lockOwner(fs.readlinkSync(path.join(profile, "SingletonLock")));
    if (pid) {
      try {
        process.kill(pid, 0);
        say(`STOP: the app is open (pid ${pid}) on ${profile}. Quit it, then run this again.`);
        return exit(1);
      } catch {
        /* a stale lock from a crash */
      }
    }
  } catch {
    /* no lock: nothing has it open */
  }

  app.whenReady().then(async () => {
    if (!components || typeof components.whenReady !== "function") {
      say("FAIL: this Electron has no Widevine (it is stock Electron, not castlabs'). Run `npm install` first.");
      return exit(2);
    }
    const have = () => (fs.existsSync(cdmDir) ? installedVersions(fs.readdirSync(cdmDir)) : []);
    say(`profile: ${profile}`);
    say(`module in the profile now: ${have().join(", ") || "none"}`);

    if (revert) {
      components.updatesEnabled = true;
      fs.rmSync(cdmDir, { recursive: true, force: true });
      say("updates are back on and the module folder is cleared; the app fetches Google's current one at its next start.");
      return exit(0);
    }

    const best = pickModule(chromeModules());
    if (!best) {
      say("FAIL: no Chrome with a Widevine module found in /Applications or ~/Applications, so there is nothing newer to use.");
      return exit(1);
    }
    const current = have()[0];
    if (current && compareVersions(best.version, current) <= 0) {
      say(`Nothing to do: Chrome's module (${best.version}) is not newer than the one here (${current}).`);
      return exit(0);
    }

    fs.mkdirSync(cdmDir, { recursive: true });
    for (const old of have()) fs.rmSync(path.join(cdmDir, old), { recursive: true, force: true });
    fs.cpSync(best.dir, path.join(cdmDir, best.version), { recursive: true });
    components.updatesEnabled = false;
    // Give the setting a moment to be written before the process ends.
    await new Promise((r) => setTimeout(r, 1500));
    const off = components.updatesEnabled === false;
    say(`copied Chrome's module ${best.version} into the profile and ${off ? "turned updates off" : "could NOT turn updates off"}.`);
    say("It takes effect the next time the app starts. Check it plays with `npm run check:widevine` (set WV_PROFILE for a profile other than the app's). Undo with `npm run widevine:unpin`.");
    return exit(off ? 0 : 1);
  });
}

module.exports = { compareVersions, installedVersions, pickModule, lockOwner, chromeModules };
// Run as the Electron main script, and only then: `require.main` is not this file under Electron, and
// a test that imports the pure parts above must not start an app.
if (process.versions.electron) main();
