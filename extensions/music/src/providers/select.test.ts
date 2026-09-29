import { test } from 'node:test';
import assert from 'node:assert/strict';

import { chooseService } from './select.js';

test('a Spotify sign-in wins when nothing is asked for', () => {
  assert.equal(chooseService({ SPOTIFY_ACCESS_TOKEN: 'tok' }, 'darwin').service, 'spotify');
});

test('a Mac with no Spotify sign-in gets Apple Music', () => {
  assert.equal(chooseService({}, 'darwin').service, 'apple');
});

test('a machine that is not a Mac never gets Apple Music', () => {
  assert.equal(chooseService({}, 'linux').service, 'spotify');
  assert.equal(chooseService({ MUSIC_SERVICE: 'apple' }, 'linux').service, 'spotify');
});

test('an explicit choice beats the sign-in', () => {
  assert.equal(chooseService({ MUSIC_SERVICE: 'apple', SPOTIFY_ACCESS_TOKEN: 'tok' }, 'darwin').service, 'apple');
  assert.equal(chooseService({ MUSIC_SERVICE: 'spotify' }, 'darwin').service, 'spotify');
});

test('the name may be spelled the way a person would', () => {
  for (const spelling of ['Apple Music', 'apple-music', 'APPLE_MUSIC', ' apple ']) {
    assert.equal(chooseService({ MUSIC_SERVICE: spelling }, 'darwin').service, 'apple', spelling);
  }
});

test('blank and auto both mean choose for me', () => {
  assert.equal(chooseService({ MUSIC_SERVICE: '' }, 'darwin').service, 'apple');
  assert.equal(chooseService({ MUSIC_SERVICE: 'auto' }, 'darwin').service, 'apple');
});

test('a value that is no service is ignored, and the log says so', () => {
  const choice = chooseService({ MUSIC_SERVICE: 'tidal', SPOTIFY_ACCESS_TOKEN: 'tok' }, 'darwin');
  assert.equal(choice.service, 'spotify');
  assert.match(choice.reason, /tidal/);
});
