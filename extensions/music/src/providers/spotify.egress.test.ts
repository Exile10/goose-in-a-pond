import { test } from 'node:test';
import assert from 'node:assert/strict';

import { EgressRefused, type Fetch } from './apple/egress.js';
import type { Speaker } from './player/speaker.js';
import { SpotifyApiError, SpotifyProvider, type EgressCheck } from './spotify.js';

/**
 * The provider's network behaviour against a fake that answers as Spotify's documentation says:
 * no active device is a 404 with the reason NO_ACTIVE_DEVICE. Nothing here talks to Spotify.
 */

type Reply = () => Response;
const noContent: Reply = () => new Response(null, { status: 204 });
const noDevice: Reply = () =>
  new Response(
    JSON.stringify({
      error: { status: 404, message: 'Player command failed: No active device found', reason: 'NO_ACTIVE_DEVICE' },
    }),
    { status: 404 },
  );

interface Call {
  method: string;
  url: string;
  body: unknown;
}

/** Answers each `METHOD url-prefix` with the queued replies in order, and records every call. */
function fakeNetwork(routes: Record<string, Reply[]>) {
  const calls: Call[] = [];
  const fetchFn = (async (url: string | URL | Request, init?: RequestInit) => {
    const u = String(url);
    const method = init?.method ?? 'GET';
    calls.push({ method, url: u, body: init?.body ? JSON.parse(String(init.body)) : undefined });
    const key = Object.keys(routes).find((k) => `${method} ${u}`.startsWith(k));
    const reply = key ? routes[key]!.shift() : undefined;
    if (!reply) throw new Error(`unexpected call: ${method} ${u}`);
    return reply();
  }) as unknown as Fetch;
  return { fetchFn, calls };
}

function gate(refuse?: string) {
  const asked: Array<[string, string | undefined]> = [];
  const egress: EgressCheck = {
    allow: async (url, method) => {
      asked.push([url, method]);
      if (refuse) throw new EgressRefused(refuse);
    },
  };
  return { egress, asked };
}

function speaker(id: string | null) {
  let asks = 0;
  const s: Speaker = {
    deviceId: async () => {
      asks += 1;
      return id;
    },
    // These tests are about the gate and the fallback, not the check afterwards: a player that cannot
    // be seen is not waited on.
    status: async () => null,
    unavailableBecause: async () => null,
  };
  return { speaker: s, asks: () => asks };
}

const PLAY = 'PUT https://api.spotify.com/v1/me/player/play';

// ── the gate ─────────────────────────────────────────────────────────────────

test('every call to Spotify asks the host first, with its method', async () => {
  const net = fakeNetwork({ [PLAY]: [noContent], 'PUT https://api.spotify.com/v1/me/player/pause': [noContent] });
  const g = gate();
  const provider = new SpotifyProvider({ fetch: net.fetchFn, egress: g.egress, token: 't' });

  await provider.play('spotify:album:1DFixLWuPkv3KT3TnV35m3');
  await provider.pause();

  assert.deepEqual(g.asked, [
    ['https://api.spotify.com/v1/me/player/play', 'PUT'],
    ['https://api.spotify.com/v1/me/player/pause', 'PUT'],
  ]);
});

test('a call the network policy refuses is never sent, and says why in the policy\'s words', async () => {
  const net = fakeNetwork({});
  const g = gate('Network mode is offline, so Spotify was not contacted.');
  const provider = new SpotifyProvider({ fetch: net.fetchFn, egress: g.egress, token: 't' });

  await assert.rejects(provider.pause(), /Network mode is offline/);
  assert.equal(net.calls.length, 0, 'nothing may leave when the policy says no');
});

test('the retry after a refreshed token asks the host again, and the refresh itself is not a Spotify call', async () => {
  const net = fakeNetwork({
    'PUT https://api.spotify.com/v1/me/player/pause': [() => new Response('{}', { status: 401 }), noContent],
    'POST http://127.0.0.1:4000/api/v1/oauth/refresh': [
      () => new Response(JSON.stringify({ refreshed: true, access_token: 'fresh' }), { status: 200 }),
    ],
  });
  const g = gate();
  const provider = new SpotifyProvider({ fetch: net.fetchFn, egress: g.egress, token: 'stale' });

  await provider.pause();

  assert.equal(g.asked.length, 2, 'the first attempt and the retry are each a call to Spotify');
  assert.ok(g.asked.every(([url]) => url.startsWith('https://api.spotify.com/')), 'the refresh goes to the host, not through the gate');
});

test('without a gate the provider behaves as it always has', async () => {
  const net = fakeNetwork({ 'PUT https://api.spotify.com/v1/me/player/pause': [noContent] });
  const provider = new SpotifyProvider({ fetch: net.fetchFn, token: 't' });
  assert.equal(await provider.pause(), 'Playback paused');
});

