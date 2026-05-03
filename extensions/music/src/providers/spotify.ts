import type { MusicProvider, TrackInfo, PlaylistInfo, AlbumInfo } from './types.js';

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
  tracks: { total: number };
  uri: string;
}

export class SpotifyProvider implements MusicProvider {
  name = 'Spotify';
  private baseUrl = 'https://api.spotify.com/v1';

  private get token(): string {
    const t = process.env.SPOTIFY_ACCESS_TOKEN;
    if (!t) {
      throw new Error('SPOTIFY_ACCESS_TOKEN not set. Sign in via GIAP Extensions.');
    }
    return t;
  }

  private async api<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
    const resp = await fetch(`${this.baseUrl}${path}`, {
      method,
      headers: {
        'Authorization': `Bearer ${this.token}`,
        'Content-Type': 'application/json',
      },
      body: body ? JSON.stringify(body) : undefined,
    });

    if (resp.status === 204) {
      return {} as T;
    }

    if (!resp.ok) {
      const err = await resp.text();
      throw new Error(`Spotify API ${resp.status}: ${err}`);
    }

    return resp.json() as Promise<T>;
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

  private parsePlaylist(playlist: SpotifyPlaylist): PlaylistInfo {
    return {
      id: playlist.id,
      name: playlist.name,
      description: playlist.description || '',
      track_count: playlist.tracks.total,
      uri: playlist.uri,
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

    await this.api('PUT', '/me/player/play', Object.keys(body).length > 0 ? body : undefined);
    return uri ? `Playing ${uri}` : 'Resumed playback';
  }

  async pause(): Promise<string> {
    await this.api('PUT', '/me/player/pause');
    return 'Playback paused';
  }

  async next(): Promise<string> {
    await this.api('POST', '/me/player/next');
    return 'Skipped to next track';
  }

  async previous(): Promise<string> {
    await this.api('POST', '/me/player/previous');
    return 'Went to previous track';
  }

  async setVolume(percent: number): Promise<string> {
    const clamped = Math.max(0, Math.min(100, Math.round(percent)));
    await this.api('PUT', `/me/player/volume?volume_percent=${clamped}`);
    return `Volume set to ${clamped}%`;
  }

  async setShuffle(enabled: boolean): Promise<string> {
    await this.api('PUT', `/me/player/shuffle?state=${enabled}`);
    return `Shuffle ${enabled ? 'enabled' : 'disabled'}`;
  }

  async getNowPlaying(): Promise<TrackInfo | null> {
    interface PlayerState {
      item: SpotifyTrack | null;
      is_playing: boolean;
      progress_ms: number;
    }

    try {
      const data = await this.api<PlayerState>('GET', '/me/player/currently-playing');
      if (!data || !data.item) return null;

      const track = this.parseTrack(data.item);
      track.is_playing = data.is_playing;
      track.progress_ms = data.progress_ms;
      return track;
    } catch {
      return null;
    }
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

  async searchTracks(query: string, limit: number = 10): Promise<TrackInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));
    const encoded = encodeURIComponent(query);

    interface SearchResponse {
      tracks: { items: SpotifyTrack[] };
    }

    const data = await this.api<SearchResponse>(
      'GET',
      `/search?type=track&q=${encoded}&limit=${clamped}`
    );

    return (data.tracks?.items || []).map(t => this.parseTrack(t));
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

  async getPlaylists(limit: number = 20): Promise<PlaylistInfo[]> {
    const clamped = Math.max(1, Math.min(50, limit));

    interface PlaylistsResponse {
      items: SpotifyPlaylist[];
    }

    const data = await this.api<PlaylistsResponse>(
      'GET',
      `/me/playlists?limit=${clamped}`
    );

    return (data.items || []).map(p => this.parsePlaylist(p));
  }

  async getPlaylistTracks(playlistId: string): Promise<TrackInfo[]> {
    interface PlaylistTracksResponse {
      items: Array<{ track: SpotifyTrack }>;
    }

    const data = await this.api<PlaylistTracksResponse>(
      'GET',
      `/playlists/${playlistId}/tracks?limit=50`
    );

    return (data.items || [])
      .filter(item => item.track)
      .map(item => this.parseTrack(item.track));
  }

  async createPlaylist(name: string, description?: string): Promise<PlaylistInfo> {
    interface UserResponse {
      id: string;
    }

    const user = await this.api<UserResponse>('GET', '/me');

    const playlist = await this.api<SpotifyPlaylist>(
      'POST',
      `/users/${user.id}/playlists`,
      {
        name,
        description: description || '',
        public: false,
      }
    );

    return this.parsePlaylist(playlist);
  }

  async addToPlaylist(playlistId: string, trackUris: string[]): Promise<string> {
    await this.api('POST', `/playlists/${playlistId}/tracks`, {
      uris: trackUris,
    });

    return `Added ${trackUris.length} track(s) to playlist`;
  }
}
