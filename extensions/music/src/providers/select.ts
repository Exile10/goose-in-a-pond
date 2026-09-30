import type { ServiceId } from "./types.js";

export interface Choice {
  /** Null: no music service is offered to the assistant here. */
  service: ServiceId | null;
  /** Why, for the log and for the assistant: a missing service is otherwise invisible. */
  reason: string;
}

/**
 * The assistant plays Apple Music, and only on a Mac, where the Music app and the music player page
 * are. Spotify is never offered to the assistant: Spotify's Developer Policy forbids an app that
 * controls Spotify by voice (III.3), and its Developer Terms forbid feeding Spotify content into an
 * AI model (IV.2.a.i), which every tool result would do. Spotify plays in the pond's Spotify player
 * page instead, controlled by hand there or with the app's music controls.
 */
export function chooseService(platform: string): Choice {
  if (platform === "darwin") return { service: "apple", reason: "Apple Music, on macOS" };
  return {
    service: null,
    reason: "Apple Music needs macOS, and Spotify cannot be controlled by the assistant",
  };
}

/** What the assistant is told when it has no music tools, so it can say why instead of guessing. */
export const NO_MUSIC_INSTRUCTIONS =
  "No music service can be controlled by the assistant on this computer. Apple Music needs a Mac. " +
  "Spotify is never controlled by the assistant, because Spotify's developer rules do not allow " +
  "voice or AI control of Spotify: the person plays Spotify in the Spotify app, or in Goose In A " +
  "Pond's Spotify player page, and controls it with the app's music controls. If they ask you to " +
  "play or control music, tell them that, in a sentence.";
