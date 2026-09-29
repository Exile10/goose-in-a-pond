import type { ServiceId } from "./types.js";

export interface Choice {
  service: ServiceId;
  /** Why, for the log: a wrong pick is otherwise invisible. */
  reason: string;
}

/**
 * One service is active at a time, which keeps the tool list, and so the prompt, the same size
 * however many services exist. `MUSIC_SERVICE` is an explicit choice; otherwise Spotify wins when
 * the user has signed in to it, and Apple Music is the answer on a Mac with no Spotify sign-in.
 */
export function chooseService(
  env: { MUSIC_SERVICE?: string; SPOTIFY_ACCESS_TOKEN?: string },
  platform: string,
): Choice {
  const asked = (env.MUSIC_SERVICE ?? "").trim().toLowerCase().replace(/[\s_-]+/g, "");

  if (asked === "spotify") return { service: "spotify", reason: "MUSIC_SERVICE is spotify" };

  if (asked === "apple" || asked === "applemusic") {
    return platform === "darwin"
      ? { service: "apple", reason: "MUSIC_SERVICE is apple" }
      : { service: "spotify", reason: "MUSIC_SERVICE asked for Apple Music, which needs macOS" };
  }

  const unrecognised = asked !== "" && asked !== "auto";
  const prefix = unrecognised ? `MUSIC_SERVICE "${asked}" is not a service; ` : "";

  if (env.SPOTIFY_ACCESS_TOKEN) return { service: "spotify", reason: `${prefix}signed in to Spotify` };
  if (platform === "darwin") return { service: "apple", reason: `${prefix}no Spotify sign-in, on macOS` };
  return { service: "spotify", reason: `${prefix}no Spotify sign-in, and Apple Music needs macOS` };
}
