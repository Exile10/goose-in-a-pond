import type { PlayerState } from "../player/types";

/** What the player last reported for a service, as `GET /player/state` answers it. */
export interface PlayerReply {
  attached: boolean;
  state: PlayerState | null;
}

/** What the sign-in row shows. `note` is a sentence for a person, when there is one to say. */
export type SignInView =
  | { kind: "not_in_shell"; note: string }
  | { kind: "starting"; note: string }
  | { kind: "unavailable"; note: string }
  | { kind: "ready"; note?: string }
  | { kind: "signing_in"; note?: string }
  | { kind: "signed_in" };

/**
 * One decision, kept out of the component so it can be tested: given what the desktop shell and the
 * player say, what does the person see. The one button appears only when pressing it can work.
 */
export function signInView(input: {
  /** The music player lives in the desktop shell; a browser or a phone cannot sign in to it. */
  desktop: boolean;
  /** Null when the player could not be asked. */
  reply: PlayerReply | null;
  /** The button was pressed and the sign-in has not finished. */
  pending: boolean;
  label: string;
}): SignInView {
  const { desktop, reply, pending, label } = input;
  if (!desktop) {
    return {
      kind: "not_in_shell",
      note: `Sign in to ${label} from the Goose In A Pond app on the computer that plays your music.`,
    };
  }
  if (!reply || !reply.attached || !reply.state) {
    return { kind: "starting", note: "The music player is starting. This takes a few seconds." };
  }

  const state = reply.state;
  if (state.need === "none") return { kind: "signed_in" };
  if (state.need === "setup") {
    return {
      kind: "unavailable",
      note: state.message ?? `${label} is not set up on this pond yet.`,
    };
  }
  // Waiting for a sign-in. While one is under way, a message here is why the last try failed.
  return pending
    ? { kind: "signing_in", ...(state.message ? { note: state.message } : {}) }
    : { kind: "ready", ...(state.message ? { note: state.message } : {}) };
}

/** Advanced fields sit apart from the ordinary ones; the order within each is the registry's. */
export function splitAdvanced<T extends { advanced?: boolean }>(
  requirements: T[],
): { ordinary: T[]; advanced: T[] } {
  return {
    ordinary: requirements.filter((r) => !r.advanced),
    advanced: requirements.filter((r) => r.advanced === true),
  };
}
