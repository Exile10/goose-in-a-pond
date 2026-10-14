// How a refused host credential is worded and paced. A browser tab keeps the credential its
// sign-in link gave it, so a refusal there is final. The desktop shell reads the current one for
// every request: one refusal is usually the Pond restarting between the read and the request, and
// only repeated ones mean the shell and the Pond disagree, as when they use different data directories.

import { isDesktopShell } from "../shell";

export type SignInRefusalKey = "signIn.hostOnly" | "signIn.shellMismatch";

/** Refusals in a row the shell lets pass, as a Pond restart, before it tells the user. */
export const SHELL_REFUSAL_GRACE = 2;
export const REFUSAL_RETRY_MS = 5_000;
export const REFUSAL_RETRY_CAP_MS = 60_000;

export interface Refusal {
  key: SignInRefusalKey;
  /** Whether to show `key` yet: at once in a browser, past the grace in the shell. */
  notice: boolean;
  /** Wait before asking again; null when asking again cannot help until the user acts. */
  retryAfterMs: number | null;
}

/** The text to show a user who was refused, for wherever this app runs. */
export function signInRefusalKey(inShell: boolean = isDesktopShell()): SignInRefusalKey {
  return inShell ? "signIn.shellMismatch" : "signIn.hostOnly";
}

/** How to treat the `count`th refusal in a row (1 is the first). */
export function describeRefusal(count: number, inShell: boolean = isDesktopShell()): Refusal {
  if (!inShell) return { key: signInRefusalKey(false), notice: true, retryAfterMs: null };
  const doublings = Math.max(0, count - SHELL_REFUSAL_GRACE);
  return {
    key: signInRefusalKey(true),
    notice: count > SHELL_REFUSAL_GRACE,
    retryAfterMs: Math.min(REFUSAL_RETRY_MS * 2 ** doublings, REFUSAL_RETRY_CAP_MS),
  };
}

/** Counts the refusals in a row on one screen; `clear` after a call is admitted. */
export function refusalCounter(): { refused: () => Refusal; clear: () => void } {
  let count = 0;
  return {
    refused: () => describeRefusal(++count),
    clear: () => {
      count = 0;
    },
  };
}
