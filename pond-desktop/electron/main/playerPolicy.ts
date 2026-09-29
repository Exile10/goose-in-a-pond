// The network policy for the music player window. Chromium makes every request in that window,
// so the Rust egress gate never sees them; this asks the server, which owns `network_mode`, before
// each new host and lets the answer stand for a minute. The player has hundreds of requests a song
// and a handful of hosts, so a per-host answer is both cheap and enough.

export interface PolicyDeps {
  serverUrl(): string;
  fetchFn?: typeof fetch;
  now?: () => number;
  /** How long an answer stands. */
  ttlMs?: number;
  /** How long a failure to ask stands: short, so a server coming back is noticed. */
  failureTtlMs?: number;
}

export interface Verdict {
  cancel: boolean;
}

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]", "::1"]);
const NETWORK_SCHEMES = new Set(["http:", "https:", "ws:", "wss:"]);

interface Answer {
  allowed: boolean;
  until: number;
}

/**
 * Returns the decision function for one request. Not the network: app://, blob:, data: and
 * loopback pass untouched (the server API is loopback). A host the server cannot be asked about is
 * refused, since without the server the player has no commands and nothing worth a leak.
 */
export function createRequestPolicy(
  deps: PolicyDeps,
): (url: string, method: string) => Promise<Verdict> {
  const ask = deps.fetchFn ?? fetch;
  const now = deps.now ?? Date.now;
  const ttl = deps.ttlMs ?? 60_000;
  const failureTtl = deps.failureTtlMs ?? 5_000;
  const answers = new Map<string, Answer>();
  const inFlight = new Map<string, Promise<boolean>>();

  async function consult(url: URL, method: string): Promise<boolean> {
    const host = url.host;
    try {
      const res = await ask(`${deps.serverUrl()}/api/v1/player/egress-policy`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        // The origin only: a path and query can carry identifiers, and the policy judges hosts.
        body: JSON.stringify({ url: `${url.origin}/`, method }),
        signal: AbortSignal.timeout(3_000),
      });
      const body = (await res.json()) as { allowed?: boolean };
      const allowed = res.ok && body.allowed === true;
      answers.set(host, { allowed, until: now() + ttl });
      return allowed;
    } catch {
      answers.set(host, { allowed: false, until: now() + failureTtl });
      return false;
    }
  }

  return async (rawUrl, method) => {
    let url: URL;
    try {
      url = new URL(rawUrl);
    } catch {
      return { cancel: false };
    }
    if (!NETWORK_SCHEMES.has(url.protocol) || LOOPBACK.has(url.hostname)) {
      return { cancel: false };
    }

    const known = answers.get(url.host);
    if (known && known.until > now()) return { cancel: !known.allowed };

    let pending = inFlight.get(url.host);
    if (!pending) {
      pending = consult(url, method).finally(() => inFlight.delete(url.host));
      inFlight.set(url.host, pending);
    }
    return { cancel: !(await pending) };
  };
}
