// ─── The accounts a household can connect ───────────────────────────────────
//
// Mirrors `CalDavProvider` and `ImapProvider` on the Rust side. The `id` values
// are what `context_sources.provider` stores, so they are schema rather than
// display strings: renaming one orphans every source somebody connected.
//
// The setup hints are duplicated from `setup_hint()` in those two crates, and
// that duplication is deliberate rather than an oversight. The server's copy is
// what an API caller sees; this copy is what somebody staring at a password box
// sees, and it has to be here because the box renders before any request is
// made. `providers_match_the_backend.test.ts` fails if the two lists drift.

export type ConnectionKind = "calendar" | "mail";

export interface ProviderOption {
  /** Stored in `context_sources.provider`. */
  id: string;
  label: string;
  kind: ConnectionKind;
  /** What the household has to go and do first. */
  hint: string;
  /** Whether this provider needs a server address as well. */
  needsServer?: boolean;
  /** Placeholder for the server field, when there is one. */
  serverPlaceholder?: string;
}

export const PROVIDERS: ProviderOption[] = [
  {
    id: "google",
    label: "Google Calendar",
    kind: "calendar",
    hint: "Turn on 2-Step Verification, then create an app password on your Google account's Security page. Your normal password will not work.",
  },
  {
    id: "gmail",
    label: "Gmail",
    kind: "mail",
    hint: "Use the same app password as Google Calendar, and switch IMAP on in Gmail's own settings under Forwarding and POP/IMAP. That second step is the one people miss.",
  },
  {
    id: "icloud",
    label: "iCloud Calendar",
    kind: "calendar",
    hint: "Create an app-specific password under Sign-In and Security in your Apple account.",
  },
  {
    id: "icloud",
    label: "iCloud Mail",
    kind: "mail",
    hint: "Use the same app-specific password, and your full iCloud address as the username.",
  },
  {
    id: "fastmail",
    label: "Fastmail Calendar",
    kind: "calendar",
    hint: "Create an app password with calendar access under Settings, Privacy & Security, Connected apps.",
  },
  {
    id: "fastmail",
    label: "Fastmail Mail",
    kind: "mail",
    hint: "Create an app password with mail access under Settings, Privacy & Security, Connected apps.",
  },
  {
    id: "nextcloud",
    label: "Nextcloud Calendar",
    kind: "calendar",
    hint: "Create a device password under Settings, Security. The server address is the one you use in a browser.",
    needsServer: true,
    serverPlaceholder: "https://cloud.example.org/remote.php/dav",
  },
  {
    id: "custom",
    label: "Another mail server",
    kind: "mail",
    hint: "Use your provider's IMAP address. The pond connects over TLS on port 993 and will not fall back to an unencrypted connection.",
    needsServer: true,
    serverPlaceholder: "imap.example.org",
  },
];

/** How a source's status should read to a person, and how urgently.
 *
 * `lastSync` matters as much as the status here. `connected` is the state a
 * source is CREATED in, before anything has run, so reporting it as "working,
 * checked recently" beside a "not checked yet" line was the card contradicting
 * itself on the one screen where somebody is trying to find out whether their
 * password took. */
export function describeStatus(
  status: string,
  lastSync?: string | null,
): {
  label: string;
  tone: "ok" | "warn" | "muted";
  detail: string;
} {
  switch (status) {
    case "connected":
      return lastSync
        ? {
            label: "Connected",
            tone: "ok",
            detail: "Working. The pond read this account and found what it expected.",
          }
        : {
            label: "Not checked yet",
            tone: "muted",
            detail:
              "Saved, but the pond has not read this account yet, so the password is still unproven. Check now to find out.",
          };
    case "needs_reauth":
      return {
        label: "Needs attention",
        tone: "warn",
        // Says what to do, because an unactionable warning is the failure the
        // privacy work exists to prevent.
        detail:
          "The password was refused. Usually it was revoked, or an ordinary account password was used. Reconnect to fix it — the pond has stopped trying.",
      };
    case "paused":
      return {
        label: "Paused",
        tone: "muted",
        detail: "This pond is offline, so it is not reaching out. Nothing is broken.",
      };
    default:
      return {
        label: "Not working",
        tone: "warn",
        detail: "The last check failed. The pond will try again on its own.",
      };
  }
}

/** "2 hours ago", or the honest answer when it has never run. */
export function describeLastSync(iso: string | null, now = Date.now()): string {
  if (!iso) return "Not checked yet";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "Not checked yet";
  const mins = Math.max(0, Math.round((now - then) / 60000));
  if (mins < 1) return "Checked just now";
  if (mins < 60) return `Checked ${mins} minute${mins === 1 ? "" : "s"} ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `Checked ${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.round(hours / 24);
  return `Checked ${days} day${days === 1 ? "" : "s"} ago`;
}
