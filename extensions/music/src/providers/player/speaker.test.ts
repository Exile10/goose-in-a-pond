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
