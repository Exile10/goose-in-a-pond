import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { Fetch } from './apple/egress.js';
import type { Speaker, SpeakerStatus } from './player/speaker.js';
import { SpotifyApiError, SpotifyProvider } from './spotify.js';

/**
 * Spotify answers a play request before anything has played. These tests are about what the provider
 * does next: it looks at the in-app player, and says what the player did. The player is scripted; the
 * network is a fake that answers as Spotify's documentation says. Nothing here talks to Spotify.
 */

const noContent = () => new Response(null, { status: 204 });
const noDevice = () =>
  new Response(
    JSON.stringify({ error: { status: 404, message: 'Player command failed: No active device found', reason: 'NO_ACTIVE_DEVICE' } }),
    { status: 404 },
  );
const deviceGone = () =>
  new Response(JSON.stringify({ error: { status: 404, message: 'Device not found', reason: 'DEVICE_NOT_FOUND' } }), { status: 404 });

function network(replies: Array<() => Response>) {
  const calls: string[] = [];
  const fetchFn = (async (url: string | URL | Request, init?: RequestInit) => {
    calls.push(`${init?.method ?? 'GET'} ${String(url)}`);
    const reply = replies.shift();
    if (!reply) throw new Error('unexpected call');
    return reply();
  }) as unknown as Fetch;
  return { fetchFn, calls };
}

const at = (status: string, position_ms = 0, message?: string): SpeakerStatus => ({
  status,
  position_ms,
  ...(message ? { message } : {}),
});

/** A speaker whose status() walks through `looks` (the first is the look before the play), then repeats the last. */
function scripted(opts: { device?: string | null; looks: Array<SpeakerStatus | null>; why?: string | null }) {
  const queue = [...opts.looks];
  const s: Speaker & { statusCalls: number; whyCalls: number } = {
    statusCalls: 0,
    whyCalls: 0,
    deviceId: async () => (opts.device === undefined ? 'in-app' : opts.device),
    status: async () => {
      s.statusCalls += 1;
      return queue.length > 1 ? queue.shift()! : queue[0]!;
    },
    unavailableBecause: async () => {
      s.whyCalls += 1;
      return opts.why ?? null;
    },
  };
  return s;
}

function provider(net: { fetchFn: Fetch }, speaker: Speaker) {
  const sleeps: number[] = [];
  const p = new SpotifyProvider({
    fetch: net.fetchFn,
    speaker,
    token: 't',
    sleep: async (ms) => void sleeps.push(ms),
  });
  return { p, sleeps };
}

const ALBUM = 'spotify:album:1DFixLWuPkv3KT3TnV35m3';

test('a song the in-app player then plays is reported as playing, after waiting for it to start', async () => {
  const s = scripted({ looks: [at('idle'), at('buffering'), at('playing', 200), at('playing', 1_400)] });
  const { p, sleeps } = provider(network([noContent]), s);

  assert.equal(await p.play(ALBUM), `Playing ${ALBUM}`);
  assert.equal(sleeps.length, 3, 'one wait before each look, and it stopped at the first that showed the song playing');
});

test('an error the in-app player reports is said, instead of "playing"', async () => {
  const s = scripted({ looks: [at('idle'), at('buffering'), at('error', 0, 'Spotify could not play this: Playback error.')] });
  const { p } = provider(network([noDevice, noContent]), s);

  await assert.rejects(p.play(ALBUM), (e: unknown) => {
    assert.ok(e instanceof Error);
    assert.match(e.message, /accepted the request, but the in-app player could not play it/);
    assert.match(e.message, /Playback error/);
    assert.match(e.message, /Nothing is playing/);
    return true;
  });
});

test('an error on the very first look counts, when it was not there before', async () => {
  const s = scripted({ looks: [at('idle'), at('error', 0, 'Spotify could not play this.')] });
  const { p, sleeps } = provider(network([noContent]), s);
  await assert.rejects(p.play(ALBUM), /could not play it/);
  assert.equal(sleeps.length, 1);
});

