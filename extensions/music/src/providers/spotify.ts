import type { MusicProvider, TrackInfo, PlaylistInfo, AlbumInfo, DeviceInfo, RepeatState, ArtistInfo, TimeRange } from './types.js';

interface SpotifyTrack {
  id: string;
  name: string;
  artists: Array<{ name: string }>;
  album: { name: string };
  duration_ms: number;
  uri: string;
}

interface SpotifyAlbum {
  id: string;
  name: string;
  artists: Array<{ name: string }>;
  total_tracks: number;
  uri: string;
  release_date: string;
}

interface SpotifyPlaylist {
  id: string;
  name: string;
  description: string;
  /** Absent on some /me/playlists entries, which carry `items` instead. */
  tracks?: { total: number };
  items?: unknown[];
  uri: string;
  owner?: { id: string; display_name?: string };
}

/**
 * Endpoints Spotify withdrew from apps created after 2024-11-27, which includes
 * GIAP's. Verified against a live token — these are not a scope problem and
 * asking for more permissions will not bring them back:
 *
 *   GET /recommendations                     404
 *   GET /recommendations/available-genre-seeds  404
 *   GET /audio-features/{id}                 403
 *   GET /audio-analysis/{id}                 403
 *   GET /artists/{id}/related-artists        403
 *   GET /artists/{id}/top-tracks             403
 *   GET /browse/featured-playlists           403
 *   GET /browse/new-releases                 403
 *   track.preview_url                        always null
 *
 * So there is no "play me something like this", no mood or tempo matching, and
 * no 30-second previews. Do not build features that depend on them.
 */
export class SpotifyProvider implements MusicProvider {
  name = 'Spotify';
  private baseUrl = 'https://api.spotify.com/v1';

  /** Current access token — initialized from env, updated on refresh. */
  private accessToken: string | null = process.env.SPOTIFY_ACCESS_TOKEN ?? null;

  private get token(): string {
    if (!this.accessToken) {
      throw new Error('SPOTIFY_ACCESS_TOKEN not set. Sign in via GIAP Extensions.');
    }
    return this.accessToken;
  }

  /** GIAP server URL for OAuth refresh requests. */
  private readonly giapUrl = process.env.GIAP_SERVER_URL || 'http://127.0.0.1:4000';

