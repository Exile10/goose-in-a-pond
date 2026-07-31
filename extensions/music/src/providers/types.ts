export interface TrackInfo {
  id: string;
  name: string;
  artist: string;
  album: string;
  duration_ms: number;
  uri: string;
  is_playing?: boolean;
  progress_ms?: number;
  volume_percent?: number;
}

/** An artist, as returned by the taste endpoints. */
export interface ArtistInfo {
  id: string;
  name: string;
  genres: string[];
}

/** How far back the taste endpoints look. */
export type TimeRange = "short_term" | "medium_term" | "long_term";

/** Spotify's repeat modes: off, repeat one track, repeat the whole context. */
export type RepeatState = "off" | "track" | "context";

/** A device Spotify can play on — phone, computer, speaker. */
export interface DeviceInfo {
  id: string;
  name: string;
  /** Spotify's own label: "Computer", "Smartphone", "Speaker", "TV"... */
  type: string;
  is_active: boolean;
  volume_percent?: number;
}

export interface PlaylistInfo {
  id: string;
  name: string;
  description: string;
  track_count: number;
  uri: string;
  /** Display name of whoever created it. */
  owner: string;
  /** True when the signed-in user created it, false when they only follow it. */
  is_own: boolean;
}

export interface MusicProvider {
  name: string;
  play(uri?: string): Promise<string>;
  pause(): Promise<string>;
  next(): Promise<string>;
  previous(): Promise<string>;
  setVolume(percent: number): Promise<string>;
  setShuffle(enabled: boolean): Promise<string>;
  seek(positionMs: number): Promise<string>;
  setRepeat(state: RepeatState): Promise<string>;
  getDevices(): Promise<DeviceInfo[]>;
  transferPlayback(deviceId: string, deviceName: string): Promise<string>;
  getSavedTracks(limit?: number): Promise<TrackInfo[]>;
  getTopTracks(range: TimeRange, limit?: number): Promise<TrackInfo[]>;
  getTopArtists(range: TimeRange, limit?: number): Promise<ArtistInfo[]>;
  getRecentlyPlayed(limit?: number): Promise<TrackInfo[]>;
  getNowPlaying(): Promise<TrackInfo | null>;
  getQueue(): Promise<TrackInfo[]>;
  /** Appends to the queue without disturbing what is currently playing. */
  addToQueue(uri: string): Promise<string>;
  searchTracks(query: string, limit?: number): Promise<TrackInfo[]>;
  searchAlbums(query: string, limit?: number): Promise<AlbumInfo[]>;
  getPlaylists(limit?: number): Promise<PlaylistInfo[]>;
}

export interface AlbumInfo {
  id: string;
  name: string;
  artist: string;
  total_tracks: number;
  uri: string;
  release_date: string;
}
