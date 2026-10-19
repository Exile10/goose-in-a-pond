//! The mic and the speaker route keep the device they found by name, because finding it again on
//! ALSA lists every node. A kept device must hold only what its open stream uses: the other
//! direction left open locks PulseAudio and every other program out of the card.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::time::Duration;

use pond_audio::{CpalCapture, MicState};

/// An ALSA `null` PCM, which opens `/dev/null` for playback and `/dev/full` for capture, so each
/// direction this process holds can be counted.
const PCM: &str = "pondtest";

fn held() -> (isize, isize) {
    let (mut playback, mut capture) = (0, 0);
    for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        match std::fs::read_link(entry.path()) {
            Ok(p) if p == Path::new("/dev/null") => playback += 1,
            Ok(p) if p == Path::new("/dev/full") => capture += 1,
            _ => {}
        }
    }
    (playback, capture)
}

#[test]
fn a_kept_device_holds_only_the_direction_in_use() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".asoundrc"),
        format!("pcm.{PCM} {{\n  type null\n  hint {{ show on description \"pond test\" }}\n}}\n"),
    )
    .unwrap();
    // ALSA reads ~/.asoundrc once, on first use, and nothing else in this binary touches it.
    std::env::set_var("HOME", home.path());
    let base = held();
    let open = || {
        let now = held();
        (now.0 - base.0, now.1 - base.1)
    };
    let wait = Duration::from_secs(5);

    let (mic, owner) =
        pond_audio::spawn(Box::new(CpalCapture::with_device(PCM)), 16_000, 1_000, true);
    // Twice, so the second open is on the kept device.
    for _ in 0..2 {
        mic.open();
        assert!(mic.wait_for(MicState::is_open, wait), "{:?}", mic.state());
        let on = mic.run_exclusive(wait, |v| v.open_device.clone()).flatten();
        assert_eq!(on.as_deref(), Some(PCM), "fell back to the default");
        assert_eq!(open(), (0, 1), "the open mic holds the playback side too");
        mic.close();
        assert!(mic.wait_for(|s| *s == MicState::Closed, wait));
        assert_eq!(open(), (0, 0), "the closed mic still holds the card");
    }
    mic.shutdown();
    owner.join().unwrap();

    for _ in 0..2 {
        let (stream, _handle, opened) = pond_audio_out::route::open_output(Some(PCM)).unwrap();
        assert_eq!(opened, PCM);
        assert_eq!(
            open(),
            (1, 0),
            "the open speaker holds the capture side too"
        );
        drop(stream);
        assert_eq!(open(), (0, 0), "the closed speaker still holds the card");
    }
}