// ── the in-app player as somewhere to play ───────────────────────────────────

test('an active device is used as before, and the in-app player is not even asked about', async () => {
  const net = fakeNetwork({ [PLAY]: [noContent] });
  const s = speaker('in-app-device');
  const provider = new SpotifyProvider({ fetch: net.fetchFn, speaker: s.speaker, token: 't' });

  await provider.play('spotify:album:1DFixLWuPkv3KT3TnV35m3');

  assert.equal(s.asks(), 0);
  assert.equal(net.calls.length, 1);
  assert.equal(net.calls[0]!.url, 'https://api.spotify.com/v1/me/player/play');
});

test('with no active device, playback moves to the in-app player and keeps what was asked for', async () => {
  const net = fakeNetwork({ [PLAY]: [noDevice, noContent] });
  const s = speaker('in app/device');
  const provider = new SpotifyProvider({ fetch: net.fetchFn, speaker: s.speaker, token: 't' });

  const said = await provider.play('spotify:playlist:37i9dQZF1DXcBWIGoYBM5M');

  assert.equal(said, 'Playing spotify:playlist:37i9dQZF1DXcBWIGoYBM5M');
  assert.equal(net.calls.length, 2);
  assert.equal(net.calls[1]!.url, 'https://api.spotify.com/v1/me/player/play?device_id=in%20app%2Fdevice', 'the id is encoded');
  assert.deepEqual(net.calls[1]!.body, net.calls[0]!.body);
  assert.deepEqual(net.calls[1]!.body, { context_uri: 'spotify:playlist:37i9dQZF1DXcBWIGoYBM5M' });
});

test('with no active device and no in-app player, the error is Spotify\'s, as before', async () => {
  const none = fakeNetwork({ [PLAY]: [noDevice] });
  const p1 = new SpotifyProvider({ fetch: none.fetchFn, token: 't' });
  await assert.rejects(p1.play('spotify:album:1DFixLWuPkv3KT3TnV35m3'), (e: unknown) => {
    assert.ok(e instanceof SpotifyApiError);
    assert.equal(e.status, 404);
    assert.match(e.message, /NO_ACTIVE_DEVICE/);
    return true;
  });

  const notReady = fakeNetwork({ [PLAY]: [noDevice] });
  const s = speaker(null);
  const p2 = new SpotifyProvider({ fetch: notReady.fetchFn, speaker: s.speaker, token: 't' });
  await assert.rejects(p2.play('spotify:album:1DFixLWuPkv3KT3TnV35m3'), /NO_ACTIVE_DEVICE/);
  assert.equal(s.asks(), 1, 'asked once, found nothing, and gave up');
  assert.equal(notReady.calls.length, 1, 'no second attempt without a device');
});

test('only "no active device" moves playback: other refusals are passed on untouched', async () => {
  for (const [status, body] of [
    [404, JSON.stringify({ error: { status: 404, message: 'Device not found', reason: 'DEVICE_NOT_FOUND' } })],
    [403, JSON.stringify({ error: { status: 403, message: 'Player command failed: Premium required', reason: 'PREMIUM_REQUIRED' } })],
    [500, 'boom'],
  ] as const) {
    const net = fakeNetwork({ [PLAY]: [() => new Response(body, { status })] });
    const s = speaker('in-app-device');
    const provider = new SpotifyProvider({ fetch: net.fetchFn, speaker: s.speaker, token: 't' });
    await assert.rejects(provider.play('spotify:album:x'), new RegExp(`Spotify API ${status}`));
    assert.equal(s.asks(), 0, `a ${status} must not go looking for another device`);
    assert.equal(net.calls.length, 1);
  }
});

test('resuming with nothing named falls back the same way and sends no body', async () => {
  const net = fakeNetwork({ [PLAY]: [noDevice, noContent] });
  const provider = new SpotifyProvider({ fetch: net.fetchFn, speaker: speaker('d').speaker, token: 't' });
  assert.equal(await provider.play(), 'Resumed playback');
  assert.equal(net.calls[1]!.body, undefined);
});

test('the fallback is gated like any other call to Spotify', async () => {
  const net = fakeNetwork({ [PLAY]: [noDevice, noContent] });
  const g = gate();
  const provider = new SpotifyProvider({ fetch: net.fetchFn, egress: g.egress, speaker: speaker('d').speaker, token: 't' });
  await provider.play('spotify:album:x');
  assert.deepEqual(
    g.asked.map(([url]) => url),
    ['https://api.spotify.com/v1/me/player/play', 'https://api.spotify.com/v1/me/player/play?device_id=d'],
  );
});