  /**
   * Ask GIAP to refresh the Spotify token, then update the in-memory
   * token from the response so we can retry without a process restart.
   */
  private async refreshToken(): Promise<boolean> {
    try {
      const refreshResp = await fetch(`${this.giapUrl}/api/v1/oauth/refresh`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${process.env.GIAP_INTERNAL_TOKEN ?? ''}`,
        },
        body: JSON.stringify({ provider: 'spotify' }),
      });
      if (!refreshResp.ok) return false;

      const data = await refreshResp.json() as { refreshed?: boolean; access_token?: string };
      if (data.access_token) {
        this.accessToken = data.access_token;
        return true;
      }
    } catch { /* GIAP may be unreachable */ }
    return false;
  }

  /**
   * Issues a request, refreshing the token and retrying once on a 401.
   *
   * Returns the raw `Response` and reads nothing from it — whether there is a
   * body, and what it means, is the caller's business.
   */
  private async request(method: string, path: string, body?: unknown): Promise<Response> {
    // Re-read the `token` getter on each attempt: a refresh replaces the token
    // in place, and the retry has to send the new one.
    const send = () => fetch(`${this.baseUrl}${path}`, {
      method,
      headers: {
        'Authorization': `Bearer ${this.token}`,
        'Content-Type': 'application/json',
      },
      body: body ? JSON.stringify(body) : undefined,
    });

    let resp = await send();
    let afterRefresh = '';

    if (resp.status === 401) {
      if (!await this.refreshToken()) {
        throw new Error(
          'Spotify token expired and refresh failed. Re-authenticate via GIAP Extensions.'
        );
      }
      resp = await send();
      afterRefresh = ' (after refresh)';
    }

    if (!resp.ok) {
      const body = await resp.text();

      // A scope the token was never granted. Distinct from the withdrawn
      // endpoints above, which answer 403 with a bare "Forbidden" and stay
      // broken however many times the user signs in — this one is fixed by
      // re-authorising, so say that instead of surfacing a raw 403.
      if (resp.status === 403 && body.includes('Insufficient client scope')) {
        throw new Error(
          'Spotify has not granted GIAP this permission yet. Sign in to Spotify again ' +
            'from the Extensions tab to authorise it.'
        );
      }

      throw new Error(`Spotify API ${resp.status}${afterRefresh}: ${body}`);
    }

    return resp;
  }

  /**
   * Issues a request whose response body is of no interest.
   *
   * The player-control endpoints are documented to answer 204, but Spotify
   * actually answers `POST /me/player/next` with a 200 that carries no
   * content-type and a 27-byte opaque token. Parsing that as JSON is what made
   * every skip fail with `Unexpected token ... is not valid JSON` — on a body
   * no caller has ever read.
   */
  private async command(method: string, path: string, body?: unknown): Promise<void> {
    await this.request(method, path, body);
  }

  /**
   * Issues a request and parses a JSON body, tolerating a bodyless success.
   *
   * Reads the body as text first: `Response.json()` throws on an empty body,
   * and an empty 200 or a 204 is a legitimate answer to several of these calls.
   * A non-empty body that is not JSON is still an error worth surfacing.
   */
  private async api<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
    const resp = await this.request(method, path, body);
    const text = await resp.text();
    if (!text.trim()) return {} as T;
    return JSON.parse(text) as T;
  }

  private parseTrack(track: SpotifyTrack): TrackInfo {
    return {
      id: track.id,
      name: track.name,
      artist: track.artists.map(a => a.name).join(', '),
      album: track.album.name,
      duration_ms: track.duration_ms,
      uri: track.uri,
    };
  }

  private parseAlbum(album: SpotifyAlbum): AlbumInfo {
    return {
      id: album.id,
      name: album.name,
      artist: album.artists.map(a => a.name).join(', '),
      total_tracks: album.total_tracks,
      uri: album.uri,
      release_date: album.release_date,
    };
  }

  private parsePlaylist(playlist: SpotifyPlaylist, currentUserId?: string): PlaylistInfo {
    const owner = playlist.owner;
    return {
      id: playlist.id,
      name: playlist.name,
      description: playlist.description || '',
      // `tracks` is not always present. /me/playlists returns some entries with
      // an `items` array and no `tracks` object at all, so reading
      // `playlist.tracks.total` outright throws on a perfectly ordinary account.
      track_count: playlist.tracks?.total ?? playlist.items?.length ?? 0,
      uri: playlist.uri,
      owner: owner?.display_name || owner?.id || 'unknown',
      is_own: currentUserId !== undefined && owner?.id === currentUserId,
    };
  }

  async play(uri?: string): Promise<string> {
    const body: Record<string, unknown> = {};

    if (uri) {
      if (uri.startsWith('spotify:track:')) {
        body.uris = [uri];
      } else {
        // Album, playlist, or artist URI -- use as context
        body.context_uri = uri;
      }
    }

    await this.command('PUT', '/me/player/play', Object.keys(body).length > 0 ? body : undefined);
    return uri ? `Playing ${uri}` : 'Resumed playback';
  }

  async pause(): Promise<string> {
    await this.command('PUT', '/me/player/pause');
    return 'Playback paused';
  }

  async next(): Promise<string> {
    await this.command('POST', '/me/player/next');
    return 'Skipped to next track';
  }

  async previous(): Promise<string> {
    await this.command('POST', '/me/player/previous');
    return 'Went to previous track';
  }

  async setVolume(percent: number): Promise<string> {
    const clamped = Math.max(0, Math.min(100, Math.round(percent)));
    await this.command('PUT', `/me/player/volume?volume_percent=${clamped}`);
    return `Volume set to ${clamped}%`;
  }

  async setShuffle(enabled: boolean): Promise<string> {
    await this.command('PUT', `/me/player/shuffle?state=${enabled}`);
    return `Shuffle ${enabled ? 'enabled' : 'disabled'}`;
  }

  async seek(positionMs: number): Promise<string> {
    const clamped = Math.max(0, Math.round(positionMs));
    await this.command('PUT', `/me/player/seek?position_ms=${clamped}`);
    const mins = Math.floor(clamped / 60000);
    const secs = String(Math.floor((clamped % 60000) / 1000)).padStart(2, '0');
    return `Jumped to ${mins}:${secs}`;
  }

  async setRepeat(state: RepeatState): Promise<string> {
    await this.command('PUT', `/me/player/repeat?state=${state}`);
    const label =
      state === 'off'
        ? 'Repeat off'
        : state === 'track'
          ? 'Repeating this track'
          : 'Repeating the album or playlist';
    return label;
  }

  async getDevices(): Promise<DeviceInfo[]> {
    interface DevicesResponse {
      devices: Array<{
        id: string | null;
        name: string;
        type: string;
        is_active: boolean;
        volume_percent?: number;
      }>;
    }

    const data = await this.api<DevicesResponse>('GET', '/me/player/devices');
    return (data.devices || [])
      // A device with no id cannot be targeted for transfer, so it is not worth
      // offering as somewhere to send playback.
      .filter(d => d.id)
      .map(d => ({
        id: d.id as string,
        name: d.name,
        type: d.type,
        is_active: d.is_active,
        volume_percent: d.volume_percent,
      }));
  }

  async transferPlayback(deviceId: string, deviceName: string): Promise<string> {
    // `play: true` keeps it playing across the move; without it Spotify can
    // hand the device the track in a paused state, which reads as a failure.
    await this.command('PUT', '/me/player', { device_ids: [deviceId], play: true });
    return `Playback moved to ${deviceName}`;
  }

  // ── Library and listening history ──────────────────────────
  // All of these need scopes added after the extension first shipped, so on an
  // install that has not re-authorised they fail with the re-sign-in message
  // from `request`, not a bare 403.

  async getSavedTracks(limit: number = 20): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface SavedResponse {
      items: Array<{ track: SpotifyTrack | null }>;
    }
    const data = await this.api<SavedResponse>('GET', `/me/tracks?limit=${clamped}`);
    return (data.items || [])
      .map(i => i.track)
      .filter((t): t is SpotifyTrack => !!t)
      .map(t => this.parseTrack(t));
  }

  async saveTrack(trackId: string): Promise<string> {
    await this.command('PUT', `/me/tracks?ids=${encodeURIComponent(trackId)}`);
    return 'Saved to your library';
  }

  async removeSavedTrack(trackId: string): Promise<string> {
    await this.command('DELETE', `/me/tracks?ids=${encodeURIComponent(trackId)}`);
    return 'Removed from your library';
  }

  async isSaved(trackId: string): Promise<boolean> {
    const data = await this.api<boolean[]>(
      'GET',
      `/me/tracks/contains?ids=${encodeURIComponent(trackId)}`
    );
    return Array.isArray(data) && data[0] === true;
  }

  async getTopTracks(range: TimeRange, limit: number = 20): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface TopTracks {
      items: SpotifyTrack[];
    }
    const data = await this.api<TopTracks>(
      'GET',
      `/me/top/tracks?time_range=${range}&limit=${clamped}`
    );
    return (data.items || []).map(t => this.parseTrack(t));
  }

  async getTopArtists(range: TimeRange, limit: number = 20): Promise<ArtistInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface TopArtists {
      items: Array<{ id: string; name: string; genres?: string[] }>;
    }
    const data = await this.api<TopArtists>(
      'GET',
      `/me/top/artists?time_range=${range}&limit=${clamped}`
    );
    return (data.items || []).map(a => ({
      id: a.id,
      name: a.name,
      genres: a.genres || [],
    }));
  }

  async getRecentlyPlayed(limit: number = 20): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    interface RecentResponse {
      items: Array<{ track: SpotifyTrack | null }>;
    }
    const data = await this.api<RecentResponse>(
      'GET',
      `/me/player/recently-played?limit=${clamped}`
    );
    return (data.items || [])
      .map(i => i.track)
      .filter((t): t is SpotifyTrack => !!t)
      .map(t => this.parseTrack(t));
  }

  async getNowPlaying(): Promise<TrackInfo | null> {
    interface PlayerState {
      item: SpotifyTrack | null;
      is_playing: boolean;
      progress_ms: number;
      device?: { volume_percent: number };
    }

    // Errors deliberately propagate. `null` here means one thing only —
    // Spotify answered, and nothing is playing — because that is exactly how
    // the caller reports it ("Nothing is currently playing on Spotify"). A
    // `catch` returning null made an expired token, a failed refresh and an
    // unreachable Spotify all indistinguishable from an idle player, which is
    // the most misleading answer available.
    const data = await this.api<PlayerState>('GET', '/me/player');
    if (!data || !data.item) return null;

    const track = this.parseTrack(data.item);
    track.is_playing = data.is_playing;
    track.progress_ms = data.progress_ms;
    track.volume_percent = data.device?.volume_percent;
    return track;
  }

  /**
   * Appends a track to the queue, leaving current playback untouched.
   *
   * This is the only insert Spotify offers: the endpoint takes a `uri` and an
   * optional `device_id` but no position, and there is no reorder endpoint, so
   * "play next" cannot be built on it. Do not let a caller imply otherwise.
   */
  async addToQueue(uri: string): Promise<string> {
    try {
      await this.command('POST', `/me/player/queue?uri=${encodeURIComponent(uri)}`);
    } catch (err) {
      // Spotify answers 404 when no device is active, which reads as "not
      // found" but means "nothing is open to queue onto" — the common case.
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes('Spotify API 404')) {
        throw new Error('No active Spotify device. Open Spotify on a device first.');
      }
      throw err;
    }
    return 'Added to queue';
  }

  async getQueue(): Promise<TrackInfo[]> {
    interface QueueResponse {
      currently_playing: SpotifyTrack | null;
      queue: SpotifyTrack[];
    }

    const data = await this.api<QueueResponse>('GET', '/me/player/queue');
    const tracks: TrackInfo[] = [];

    if (data.currently_playing) {
      const current = this.parseTrack(data.currently_playing);
      current.is_playing = true;
      tracks.push(current);
    }

    for (const item of data.queue || []) {
      tracks.push(this.parseTrack(item));
    }

    return tracks;
  }

  /**
   * Turns "Nairobi by Bensoul" into `track:"Nairobi" artist:"Bensoul"`.
   *
   * Spotify's search has no notion of natural language: every word in `q` is
   * matched as a term, so "by" and a featured artist are scored as if the user
   * had asked for them. "nairobi by bensoul" returns Extravaganza by Sauti Sol;
   * "Intro by quality control ft gucci mane" returns Easy by Nicki Minaj. The
   * field-filtered form returns the right track first in both cases.
   *
   * Returns `null` when the query has no "by", leaving it to be sent as-is.
   */
  private fieldFilteredQuery(query: string): string | null {
    const split = query.match(/^(.*?)\s+by\s+(.*)$/i);
    if (!split) return null;

    const title = split[1].trim();
    // Drop a featured-artist tail: the primary artist is what Spotify indexes
    // under artist:, and the guest usually appears in the track title anyway.
    const artist = split[2]
      .replace(/\s+(feat\.?|ft\.?|featuring|with)\s+.*$/i, '')
      .trim();

    if (!title || !artist) return null;
    // Quote both so multi-word values stay one term.
    return `track:"${title}" artist:"${artist}"`;
  }

  async searchTracks(query: string, limit: number = 10): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));

    interface SearchResponse {
      tracks: { items: SpotifyTrack[] };
    }

    const run = async (q: string) => {
      const data = await this.api<SearchResponse>(
        'GET',
        `/search?type=track&q=${encodeURIComponent(q)}&limit=${clamped}`
      );
      return (data.tracks?.items || []).map(t => this.parseTrack(t));
    };

    // Try the precise form first, but never let it lose results: a strict
    // filter finds nothing when the user misremembers a title, and the loose
    // query still would.
    const filtered = this.fieldFilteredQuery(query);
    if (filtered) {
      const hits = await run(filtered);
      if (hits.length > 0) return hits;
    }

    return run(query);
  }

  async searchAlbums(query: string, limit: number = 10): Promise<AlbumInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    const encoded = encodeURIComponent(query);

    interface SearchResponse {
      albums: { items: SpotifyAlbum[] };
    }

    const data = await this.api<SearchResponse>(
      'GET',
      `/search?type=album&q=${encoded}&limit=${clamped}`
    );

    return (data.albums?.items || []).map(a => this.parseAlbum(a));
  }

  /**
   * Every playlist in the user's library, followed ones included.
   *
   * Spotify caps a page at 50, so a library larger than that has to be paged
   * through: asking for one page silently hid 19 of this account's 69, which
   * meant "which playlists do I have" was wrong and a playlist past the first
   * page could never be found by name.
   */
  async getPlaylists(limit: number = 200): Promise<PlaylistInfo[]> {
    interface PlaylistsResponse {
      items: SpotifyPlaylist[];
      total: number;
    }

    const wanted = Math.max(1, limit);
    const out: PlaylistInfo[] = [];
    let offset = 0;

    // Resolve the owner once so each playlist can be marked as theirs or not.
    const me = await this.api<{ id: string }>('GET', '/me');

    while (out.length < wanted) {
      const page = await this.api<PlaylistsResponse>(
        'GET',
        `/me/playlists?limit=50&offset=${offset}`
      );
      const items = (page.items || []).filter(Boolean);
      out.push(...items.map(p => this.parsePlaylist(p, me.id)));

      offset += 50;
      if (items.length === 0 || offset >= (page.total ?? 0)) break;
    }

    return out.slice(0, wanted);
  }



}
