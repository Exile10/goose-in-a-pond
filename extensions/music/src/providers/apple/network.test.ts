import { test } from 'node:test';
import assert from 'node:assert/strict';

import { defaultStorefront, ItunesSearch, musicAppUrl, songIdFromLink } from './catalog.js';
import { DeveloperTokens, NotConfigured } from './developer-token.js';
import { EgressGate, EgressRefused, type Fetch } from './egress.js';
import { AppleMusicApi } from './rest.js';

/** Every network call goes through a fake `fetch`, so nothing here leaves the machine. */

interface Sent { url: string; method: string; headers: Record<string, string>; body?: string }

function fakeFetch(handler: (sent: Sent) => Response | Promise<Response>): { fetch: Fetch; sent: Sent[] } {
  const sent: Sent[] = [];
  const fetchFn = (async (url: string, init: RequestInit = {}) => {
    const entry: Sent = {
      url: String(url),
      method: init.method ?? 'GET',
      headers: (init.headers ?? {}) as Record<string, string>,
      body: typeof init.body === 'string' ? init.body : undefined,
    };
    sent.push(entry);
    return handler(entry);
  }) as unknown as Fetch;
  return { fetch: fetchFn, sent };
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

const HOST = 'http://127.0.0.1:4000';

// ── EgressGate ───────────────────────────────────────────────────────────────

test('an allowed call passes and is reported to the host with its extension name', async () => {
  const { fetch, sent } = fakeFetch(() => json({ allowed: true }));
  await new EgressGate(fetch, HOST, 'internal').allow('https://itunes.apple.com/search?term=x');

  assert.equal(sent[0].url, `${HOST}/api/v1/extension/egress`);
  assert.equal(sent[0].headers.Authorization, 'Bearer internal');
  assert.deepEqual(JSON.parse(sent[0].body!), {
    url: 'https://itunes.apple.com/search?term=x', method: 'GET', extension: 'music',
  });
});

test('a refusal carries the host\'s own explanation', async () => {
  const { fetch } = fakeFetch(() => json({ allowed: false, reason: 'Offline mode refuses itunes.apple.com.' }));
  await assert.rejects(
    new EgressGate(fetch, HOST, 'internal').allow('https://itunes.apple.com/search'),
    (err: Error) => err instanceof EgressRefused && /Offline mode/.test(err.message),
  );
});

test('an unreachable host does not silence the extension', async () => {
  const fetch = (async () => { throw new TypeError('fetch failed'); }) as unknown as Fetch;
  await new EgressGate(fetch, HOST, 'internal').allow('https://itunes.apple.com/search');
});

test('a host that does not know the route does not silence the extension either', async () => {
  const { fetch } = fakeFetch(() => json({ error: 'not found' }, 404));
  await new EgressGate(fetch, HOST, 'internal').allow('https://itunes.apple.com/search');
});

test('with no internal token there is no host to ask', async () => {
  const { fetch, sent } = fakeFetch(() => json({ allowed: false }));
  await new EgressGate(fetch, HOST, '').allow('https://itunes.apple.com/search');
  assert.equal(sent.length, 0);
});

// ── DeveloperTokens ──────────────────────────────────────────────────────────

test('a developer token is fetched once and reused', async () => {
  const { fetch, sent } = fakeFetch(() => json({ token: 'jwt-1', expires_at: 2_000_000 }));
  const tokens = new DeveloperTokens(fetch, HOST, 'internal', () => 1_000_000_000);

  assert.equal(await tokens.get(), 'jwt-1');
  assert.equal(await tokens.get(), 'jwt-1');
  assert.equal(sent.length, 1);
});

test('a token close to expiry is replaced before it is used', async () => {
  let clock = 1_000_000_000;
  let n = 0;
  const { fetch, sent } = fakeFetch(() => json({ token: `jwt-${++n}`, expires_at: clock / 1000 + 600 }));
  const tokens = new DeveloperTokens(fetch, HOST, 'internal', () => clock);

  assert.equal(await tokens.get(), 'jwt-1');
  clock += 6 * 60_000;
  assert.equal(await tokens.get(), 'jwt-2');
  assert.equal(sent.length, 2);
});

test('no Apple Music key is NotConfigured, carrying the host\'s instructions', async () => {
  const { fetch } = fakeFetch(() => json({ error: 'Add your Apple Music Team ID first.' }, 400));
  await assert.rejects(
    new DeveloperTokens(fetch, HOST, 'internal').get(),
    (err: Error) => err instanceof NotConfigured && /Team ID/.test(err.message),
  );
});

test('a token with no expiry is refused', async () => {
  const { fetch } = fakeFetch(() => json({ token: 'jwt' }));
  await assert.rejects(new DeveloperTokens(fetch, HOST, 'internal').get(), /no expiry/);
});

// ── catalog helpers ──────────────────────────────────────────────────────────

test('a song id comes out of every link shape Apple uses', () => {
  assert.equal(songIdFromLink('https://music.apple.com/ke/album/nairobi/999?i=1001&uo=4'), '1001');
  assert.equal(songIdFromLink('https://music.apple.com/us/song/nairobi/1001'), '1001');
  assert.equal(songIdFromLink('music://music.apple.com/ke/album/nairobi/999?i=1001'), '1001');
  assert.equal(songIdFromLink('https://example.com/?i=5'), null);
  assert.equal(songIdFromLink('https://music.apple.com/ke/album/nairobi/999'), null);
});

test('a page link opens in the Music app, not the browser', () => {
  assert.equal(musicAppUrl('https://music.apple.com/ke/album/x/1?i=2'), 'music://music.apple.com/ke/album/x/1?i=2');
});

test('a URL that is not an Apple page is never handed to the Music app', () => {
  assert.equal(musicAppUrl('https://evil.example/x'), null);
  assert.equal(musicAppUrl('https://music.apple.com.evil.example/x'), null);
  assert.equal(musicAppUrl('file:///etc/passwd'), null);
  assert.equal(musicAppUrl('javascript:alert(1)'), null);
});

test('the store comes from the setting, else the machine\'s region, else US', () => {
  assert.equal(defaultStorefront('KE', 'en-US'), 'ke');
  assert.equal(defaultStorefront(undefined, 'en-KE'), 'ke');
  assert.equal(defaultStorefront('', 'en_GB'), 'gb');
  assert.equal(defaultStorefront(undefined, 'en'), 'us');
  assert.equal(defaultStorefront('nonsense', 'fr-FR'), 'fr');
});

// ── ItunesSearch ─────────────────────────────────────────────────────────────

const allowAll = new EgressGate((async () => json({ allowed: true })) as unknown as Fetch, HOST, 'internal');

test('a search asks for songs in the right store, with "by" folded into the term', async () => {
  const { fetch, sent } = fakeFetch(() => json({ results: [] }));
  await new ItunesSearch(fetch, allowAll, 'ke').searchSongs("Marvin's Room by Drake", 5);

  const url = new URL(sent[0].url);
  assert.equal(url.hostname, 'itunes.apple.com');
  assert.equal(url.searchParams.get('term'), "Marvin's Room Drake");
  assert.equal(url.searchParams.get('entity'), 'song');
  assert.equal(url.searchParams.get('country'), 'ke');
});

test('only songs come back, whatever else the index returns', async () => {
  const { fetch } = fakeFetch(() => json({
    results: [
      { kind: 'song', trackId: 1, trackName: 'Nairobi', artistName: 'Bensoul', collectionName: 'Q', trackTimeMillis: 1000, trackViewUrl: 'u' },
      { kind: 'music-video', trackId: 2, trackName: 'Nairobi (Video)' },
      { kind: 'song', trackName: 'no id' },
    ],
  }));
  const songs = await new ItunesSearch(fetch, allowAll, 'us').searchSongs('Nairobi', 5);
  assert.deepEqual(songs.map(s => s.id), ['1']);
});

test('a refused egress stops the search before anything is sent', async () => {
  const refusing = new EgressGate((async () => json({ allowed: false, reason: 'no' })) as unknown as Fetch, HOST, 'internal');
  const { fetch, sent } = fakeFetch(() => json({ results: [] }));

  await assert.rejects(new ItunesSearch(fetch, refusing, 'us').searchSongs('x', 5), EgressRefused);
  assert.equal(sent.length, 0);
});

test('rate limiting is explained', async () => {
  const { fetch } = fakeFetch(() => json({}, 429));
  await assert.rejects(new ItunesSearch(fetch, allowAll, 'us').searchSongs('x', 5), /rate limiting/);
});

// ── AppleMusicApi ────────────────────────────────────────────────────────────

function apiFetch(extra?: (s: Sent) => Response | undefined) {
  let tokenN = 0;
  return fakeFetch(s => {
    if (s.url.endsWith('/musickit/developer-token')) return json({ token: `dev-${++tokenN}`, expires_at: 9_999_999_999 });
    const custom = extra?.(s);
    if (custom) return custom;
    if (s.url.endsWith('/v1/me/storefront')) return json({ data: [{ id: 'ke' }] });
    if (s.url.includes('/search')) {
      return json({ results: { songs: { data: [{ id: '1001', attributes: { name: 'Nairobi', artistName: 'Bensoul', albumName: 'Q', durationInMillis: 5, url: 'u' } }] } } });
    }
    return json({}, 202);
  });
}

const restWith = (fetch: Fetch, userToken: string | null = 'MUT') =>
  new AppleMusicApi(fetch, allowAll, new DeveloperTokens(fetch, HOST, 'internal'), userToken ?? undefined);

test('without a Music User Token the API is simply unavailable', async () => {
  const { fetch, sent } = apiFetch();
  assert.equal(await restWith(fetch, null).available(), false);
  assert.equal(sent.length, 0);
});

test('without a signing key the API is unavailable, not an error', async () => {
  const { fetch } = fakeFetch(() => json({ error: 'no key' }, 400));
  assert.equal(await restWith(fetch).available(), false);
});

test('with both credentials the API is available', async () => {
  const { fetch } = apiFetch();
  assert.equal(await restWith(fetch).available(), true);
});

test('a catalog search uses the account\'s store and sends both tokens', async () => {
  const { fetch, sent } = apiFetch();
  const songs = await restWith(fetch).searchSongs('Nairobi', 5);

  assert.deepEqual(songs.map(s => s.id), ['1001']);
  const search = sent.find(s => s.url.includes('/search'))!;
  assert.match(search.url, /\/v1\/catalog\/ke\/search\?/);
  assert.equal(search.headers.Authorization, 'Bearer dev-1');
  assert.equal(search.headers['Music-User-Token'], 'MUT');
});

test('a 401 gets one retry with a fresh developer token', async () => {
  let searches = 0;
  const { fetch, sent } = apiFetch(s => (s.url.includes('/search') && ++searches === 1 ? json({}, 401) : undefined));

  const songs = await restWith(fetch).searchSongs('Nairobi', 5);

  assert.equal(songs.length, 1);
  const auths = sent.filter(s => s.url.includes('/search')).map(s => s.headers.Authorization);
  assert.deepEqual(auths, ['Bearer dev-1', 'Bearer dev-2']);
});

test('a 403 tells the user to sign in again', async () => {
  const { fetch } = apiFetch(s => (s.url.includes('/search') ? json({}, 403) : undefined));
  await assert.rejects(restWith(fetch).searchSongs('x', 5), /Sign in to Apple Music again/);
});

test('adding a song posts its id to the library', async () => {
  const { fetch, sent } = apiFetch();
  await restWith(fetch).addSongToLibrary('1001');

  const add = sent.find(s => s.method === 'POST' && s.url.includes('/v1/me/library'))!;
  assert.equal(new URL(add.url).searchParams.get('ids[songs]'), '1001');
});
