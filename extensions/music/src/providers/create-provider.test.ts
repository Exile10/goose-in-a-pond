import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { Fetch } from './apple/egress.js';
import { createProvider } from './index.js';

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const host = (reply: () => Response | Error): Fetch =>
  (async () => {
    const out = reply();
    if (out instanceof Error) throw out;
    return out;
  }) as unknown as Fetch;

test('Spotify is chosen when the user is signed in to it', async () => {
  const p = await createProvider({ SPOTIFY_ACCESS_TOKEN: 't' }, 'darwin', host(() => new Error('not asked')));
  assert.equal(p.id, 'spotify');
});

test('Apple Music plays through the in-app player once a key is set up', async () => {
  const p = await createProvider({ GIAP_INTERNAL_TOKEN: 'x' }, 'darwin', host(() => json({ apple: { attached: true, configured: true } })));
  assert.equal(p.id, 'apple');
  assert.deepEqual(p.capabilities, { devices: false, queue: true, timeRange: false });
});

test('with no key, Apple Music plays through the Music app', async () => {
  const p = await createProvider({ GIAP_INTERNAL_TOKEN: 'x' }, 'darwin', host(() => json({ apple: { attached: false, configured: false } })));
  assert.deepEqual(p.capabilities, { devices: true, queue: false, timeRange: false });
});

test('with the host unreachable, Apple Music plays through the Music app', async () => {
  const p = await createProvider({}, 'darwin', host(() => new TypeError('connection refused')));
  assert.deepEqual(p.capabilities, { devices: true, queue: false, timeRange: false });
});
