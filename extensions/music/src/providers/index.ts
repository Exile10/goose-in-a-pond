import { log } from "../log.js";
import { AppleMusicProvider } from "./apple-music.js";
import { defaultStorefront, ItunesSearch, songIdFromLink } from "./apple/catalog.js";
import { EgressGate, type Fetch } from "./apple/egress.js";
import { MusicApp } from "./apple/music-app.js";
import { runAppleScript } from "./apple/osascript.js";
import { HostPlayer } from "./player/host.js";
import { HostSpeaker } from "./player/speaker.js";
import { chooseService } from "./select.js";
import { SpotifyProvider } from "./spotify.js";
import type { MusicProvider } from "./types.js";
import { WebPlayerProvider } from "./web-player.js";

function createApple(env: NodeJS.ProcessEnv): AppleMusicProvider {
  const hostUrl = env.GIAP_SERVER_URL || "http://127.0.0.1:4000";
  const internalToken = env.GIAP_INTERNAL_TOKEN ?? "";

  const egress = new EgressGate(fetch, hostUrl, internalToken);
  const storefront = defaultStorefront(env.APPLE_MUSIC_STOREFRONT, Intl.DateTimeFormat().resolvedOptions().locale);

  return new AppleMusicProvider({
    app: new MusicApp(runAppleScript),
    catalog: new ItunesSearch(fetch, egress, storefront),
  });
}

function createSpotify(env: NodeJS.ProcessEnv, fetchFn: Fetch): SpotifyProvider {
  const hostUrl = env.GIAP_SERVER_URL || "http://127.0.0.1:4000";
  const internalToken = env.GIAP_INTERNAL_TOKEN ?? "";

  return new SpotifyProvider({
    fetch: fetchFn,
    // Every call to Spotify asks the host first, so `network_mode` covers it as it does Apple's.
    egress: new EgressGate(fetchFn, hostUrl, internalToken),
    // The in-app player is a Connect device: somewhere to play when no other device is active.
    speaker: new HostSpeaker(new HostPlayer(fetchFn, hostUrl, internalToken, "spotify")),
  });
}

/**
 * The provider for this run. Apple Music plays through the app's own player when the user has
 * added an Apple Music key, since that plays the whole catalog; otherwise, and whenever the player
 * cannot be used, through the Music app.
 */
export async function createProvider(
  env: NodeJS.ProcessEnv = process.env,
  platform: string = process.platform,
  fetchFn: Fetch = fetch,
): Promise<MusicProvider> {
  const { service, reason } = chooseService(env, platform);
  log.info("service_chosen", `using ${service}`, { service, reason });
  if (service !== "apple") return createSpotify(env, fetchFn);

  const local = createApple(env);
  const host = new HostPlayer(
    fetchFn,
    env.GIAP_SERVER_URL || "http://127.0.0.1:4000",
    env.GIAP_INTERNAL_TOKEN ?? "",
    "apple",
  );
  const status = await host.status();
  if (!status?.configured) {
    log.info("apple_backend", "using the Music app: no Apple Music key is set up", {
      host_reachable: status !== null,
    });
    return local;
  }

  log.info("apple_backend", "using the music player page, with the Music app as its fallback", {
    player_attached: status.attached,
  });
  return new WebPlayerProvider({
    host,
    service: "apple",
    label: "Apple Music",
    local,
    linkToId: songIdFromLink,
  });
}
