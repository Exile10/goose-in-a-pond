// Does Widevine work in this build of the shell? Plays Google's public Widevine-encrypted test stream
// muted, with a license from Google's public test proxy, and says whether the license exchange and
// the decryption really happened. Nothing about the person or the pond is sent.
//
//   npx electron scripts/widevine-check.cjs             a fresh profile, the way a first launch is
//   WV_PROFILE=<dir> npx electron scripts/widevine-check.cjs   an existing one
//
// Exit 0: protected content decrypted and played. 1: it did not. 2: this Electron is not castlabs'
// (stock Electron has no Widevine at all). Manual: it needs the network and Google's test hosts, so it
// is not part of CI. It says nothing about whether Apple or Spotify will accept the module.
"use strict";

const fs = require("fs");
const http = require("http");
const os = require("os");
const path = require("path");
const { app, BrowserWindow, components } = require("electron");

const MANIFEST = "https://storage.googleapis.com/shaka-demo-assets/angel-one-widevine/dash.mpd";
const LICENSE = "https://cwip-shaka-proxy.appspot.com/no_auth";
const SHAKA = "https://cdnjs.cloudflare.com/ajax/libs/shaka-player/5.2.11/shaka-player.compiled.js";
const MODULE_WAIT_MS = 150_000;
const WATCHDOG_MS = MODULE_WAIT_MS + 90_000;

const fresh = !process.env.WV_PROFILE;
const profile = process.env.WV_PROFILE || fs.mkdtempSync(path.join(os.tmpdir(), "giap-widevine-"));
app.setPath("userData", profile);

const t0 = Date.now();
const lap = () => `${((Date.now() - t0) / 1000).toFixed(1)} s`;
const say = (s) => console.log(s);
let stage = "starting";
function finish(code) {
  if (fresh) fs.rmSync(profile, { recursive: true, force: true });
  app.exit(code);
}
// A check that can hang is worse than no check: whatever is stuck, this ends, and says where.
setTimeout(() => {
  say(`FAIL: no answer after ${WATCHDOG_MS / 1000} s, while ${stage}`);
  finish(1);
}, WATCHDOG_MS).unref();

// The page runs inside the window, so it is a string.
const PAGE = `(async () => {
  const out = { secureContext: isSecureContext };
  await new Promise((res, rej) => { const s = document.createElement("script"); s.src = ${JSON.stringify(SHAKA)}; s.onload = res; s.onerror = () => rej(new Error("could not load the player library")); document.head.appendChild(s); });
  shaka.polyfill.installAll();
  const video = document.getElementById("v"); video.muted = true;
  const player = new shaka.Player(); await player.attach(video);
  player.configure({ drm: { servers: { "com.widevine.alpha": ${JSON.stringify(LICENSE)} } } });
  try { await player.load(${JSON.stringify(MANIFEST)}); }
  catch (e) { return { ...out, loaded: false, error: "code " + e.code + ": " + String(e.message).slice(0, 160) }; }
  out.keySystem = player.keySystem();
  await video.play().catch((e) => { out.playError = String(e.message).slice(0, 100); });
  const start = Date.now();
  while (Date.now() - start < 25000 && video.currentTime < 6) await new Promise((r) => setTimeout(r, 250));
  const q = video.getVideoPlaybackQuality(), st = player.getStats();
  out.currentTime = +video.currentTime.toFixed(2); out.frames = q.totalVideoFrames; out.dropped = q.droppedVideoFrames;
  out.licenseSeconds = st.licenseTime; out.stalls = st.stallsDetected;
  out.worked = video.currentTime >= 3 && q.totalVideoFrames > 30;
  return out;
})()`;

app.whenReady().then(async () => {
  say(`Electron ${process.versions.electron}, Chrome ${process.versions.chrome}`);
  if (!components || typeof components.whenReady !== "function") {
    say("FAIL: this Electron has no Widevine (it is stock Electron, not castlabs'). Run `npm install` in pond-desktop.");
    return finish(2);
  }
  stage = "waiting for the Widevine module";
  try {
    await Promise.race([
      components.whenReady(),
      new Promise((_, rej) => setTimeout(() => rej(new Error(`the module was not ready after ${MODULE_WAIT_MS / 1000} s`)), MODULE_WAIT_MS)),
    ]);
  } catch (e) {
    say(`FAIL: ${e.message}`);
    return finish(1);
  }
  const mod = Object.values(components.status()).map((c) => `${c.title} ${c.version} (${c.status})`).join(", ");
  say(`module ready after ${lap()}${fresh ? " on a fresh profile" : ""}: ${mod}`);

  stage = "opening the test window";
  const server = http.createServer((_, res) => {
    res.setHeader("content-type", "text/html");
    res.end('<!doctype html><meta charset=utf-8><video id=v muted playsinline width=320></video>');
  });
  server.listen(0, "127.0.0.1", async () => {
    const win = new BrowserWindow({ show: false, webPreferences: { autoplayPolicy: "no-user-gesture-required" } });
    await win.loadURL(`http://127.0.0.1:${server.address().port}/`);
    let r;
    stage = "playing the encrypted stream";
    try { r = await win.webContents.executeJavaScript(PAGE); }
    catch (e) { r = { worked: false, error: String(e.message).slice(0, 200) }; }
    server.close();
    // WV_HOLD=<ms> keeps the process, and so the Widevine module, alive a little longer, so something
    // outside can look at which module file the browser really has open.
    const hold = Number(process.env.WV_HOLD || 0);
    if (hold > 0) await new Promise((res) => setTimeout(res, hold));
    if (r.worked) {
      say(`PASS: ${r.keySystem} licensed in ${r.licenseSeconds}s and decrypted: played ${r.currentTime} s, ${r.frames} frames, ${r.dropped} dropped, ${r.stalls} stalls`);
      return finish(0);
    }
    say(`FAIL: ${r.error || "the stream did not play"}${r.playError ? ` (${r.playError})` : ""} ${JSON.stringify({ loaded: r.loaded, currentTime: r.currentTime, frames: r.frames })}`);
    finish(1);
  });
});
