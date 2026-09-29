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
import { PlayerApp } from "./PlayerApp";

const service = new URLSearchParams(window.location.search).get("service") ?? "apple";
const api = new PondApiClient();
api.setDeviceName("GIAP Music Player");

const adapter = createAdapter(service, {
  fetchDeveloperToken: async () => (await api.musickitDeveloperToken()).token,
});

const root = ReactDOM.createRoot(document.getElementById("root")!);

if (!adapter) {
  root.render(
    <main className="player">
      <p className="player__text">
        There is no player for "{service}". Available: {knownServices().join(", ")}.
      </p>
    </main>,
  );
} else {
  root.render(
    <React.StrictMode>
      <PlayerApp adapter={adapter} />
    </React.StrictMode>,
  );
  // The window stays hidden until it needs the user, which is a sign-in and nothing else.
  let shown = false;
  adapter.onState((s) => {
    const needsUser = s.need === "authorization";
    if (needsUser === shown) return;
    shown = needsUser;
    void invoke("player_visibility", { visible: needsUser }).catch(() => undefined);
  });
  // The server may bind another port after this window opened.
  listen("server-url", (url) => api.setBase(url));

  void (async () => {
    // Pair first, so the bridge's first request already carries a session.
    await api.connect().catch(() => null);
    await adapter.init();
    new PlayerBridge(adapter, api, {
      log: (message, detail) => console.warn(`[player] ${message}`, detail ?? ""),
    }).start();
  })();
}
