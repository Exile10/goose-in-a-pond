import { test } from 'node:test';
import assert from 'node:assert/strict';

import { chooseService, NO_MUSIC_INSTRUCTIONS } from './select.js';

test('a Mac gets Apple Music', () => {
  assert.equal(chooseService('darwin').service, 'apple');
});

test('anything else gets no music service at all, and says why', () => {
  for (const platform of ['linux', 'win32', 'freebsd']) {
    const choice = chooseService(platform);
    assert.equal(choice.service, null, platform);
    assert.match(choice.reason, /Apple Music needs macOS/);
    assert.match(choice.reason, /Spotify cannot be controlled by the assistant/);
  }
});

test('Spotify is never a choice, whatever is signed in or asked for', () => {
  // The choice no longer reads the environment: a Spotify sign-in or MUSIC_SERVICE=spotify cannot
  // bring Spotify to the assistant, because Spotify's rules forbid voice and AI control of it.
  assert.equal(chooseService.length, 1);
  for (const platform of ['darwin', 'linux']) assert.notEqual(chooseService(platform).service, 'spotify');
});

test('the words the assistant gets with no tools explain the rule and where Spotify is played', () => {
  assert.match(NO_MUSIC_INSTRUCTIONS, /never controlled by the assistant/);
  assert.match(NO_MUSIC_INSTRUCTIONS, /Spotify player page/);
  assert.match(NO_MUSIC_INSTRUCTIONS, /music controls/);
});
