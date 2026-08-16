//! Live synthesis against real Kokoro weights.
//!
//! `#[ignore]` by default, like the rest of GIAP's hardware-dependent tests —
//! it needs the model on disk and a working ONNX Runtime. Point it at a model
//! directory and run:
//!
//! ```bash
//! KOKORO_DIR=~/Documents/Jarida/kokoro-lab/models \
//!   cargo test -p pond-adapters-kokoro -- --ignored --nocapture
//! ```
//!
//! The directory must contain `model_quantized.onnx`, `tokenizer.json`, and
//! `voices/af_heart.bin`.
//!
//! This doubles as the Stage 0 bench: it prints the real-time factor for
//! native `ort`, which is the number the browser lab explicitly cannot give.

use pond_adapters_kokoro::{engine::duration_secs, tokenizer, Engine, StyleTable, Vocab};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("KOKORO_DIR").ok()?);
    dir.join("tokenizer.json").exists().then_some(dir)
}

/// Intra-op threads for the session under test.
///
/// Defaults to what `pond-server` will actually use, for the same reason
/// [`model_file`] does: a bench that measures a configuration the pond never
/// runs answers the wrong question. This was pinned at a literal 2 and kept
/// printing 1.35 on a Jetson after the server had moved to 4 — the number was
/// real and described nothing shipping.
///
/// `KOKORO_THREADS` overrides it, so the choice can still be swept.
fn intra_threads() -> Option<usize> {
    match std::env::var("KOKORO_THREADS") {
        Ok(s) => s.parse().ok(),
        Err(_) => Some(pond_adapters_kokoro::default_intra_threads()),
    }
}

/// Weights file to load from `KOKORO_DIR`.
///
/// Defaults to the tier this host would actually start on, so the RTF printed
/// below is the number the household gets rather than one for a tier the pond
/// would never pick here. `KOKORO_MODEL` overrides it — file size does not
/// predict speed (q4f16 is larger than q8 and far faster on aarch64), so
/// comparing tiers has to be possible on one board.
fn model_file() -> String {
    std::env::var("KOKORO_MODEL").unwrap_or_else(|_| {
        pond_adapters_kokoro::model_filename(pond_adapters_kokoro::host_default_quality())
            .to_owned()
    })
}

/// Any installed voice that is not the default.
///
/// Naming a second voice outright is a trap: `voices_to_fetch` guarantees only
/// `DEFAULT_VOICE`, and which others exist depends on what the household has
/// picked. This test used to hardcode `am_michael`, which nothing fetches — so
/// it failed on every machine that had not chosen that exact voice by hand.
fn second_voice(voices: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(voices)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(".bin").map(str::to_owned)
        })
        .filter(|n| n != pond_adapters_kokoro::DEFAULT_VOICE)
        .collect();
    names.sort();
    names.into_iter().next()
}

/// End-to-end: text in, non-trivial 24 kHz audio out.
#[test]
#[ignore = "needs Kokoro weights; set KOKORO_DIR"]
fn synthesizes_real_audio() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR to a directory holding tokenizer.json + model + voices/");
    };

    let vocab = Vocab::load(&dir.join("tokenizer.json")).expect("vocab");
    assert_eq!(vocab.len(), 115, "shipped Kokoro vocab is 115 symbols");

    let style = StyleTable::load(&dir.join("voices"), "af_heart").expect("voice");
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).expect("engine");

    let text = pond_adapters_kokoro::PREVIEW_SENTENCE;
    let (chunks, dropped) = tokenizer::chunk(text, &vocab).expect("chunk");
    assert!(!chunks.is_empty(), "preview sentence produced no chunks");
    assert_eq!(
        dropped, 0,
        "preview sentence lost phonemes to the vocab filter"
    );

    let t0 = Instant::now();
    let mut samples = Vec::new();
    for c in &chunks {
        samples.extend(
            engine
                .synthesize(c, style.style_for(c.tokens.len()), 1.0)
                .expect("synthesis"),
        );
    }
    let elapsed = t0.elapsed().as_secs_f32();
    let audio = duration_secs(&samples);

    assert!(
        audio > 1.0,
        "expected more than a second of audio, got {audio}s"
    );
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "output is silence (peak {peak})");
    assert!(peak <= 1.5, "output is wildly out of range (peak {peak})");

    println!(
        "kokoro {}: {audio:.2}s audio in {elapsed:.2}s  RTF={:.3}  chunks={}  peak={peak:.3}",
        model_file(),
        elapsed / audio,
        chunks.len()
    );
}

