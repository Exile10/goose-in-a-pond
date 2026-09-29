import type { Fetch } from "./egress.js";

/** The user has not given GIAP an Apple Music key, so there is nothing to sign with. */
export class NotConfigured extends Error {}

/** Refresh this long before Apple would reject the token, so a request never carries a stale one. */
const REFRESH_MARGIN_MS = 5 * 60_000;

/**
 * Developer tokens are signed by the host, which holds the private key; this asks for one and
 * keeps it until shortly before it expires.
 */
export class DeveloperTokens {
  private cached: { token: string; expiresAtMs: number } | null = null;

  constructor(
    private readonly fetchFn: Fetch,
    private readonly hostUrl: string,
    private readonly internalToken: string,
    private readonly now: () => number = Date.now,
  ) {}

  invalidate(): void {
    this.cached = null;
  }

  async get(): Promise<string> {
    if (this.cached && this.cached.expiresAtMs - this.now() > REFRESH_MARGIN_MS) {
      return this.cached.token;
    }

    if (!this.internalToken) {
      throw new NotConfigured("Apple Music catalog sign-in needs the Goose In A Pond app.");
    }

    const resp = await this.fetchFn(`${this.hostUrl}/api/v1/musickit/developer-token`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.internalToken}`,
      },
      body: "{}",
      signal: AbortSignal.timeout(5_000),
    });

    if (resp.status === 400) {
      const body = (await resp.json().catch(() => ({}))) as { error?: string };
      throw new NotConfigured(body.error || "No Apple Music key has been added.");
    }
    if (!resp.ok) {
      throw new Error(`GIAP could not issue an Apple Music developer token (status ${resp.status}).`);
    }

    const data = (await resp.json()) as { token?: string; expires_at?: number };
    if (!data.token || typeof data.expires_at !== "number") {
      throw new Error("GIAP returned an Apple Music developer token with no expiry.");
    }

    this.cached = { token: data.token, expiresAtMs: data.expires_at * 1000 };
    return data.token;
  }
}
