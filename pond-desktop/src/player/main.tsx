// The player window's entry. It runs in its own session partition (see electron/main/player.ts),
// so it pairs with the server as its own device and its cookies never mix with the app's.

import React from "react";
import ReactDOM from "react-dom/client";

import "../styles/design-tokens.css";
import "../styles/base.css";

import { PondApiClient } from "../api/PondApiClient";
import { invoke, listen } from "../shell";
import { createAdapter, knownServices } from "./adapters";
import { PlayerBridge } from "./bridge";
import { Players } from "./Players";
import { retrySetup } from "./retry";
import { needsPerson, startSignIn } from "./signIn";

declare global {
  interface Window {
    /** The shell's hook for starting a sign-in with a user gesture; see electron/main/player.ts. */
    __giapPlayer?: { authorize(service: string): ReturnType<typeof startSignIn> };
  }
}

// One window runs every service it has an adapter for: `?service=apple,spotify` narrows it, and
// none means all. A service that is not set up sits dormant and costs one small request every
// ten seconds until it is.
const asked = (new URLSearchParams(window.location.search).get("service") ?? "")
  .split(",")
  .map((s) => s.trim())
  .filter(Boolean);
const services = asked.length > 0 ? asked : knownServices();
const api = new PondApiClient();
api.setDeviceName("GIAP Music Player");

// The shell says which Widevine module it loaded, since the page cannot tell.
const cdm = new URLSearchParams(window.location.search).get("cdm") ?? undefined;
const context = {
  widevineVersion: cdm,
  fetchDeveloperToken: async () => (await api.musickitDeveloperToken()).token,
  probeDeveloperToken: () => api.musickitDeveloperTokenAvailable(),
  fetchUserToken: async (service: string, refresh: boolean) =>
    (await api.playerUserToken(service, refresh)).token,
};
const adapters = services.flatMap((s) => {
  const adapter = createAdapter(s, context);
  return adapter ? [adapter] : [];
});

const root = ReactDOM.createRoot(document.getElementById("root")!);

if (adapters.length === 0) {
  root.render(
    <main className="player">
      <p className="player__text">
        There is no player for "{services.join(", ")}". Available: {knownServices().join(", ")}.
      </p>
    </main>,
  );
} else {
  root.render(
    <React.StrictMode>
      <Players adapters={adapters} />
    </React.StrictMode>,
  );
  // The window stays hidden until it needs the user, which is a sign-in and nothing else.
  let shown = false;
  // A sleeping adapter offers a sign-in in the settings row, but does not raise this window for it.
  const needsUser = () => adapters.some((a) => needsPerson(a.state()));
  const syncVisibility = () => {
    const want = needsUser();
    if (want === shown) return;
    shown = want;
    void invoke("player_visibility", { visible: want }).catch(() => undefined);
  };
  for (const adapter of adapters) adapter.onState(syncVisibility);
  window.__giapPlayer = { authorize: (service) => startSignIn(adapters, service) };
  // The server may bind another port after this window opened.
  listen("server-url", (url) => api.setBase(url));

  void (async () => {
    // Pair first, so the bridge's first request already carries a session.
    await api.connect().catch(() => null);
    await Promise.all(adapters.map((adapter) => adapter.init()));
    for (const adapter of adapters) {
      // The key is added after this window opened; keep asking until it is there.
      retrySetup(adapter);
      new PlayerBridge(adapter, api, {
        log: (message, detail) =>
          console.warn(`[player:${adapter.service}] ${message}`, detail ?? ""),
      }).start();
    }
  })();
}
