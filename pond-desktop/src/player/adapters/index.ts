// The registry of services the player can drive. Adding one is a file here and a line below; the
// bridge, the host and the extension already speak only in the player's own terms.

import type { PlayerAdapter } from "../types";
import {
  AppleMusicKitAdapter,
  loadMusicKitFromApple,
} from "./appleMusicKit";
import { widevineProblem } from "./drm";
import { SpotifyWebPlaybackAdapter, loadSpotifySdk } from "./spotifyWebPlayback";

export interface AdapterContext {
  /** Apple: a developer token the host signs, since the host holds the key. */
  fetchDeveloperToken(): Promise<string>;
  /** Spotify: the person's own access token, held by the host. `refresh` asks for a new one. */
  fetchUserToken(service: string, refresh: boolean): Promise<string>;
}

const ADAPTERS: Record<string, (ctx: AdapterContext) => PlayerAdapter> = {
  apple: (ctx) =>
    new AppleMusicKitAdapter({
      loadMusicKit: loadMusicKitFromApple,
      fetchDeveloperToken: ctx.fetchDeveloperToken,
      checkDrm: () => widevineProblem("Apple Music"),
    }),
  spotify: (ctx) =>
    new SpotifyWebPlaybackAdapter({
      loadSdk: loadSpotifySdk,
      fetchUserToken: (refresh) => ctx.fetchUserToken("spotify", refresh),
      checkDrm: () => widevineProblem("Spotify"),
    }),
};

export function knownServices(): string[] {
  return Object.keys(ADAPTERS);
}

/** The adapter for `service`, or null when the player has none. */
export function createAdapter(
  service: string,
  ctx: AdapterContext,
): PlayerAdapter | null {
  return ADAPTERS[service]?.(ctx) ?? null;
}
