import { test } from 'node:test';
import assert from 'node:assert/strict';

import { PlayerFailure, PlayerUnavailable, type PlayerHost } from './host.js';
import { HostSpeaker } from './speaker.js';

function host(call: PlayerHost['call']): PlayerHost {
  return { call, status: async () => ({ attached: true, configured: true }) };
}

test('the in-app player\'s device id, when it is ready', async () => {
  const asked: Array<[string, unknown]> = [];
  const s = new HostSpeaker(
    host((async (op: string, args?: unknown) => {
      asked.push([op, args]);
      return { device_id: 'abc123', name: 'Goose In A Pond', ready: true };
    }) as PlayerHost['call']),
  );
  assert.equal(await s.deviceId(), 'abc123');
  assert.deepEqual(asked, [['device', {}]]);
});

test('a device that is not ready, or has no id, is not somewhere to play', async () => {
  for (const reply of [
    { device_id: 'abc', ready: false },
    { device_id: null, ready: true },
    { device_id: '', ready: true },
    { ready: true },
    {},
  ]) {
    const s = new HostSpeaker(host((async () => reply) as PlayerHost['call']));
    assert.equal(await s.deviceId(), null, JSON.stringify(reply));
  }
});

test('no window, a window that will not answer, or one that says no, is simply "no speaker"', async () => {
  for (const error of [
    new PlayerUnavailable('no_player', 'no player'),
    new PlayerUnavailable('timeout', 'timed out'),
    new PlayerUnavailable('refused', 'network mode'),
    new PlayerFailure('unsupported', 'Spotify has no device'),
    new PlayerFailure('not_ready', 'not ready'),
  ]) {
    const s = new HostSpeaker(host((async () => { throw error; }) as PlayerHost['call']));
    assert.equal(await s.deviceId(), null, error.message);
  }
});

test('a bug is not swallowed as "no speaker"', async () => {
  const s = new HostSpeaker(host((async () => { throw new TypeError('bad'); }) as PlayerHost['call']));
  await assert.rejects(s.deviceId(), TypeError);
});

test('what the player is doing, in its own terms', async () => {
  const asked: string[] = [];
  const s = new HostSpeaker(
    host((async (op: string) => {
      asked.push(op);
      return { status: 'playing', position_ms: 1234, need: 'none', ready: true };
    }) as PlayerHost['call']),
  );
  assert.deepEqual(await s.status(), { status: 'playing', position_ms: 1234 });
  assert.deepEqual(asked, ['state']);
});

test('the player\'s own message comes through, and an empty one does not', async () => {
  const withMessage = new HostSpeaker(
    host((async () => ({ status: 'error', position_ms: 0, message: 'Spotify could not play this: Playback error.' })) as PlayerHost['call']),
  );
  assert.deepEqual(await withMessage.status(), {
    status: 'error',
    position_ms: 0,
    message: 'Spotify could not play this: Playback error.',
  });
  const empty = new HostSpeaker(host((async () => ({ status: 'idle', message: '' })) as PlayerHost['call']));
  assert.deepEqual(await empty.status(), { status: 'idle', position_ms: 0 });
});

test('a reply with no status, or no window at all, is "cannot see the player"', async () => {
  for (const reply of [{}, { status: 7 }, { position_ms: 5 }]) {
    const s = new HostSpeaker(host((async () => reply) as PlayerHost['call']));
    assert.equal(await s.status(), null, JSON.stringify(reply));
  }
  const gone = new HostSpeaker(host((async () => { throw new PlayerUnavailable('no_player', 'no player'); }) as PlayerHost['call']));
  assert.equal(await gone.status(), null);
  const bug = new HostSpeaker(host((async () => { throw new TypeError('bad'); }) as PlayerHost['call']));
  await assert.rejects(bug.status(), TypeError);
});

test('why the player cannot be used, only when it cannot', async () => {
  const refused = new HostSpeaker(
    host((async () => ({ status: 'idle', need: 'setup', ready: false, message: 'The Widevine module is one Spotify refuses.' })) as PlayerHost['call']),
  );
  assert.equal(await refused.unavailableBecause(), 'The Widevine module is one Spotify refuses.');

  const usable = new HostSpeaker(host((async () => ({ status: 'idle', ready: true, message: 'left over' })) as PlayerHost['call']));
  assert.equal(await usable.unavailableBecause(), null, 'a usable player is not "unavailable"');

  const silent = new HostSpeaker(host((async () => ({ status: 'idle', ready: false })) as PlayerHost['call']));
  assert.equal(await silent.unavailableBecause(), null, 'no reason given, none invented');

  const gone = new HostSpeaker(host((async () => { throw new PlayerFailure('not_ready', 'no'); }) as PlayerHost['call']));
  assert.equal(await gone.unavailableBecause(), null);
});

