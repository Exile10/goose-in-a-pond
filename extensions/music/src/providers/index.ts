import { log } from "../log.js";
import { AppleMusicProvider } from "./apple-music.js";
import { defaultStorefront, ItunesSearch } from "./apple/catalog.js";
import { DeveloperTokens } from "./apple/developer-token.js";
import { EgressGate } from "./apple/egress.js";
import { MusicApp } from "./apple/music-app.js";
import { runAppleScript } from "./apple/osascript.js";
import { AppleMusicApi } from "./apple/rest.js";
import { chooseService } from "./select.js";
import { SpotifyProvider } from "./spotify.js";
import type { MusicProvider } from "./types.js";

function createApple(env: NodeJS.ProcessEnv): AppleMusicProvider {
  const hostUrl = env.GIAP_SERVER_URL || "http://127.0.0.1:4000";
  const internalToken = env.GIAP_INTERNAL_TOKEN ?? "";

  const egress = new EgressGate(fetch, hostUrl, internalToken);
  const storefront = defaultStorefront(env.APPLE_MUSIC_STOREFRONT, Intl.DateTimeFormat().resolvedOptions().locale);
  const tokens = new DeveloperTokens(fetch, hostUrl, internalToken);

  return new AppleMusicProvider({
    app: new MusicApp(runAppleScript),
    fallbackCatalog: new ItunesSearch(fetch, egress, storefront),
    rest: env.APPLE_MUSIC_USER_TOKEN
      ? new AppleMusicApi(fetch, egress, tokens, env.APPLE_MUSIC_USER_TOKEN)
      : null,
  });
}

export function createProvider(
  env: NodeJS.ProcessEnv = process.env,
  platform: string = process.platform,
): MusicProvider {
  const { service, reason } = chooseService(env, platform);
  log.info("service_chosen", `using ${service}`, { service, reason });
  return service === "apple" ? createApple(env) : new SpotifyProvider();
}