test('an error left from last time is not taken for this one while it may still be last time\'s', async () => {
  const stale = at('error', 0, 'Spotify could not play this: Playback error.');
  const s = scripted({ looks: [stale, stale, stale, at('playing', 900)] });
  const { p } = provider(network([noContent]), s);
  assert.equal(await p.play(ALBUM), `Playing ${ALBUM}`, 'the new song played, so the old error was old');
});

test('an error that is still there after it had time to be replaced is a failure', async () => {
  const stale = at('error', 0, 'Spotify could not play this: Playback error.');
  const s = scripted({ looks: [stale, stale] });
  const { p, sleeps } = provider(network([noContent]), s);
  await assert.rejects(p.play(ALBUM), /Playback error/);
  assert.equal(sleeps.length, 3, 'two looks were let go by, the third decided');
});

test('when something else is playing, the in-app player is not waited on', async () => {
  const s = scripted({ looks: [at('idle'), at('idle')] });
  const { p, sleeps } = provider(network([noContent]), s);
  assert.equal(await p.play(ALBUM), `Playing ${ALBUM}`);
  assert.equal(sleeps.length, 1, 'one look, and it saw nothing of ours');
});

test('a request that named the in-app device is waited on through an idle state that has not caught up', async () => {
  const s = scripted({ looks: [at('idle'), at('idle'), at('paused'), at('playing', 2_000)] });
  const { p } = provider(network([noDevice, noContent]), s);
  assert.equal(await p.play(ALBUM), `Playing ${ALBUM}`);
});

test('a player still loading when the looks run out is said so, not claimed to be playing', async () => {
  const s = scripted({ looks: [at('idle'), at('buffering')] });
  const { p, sleeps } = provider(network([noContent]), s);
  const said = await p.play(ALBUM);
  assert.match(said, /^Playing spotify:album:/);
  assert.match(said, /still loading it after 6 seconds/);
  assert.equal(sleeps.length, 10);
});

test('a player that cannot be seen is not second-guessed, and nothing is waited for', async () => {
  const blind = scripted({ looks: [null] });
  const a = provider(network([noContent]), blind);
  assert.equal(await a.p.play(ALBUM), `Playing ${ALBUM}`);
  assert.equal(a.sleeps.length, 0);

  const vanishes = scripted({ looks: [at('idle'), at('buffering'), null] });
  const b = provider(network([noContent]), vanishes);
  assert.equal(await b.p.play(ALBUM), `Playing ${ALBUM}`);
});

test('resuming is checked the same way', async () => {
  const s = scripted({ looks: [at('paused'), at('playing', 800)] });
  const { p } = provider(network([noContent]), s);
  assert.equal(await p.play(), 'Resumed playback');
});

test('no device, and a player that cannot be used: its reason is given, not Spotify\'s JSON', async () => {
  const reason = 'The Widevine module in this app (4.10.3050.0) is one Spotify refuses: it revokes licenses.';
  const s = scripted({ device: null, looks: [at('idle')], why: reason });
  const { p } = provider(network([noDevice]), s);

  await assert.rejects(p.play(ALBUM), (e: unknown) => {
    assert.ok(e instanceof Error);
    assert.ok(!(e instanceof SpotifyApiError), 'it is a message for a person, not the raw API error');
    assert.match(e.message, /4\.10\.3050\.0/);
    assert.match(e.message, /Open Spotify on a phone or computer/);
    assert.doesNotMatch(e.message, /NO_ACTIVE_DEVICE/);
    return true;
  });
});

test('no device and no reason from the player: the error is Spotify\'s, as before', async () => {
  const s = scripted({ device: null, looks: [at('idle')], why: null });
  const { p } = provider(network([noDevice]), s);
  await assert.rejects(p.play(ALBUM), (e: unknown) => {
    assert.ok(e instanceof SpotifyApiError);
    assert.match(e.message, /NO_ACTIVE_DEVICE/);
    return true;
  });
});

test('another kind of 404 does not go asking the player for a reason', async () => {
  const s = scripted({ device: null, looks: [at('idle')], why: 'some reason' });
  const { p } = provider(network([deviceGone]), s);
  await assert.rejects(p.play(ALBUM), /DEVICE_NOT_FOUND/);
  assert.equal(s.whyCalls, 0);
});