/// Pace has to change the duration in the obvious direction, or the settings
/// slider is decorative.
#[test]
#[ignore = "needs Kokoro weights; set KOKORO_DIR"]
fn pace_changes_duration() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR");
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).unwrap();
    let style = StyleTable::load(&dir.join("voices"), "af_heart").unwrap();
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).unwrap();

    let (chunks, _) = tokenizer::chunk("The pond is awake and listening.", &vocab).unwrap();
    let c = &chunks[0];
    let row = style.style_for(c.tokens.len());

    let slow = engine.synthesize(c, row, 0.75).unwrap().len();
    let normal = engine.synthesize(c, row, 1.0).unwrap().len();
    let fast = engine.synthesize(c, row, 1.5).unwrap().len();

    println!("pace samples: 0.75x={slow} 1.0x={normal} 1.5x={fast}");
    assert!(slow > normal, "0.75x should be longer than 1.0x");
    assert!(fast < normal, "1.5x should be shorter than 1.0x");
}

/// Two voices must actually differ — a style table that silently fails to
/// apply would still produce valid-sounding audio in one voice.
#[test]
#[ignore = "needs Kokoro weights + a second voice; set KOKORO_DIR"]
fn different_voices_produce_different_audio() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR");
    };
    let voices = dir.join("voices");
    let Some(other) = second_voice(&voices) else {
        // One voice installed is a legitimate state — it is what a fresh pond
        // has. Nothing to compare, so there is nothing to assert.
        println!(
            "only {} installed; skipping",
            pond_adapters_kokoro::DEFAULT_VOICE
        );
        return;
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).unwrap();
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).unwrap();
    let (chunks, _) = tokenizer::chunk("Good morning.", &vocab).unwrap();
    let c = &chunks[0];

    let a = StyleTable::load(&voices, pond_adapters_kokoro::DEFAULT_VOICE).unwrap();
    let b = StyleTable::load(&voices, &other).unwrap();

    let wave_a = engine
        .synthesize(c, a.style_for(c.tokens.len()), 1.0)
        .unwrap();
    let wave_b = engine
        .synthesize(c, b.style_for(c.tokens.len()), 1.0)
        .unwrap();

    let n = wave_a.len().min(wave_b.len());
    assert!(n > 0);
    let diff: f32 = wave_a[..n]
        .iter()
        .zip(&wave_b[..n])
        .map(|(x, y)| (x - y).abs())
        .sum::<f32>()
        / n as f32;
    println!("mean abs difference af_heart vs {other}: {diff:.5}");
    assert!(
        diff > 1e-4,
        "af_heart and {other} produced near-identical audio"
    );
}

/// The phonemizer over the corpus that matters: what the pond actually says.
/// This is the R2 canary and needs no model, only espeak.
#[test]
#[ignore = "needs KOKORO_DIR for the real vocab"]
fn ordinary_text_loses_no_phonemes() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR");
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).unwrap();

    let corpus = [
        "The pond is awake. I have your calendar for tomorrow.",
        "Your meeting is at 3:45 PM on March 2nd, 2026.",
        "The total came to $1,247.83, up 12.5% from last quarter.",
        "Check the REST API over HTTP, then the SQLite DB.",
        "She sells thirty-three shiny thistles by the northern thoroughfare.",
        "Jerry asked about Nairobi, Kisumu, and the Jetson Orin Nano.",
        "It's 72 degrees and clear until about four in the afternoon.",
    ];

    let mut total_dropped = 0;
    for text in corpus {
        let (_chunks, dropped) = tokenizer::chunk(text, &vocab).unwrap();
        if dropped > 0 {
            println!("DROPPED {dropped} in: {text}");
        }
        total_dropped += dropped;
    }
    assert_eq!(
        total_dropped, 0,
        "espeak emitted phonemes outside Kokoro's vocab — words will sound wrong"
    );
}
