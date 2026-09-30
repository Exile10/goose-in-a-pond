// The host credential that pairing codes and remote-access management require. The desktop
// shell reads it from the server's data directory; a browser gets it once, from the sign-in
// link `pond-server dashboard` prints, and keeps it for that tab only.

import { invoke, isDesktopShell } from "../shell";
import { ApiError } from "./types";

const STORAGE_KEY = "giap-host-credential";
const FRAGMENT_KEY = "host";
const CREDENTIAL = /^[A-Za-z0-9_-]{43}$/;

/**
 * Move a `#host=<credential>` sign-in link out of the address bar into session storage, so the
 * credential is not left in history, bookmarks or screenshots. Anything else in the fragment
 * is kept. Call once, before the app renders.
 */
export function captureSignInLink(
  location: Pick<Location, "hash" | "pathname" | "search">,
  history: Pick<History, "replaceState">,
  storage: Pick<Storage, "setItem">,
): boolean {
  const fragment = new URLSearchParams(location.hash.replace(/^#/, ""));
  const credential = fragment.get(FRAGMENT_KEY);
  if (credential === null) return false;
  fragment.delete(FRAGMENT_KEY);
  const rest = fragment.toString();
  history.replaceState(null, "", `${location.pathname}${location.search}${rest ? `#${rest}` : ""}`);
  if (!CREDENTIAL.test(credential)) {
    console.warn("[host] ignored a malformed sign-in link");
    return false;
  }
  try {
    storage.setItem(STORAGE_KEY, credential);
  } catch (e) {
    console.warn("[host] could not keep the sign-in credential for this tab", e);
    return false;
  }
  return true;
}

/** The current credential, or null when this browser tab has not been signed in. */
export async function hostCredential(): Promise<string | null> {
  if (isDesktopShell()) return invoke("host_credential");
  try {
    return sessionStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

/** The server refused because this tab or app lacks the current host credential. */
export function isSignInRequired(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status === 403 &&
    error.message === "host_credential_required"
  );
}
