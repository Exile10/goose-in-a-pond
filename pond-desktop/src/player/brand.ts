/**
 * Where a service's official logo is served from, when its brand rules require it next to its
 * content. Spotify's design guidelines require the Spotify logo, unaltered, beside Spotify content; the
 * file must be Spotify's own download (developer.spotify.com/documentation/design), served from
 * `public/brand/`. Null until it is there: the name is then shown as text, which the guidelines do not
 * accept, so a null here is a known gap, not a choice.
 */
export const SPOTIFY_LOGO_URL: string | null = null;
