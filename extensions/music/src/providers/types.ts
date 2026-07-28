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

export interface PlaylistInfo {
  id: string;
  name: string;
  description: string;
  track_count: number;
  uri: string;
}

export interface MusicProvider {
  name: string;
  play(uri?: string): Promise<string>;
  pause(): Promise<string>;
  next(): Promise<string>;
  previous(): Promise<string>;
  setVolume(percent: number): Promise<string>;
  setShuffle(enabled: boolean): Promise<string>;
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
