import { describeError, log } from "../../log.js";
import type { CatalogSong, CatalogSource } from "./catalog.js";
import { DeveloperTokens, NotConfigured } from "./developer-token.js";
import type { EgressGate, Fetch } from "./egress.js";

const BASE = "https://api.music.apple.com";

interface ApiSong {
  id: string;
  attributes?: {
    name?: string;
    artistName?: string;
    albumName?: string;
    durationInMillis?: number;
    url?: string;
  };
}

function songFromApi(s: ApiSong): CatalogSong | null {
  const a = s.attributes;
  if (!s.id || !a?.name) return null;
  return {
    id: s.id,
    name: a.name,
    artist: a.artistName ?? "",
    album: a.albumName ?? "",
    duration_ms: a.durationInMillis ?? 0,
    url: a.url ?? null,
  };
}

/**
 * The Apple Music API, which needs the host-signed developer token and the user's Music User
 * Token. It has no playback: it finds catalog songs and puts them in the library, where the
 * Music app can play them.
 */
export class AppleMusicApi implements CatalogSource {
  private storefrontId: string | null = null;

  constructor(
    private readonly fetchFn: Fetch,
    private readonly egress: EgressGate,
    private readonly tokens: DeveloperTokens,
    private readonly userToken: string | undefined,
  ) {}

  /** Whether the user has both credentials; false, not an error, when they have not. */
  async available(): Promise<boolean> {
    if (!this.userToken) return false;
    try {
      await this.tokens.get();
      return true;
    } catch (error) {
      if (error instanceof NotConfigured) return false;
      log.warn("apple_music_token_unavailable", "could not get a developer token", {
        error: describeError(error),
      });
      return false;
    }
  }

  private async call(method: string, path: string): Promise<Response> {
    const url = `${BASE}${path}`;
    await this.egress.allow(url, method);

    const send = async () =>
      this.fetchFn(url, {
        method,
        headers: {
          Authorization: `Bearer ${await this.tokens.get()}`,
          "Music-User-Token": this.userToken ?? "",
        },
        signal: AbortSignal.timeout(10_000),
      });

    let resp = await send();
    if (resp.status === 401) {
      // A developer token the host issued can still be refused, for example after a key rotation.
      this.tokens.invalidate();
      resp = await send();
    }

    if (resp.ok) return resp;

    const body = (await resp.text().catch(() => "")).slice(0, 300);
    log.warn("apple_music_api_failed", "Apple Music refused a request", {
      method,
      path,
      status: resp.status,
      body,
    });

    if (resp.status === 401 || resp.status === 403) {
      throw new Error(
        "Apple Music rejected the saved sign-in. Sign in to Apple Music again from the Extensions tab, and check the account has an active subscription.",
      );
    }
    if (resp.status === 429) throw new Error("Apple Music is rate limiting requests. Try again shortly.");
    throw new Error(`Apple Music API ${resp.status}`);
  }

  private async storefront(): Promise<string> {
    if (this.storefrontId) return this.storefrontId;
    const resp = await this.call("GET", "/v1/me/storefront");
    const body = (await resp.json()) as { data?: Array<{ id: string }> };
    const id = body.data?.[0]?.id;
    if (!id) throw new Error("Apple Music did not say which store this account uses.");
    this.storefrontId = id;
    return id;
  }

  async searchSongs(query: string, limit: number): Promise<CatalogSong[]> {
    const store = await this.storefront();
    const params = new URLSearchParams({
      term: query,
      types: "songs",
      limit: String(Math.max(1, Math.min(25, limit))),
    });
    const resp = await this.call("GET", `/v1/catalog/${store}/search?${params}`);
    const body = (await resp.json()) as { results?: { songs?: { data?: ApiSong[] } } };
    return (body.results?.songs?.data ?? []).map(songFromApi).filter((s): s is CatalogSong => s !== null);
  }

  async lookupSong(id: string): Promise<CatalogSong | null> {
    const store = await this.storefront();
    const resp = await this.call("GET", `/v1/catalog/${store}/songs/${encodeURIComponent(id)}`);
    const body = (await resp.json()) as { data?: ApiSong[] };
    return body.data?.map(songFromApi).find((s): s is CatalogSong => s !== null) ?? null;
  }

  /** Adds a catalog song to the user's library; Apple answers 202 and the song syncs shortly after. */
  async addSongToLibrary(id: string): Promise<void> {
    const params = new URLSearchParams({ "ids[songs]": id });
    await this.call("POST", `/v1/me/library?${params}`);
  }
}
