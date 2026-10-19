# One ALSA handle per open stream

This directory vendors the published `cpal` 0.15.3 crate (upstream commit `ac6cbb2b`), selected
by the root Cargo patch for every user in the tree, rodio included. Left out of the published
crate: its registry marker, lockfile, upstream CI workflow, Dockerfile and `Cross.toml`. Added:
this note and a `[workspace]` table in `Cargo.toml`, which keeps the crate out of the
application workspace.

## The change

`src/host/alsa/mod.rs`, in `Device::build_stream_inner`: a stream closes the `Device`'s handle
for the other direction before taking its own. That is the only source change.

cpal 0.15's ALSA listing (`Devices::next` in `enumerate.rs`) opens every PCM for playback and
for capture, and keeps both handles in the `Device` it yields. A stream takes only its own
direction's handle. The Pond keeps the device it finds by name: the mic in
`crates/pond-audio/src/cpal_device.rs`, the speaker route in `crates/pond-audio-out/src/route.rs`.
Finding a device again would list about 60 PCMs and wake the `jack` plugin on every reopen of
the mic, the thinking tone and the wake ping. So the direction a kept device's stream did not
use stayed open for the life of the process. On the Jetson on 2026-10-19, an idle pond held the
USB card's playback and capture PCMs (`pcmC0D0p`, `pcmC0D0c`) through `sysdefault:CARD=Device`,
and PulseAudio could not resume the card's sink or source.

A direction closed this way opens again when it is next asked for. `supported_*_configs` and a
later stream in that direction open their own handle, as they always have for `default`, which
cpal yields with no handle open. Nothing else changes: CoreAudio and the other hosts are
untouched, and listing opens the same nodes as before.

`crates/pond-adapters-audio-devices/tests/kept_devices.rs` covers this on Linux. It runs the
mic owner and the speaker route against an ALSA `null` PCM and counts the handles held in each
direction while a stream is open and after it closes. Against the published crate it fails at its
first check, because the open mic also holds the playback side.

Drop this crate when rodio moves to a cpal whose ALSA listing no longer opens devices. Upstream's
ALSA host on `master` builds each `Device` from its PCM id without opening it.
