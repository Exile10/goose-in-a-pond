// The registry of services the player can drive. Adding one is a file here and a line below; the
// bridge, the host and the extension already speak only in the player's own terms.

import type { PlayerAdapter } from "../types";
import {
  AppleMusicKitAdapter,
  loadMusicKitFromApple,
} from "./appleMusicKit";
import { widevineProblem } from "./drm";

export interface AdapterContext {
  fetchDeveloperToken(): Promise<string>;
}

const ADAPTERS: Record<string, (ctx: AdapterContext) => PlayerAdapter> = {
  apple: (ctx) =>
    new AppleMusicKitAdapter({
      loadMusicKit: loadMusicKitFromApple,
      fetchDeveloperToken: ctx.fetchDeveloperToken,
      checkDrm: widevineProblem,
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
