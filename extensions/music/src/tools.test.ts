import { test } from 'node:test';
import assert from 'node:assert/strict';

import { AppleMusicProvider } from './providers/apple-music.js';
import { SpotifyProvider } from './providers/spotify.js';
import { buildTools } from './tools.js';
import { redactSecrets } from './log.js';
import type { MusicProvider } from './providers/types.js';

/** Tool shape only; the Spotify text is compared against the shipped list by hand in the PR. */

function apple(): MusicProvider {
  return new AppleMusicProvider({ app: {} as never, catalog: {} as never });
}

const names = (p: MusicProvider) => buildTools(p).map(t => t.name);
const props = (p: MusicProvider, tool: string) =>
  Object.keys((buildTools(p).find(t => t.name === tool)!.inputSchema as { properties: object }).properties);

test('Spotify keeps all six tools', () => {
  assert.deepEqual(names(new SpotifyProvider()), ['play', 'playlists', 'library', 'devices', 'status', 'control']);
});

test('Apple Music offers the same six, the devices being AirPlay speakers', () => {
  assert.deepEqual(names(apple()), ['play', 'playlists', 'library', 'devices', 'status', 'control']);
  const devices = buildTools(apple()).find(t => t.name === 'devices')!;
  assert.match(devices.description, /AirPlay/);
});

test('Apple Music does not advertise a queue it does not have', () => {
  assert.ok(props(new SpotifyProvider(), 'play').includes('when'));
  assert.ok(!props(apple(), 'play').includes('when'));
});

test('Apple Music does not advertise a time range it cannot honour', () => {
  assert.ok(props(new SpotifyProvider(), 'library').includes('time_range'));
  assert.ok(!props(apple(), 'library').includes('time_range'));
});

test('each service names itself and not the other', () => {
  const text = (p: MusicProvider) => JSON.stringify(buildTools(p));
  assert.ok(!/apple/i.test(text(new SpotifyProvider())));
  assert.ok(!/spotify/i.test(text(apple())));
});

test('the Apple play description does not promise playback it cannot guarantee', () => {
  const play = buildTools(apple()).find(t => t.name === 'play')!;
  assert.match(play.description, /rather than assuming playback started/);
  assert.ok(!/keeps playing/i.test(play.description));
});

// ── log redaction for the new credentials ────────────────────────────────────

test('a developer token is redacted from log text', () => {
  const jwt = 'eyJhbGciOiJFUzI1NiIsImtpZCI6IkFCQ0QifQ.eyJpc3MiOiJURUFNIiwiaWF0IjoxfQ.MEUCIQDabcdefgh';
  assert.ok(!redactSecrets(`failed with ${jwt}`).includes('eyJ'));
});

test('a Music User Token header value is redacted', () => {
  const out = redactSecrets('{"Music-User-Token": "AbCdEf123456+/=xyz"}');
  assert.ok(!out.includes('AbCdEf123456'));
});
