//! Audio primitives, in one place.
//!
//! Before this module the same handful of operations existed in up to four
//! copies each — WAV encoding in `pond-adapters-whisper`, `pond-adapters-piper`,
//! `pond-server/piper_http.rs` and `pond-desktop/src-tauri/audio.rs`; RMS in
//! three; resampling and the VAD state machine in two apiece. Fixing one copy
//! never fixed the others.
//!
//! Everything here is pure and `std`-only so it can be shared by the server,
//! both adapters, and eventually the Tauri shell (which is a separate cargo
//! workspace and cannot see `pond-core`).
//!
//! ## Sample scaling
//!
//! f32 audio is in `[-1.0, 1.0]` and converts to i16 by multiplying by
//! [`i16::MAX`] (32767) after clamping. The previous copies agreed on this —
//! whisper spelled it `32_767.0` and piper spelled it `i16::MAX as f32`, which
//! are the same number — so collapsing them changes no audio. That was checked
//! before the collapse, because a "pure move" that quietly altered scaling
//! would be an unusually hard bug to find.

// ── Level ────────────────────────────────────────────────────────────────────

/// Root-mean-square level of a block of normalised samples.
///
/// Zero for an empty block, so callers can treat "no audio yet" as silence
/// without a special case.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

// ── Rate conversion ──────────────────────────────────────────────────────────

/// Linearly resample to 16 kHz, the rate whisper expects.
///
/// Returns the input untouched when it is already 16 kHz — the common case on
/// a device whose default input config happens to match.
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16_000 || src_rate == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = 16_000.0_f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    (0..new_len)
        .map(|i| {
            let src = i as f64 / ratio;
            let lo = src.floor() as usize;
            let hi = (lo + 1).min(samples.len().saturating_sub(1));
            let frac = (src - src.floor()) as f32;
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

// ── Format conversion ────────────────────────────────────────────────────────

/// Convert normalised f32 samples to little-endian 16-bit PCM.
pub fn f32_to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Wrap little-endian 16-bit mono PCM in a canonical 44-byte WAV header.
pub fn encode_wav_pcm16(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    const CHANNELS: u16 = 1;
    const BITS: u16 = 16;
    let byte_rate = sample_rate * CHANNELS as u32 * BITS as u32 / 8;
    let block_align = CHANNELS * BITS / 8;
    let data_len = pcm.len() as u32;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

/// Encode normalised f32 samples as mono 16 kHz WAV.
pub fn encode_wav_mono_16k(samples: &[f32]) -> Vec<u8> {
    encode_wav_pcm16(&f32_to_pcm16(samples), 16_000)
}

// ── Decoding ─────────────────────────────────────────────────────────────────

/// Why a WAV could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE container at all.
    NotRiff,
    /// Structurally a WAV but a required chunk is missing.
    MissingChunk(&'static str),
    /// Truncated, or a chunk header claims more bytes than exist.
    Truncated,
    /// A shape this decoder does not handle, described for the caller.
    Unsupported(String),
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRiff => write!(f, "not a RIFF/WAVE file"),
            Self::MissingChunk(c) => write!(f, "WAV is missing its '{c}' chunk"),
            Self::Truncated => write!(f, "WAV data is truncated"),
            Self::Unsupported(d) => write!(f, "unsupported WAV format: {d}"),
        }
    }
}

impl std::error::Error for WavError {}

/// Decoded mono audio.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedWav {
    /// Normalised mono samples in `[-1.0, 1.0]`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

const FMT_PCM: u16 = 1;
const FMT_IEEE_FLOAT: u16 = 3;
const FMT_EXTENSIBLE: u16 = 0xFFFE;

/// Decode a WAV to normalised mono f32, walking the RIFF chunk list properly.
///
/// The implementation this replaces assumed the data chunk began at byte 44
/// and the payload was 16-bit mono, reading the sample rate from a fixed
/// offset. That holds only for WAVs GIAP itself produced. It also backs
/// `POST /api/v1/transcribe`, which real phone recorders hit — and those emit
/// `LIST`/`INFO` chunks before `data`, 18- and 40-byte `fmt ` chunks,
/// `WAVE_FORMAT_EXTENSIBLE`, stereo and 24-bit. Every one of those decoded to
/// noise rather than an error, so whisper hallucinated on garbage instead of
/// the caller learning anything was wrong.
///
/// Handles PCM 8/16/24/32-bit and IEEE float 32/64-bit, any channel count
/// (downmixed by averaging), and `EXTENSIBLE` via its SubFormat tag.
///
/// This parses untrusted input from the network. It never panics, never
/// indexes out of bounds, and never allocates based on an unvalidated length
/// field — every allocation is bounded by bytes actually present.
pub fn decode_wav(bytes: &[u8]) -> Result<DecodedWav, WavError> {
    // RIFF....WAVE
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotRiff);
    }

    let mut pos = 12usize;
    let mut fmt: Option<FmtChunk> = None;
    let mut data: Option<&[u8]> = None;

    // Chunk list: 4-byte id, 4-byte little-endian size, payload, then a pad
    // byte when the size is odd.
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body_start = pos + 8;
        // Clamp rather than trust: a truncated file (or a hostile size field)
        // must not push the slice past the buffer.
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => fmt = Some(parse_fmt(body)?),
            b"data" => data = Some(body),
            _ => {} // LIST, INFO, JUNK, fact, id3 … skipped by design
        }

        // Advance past the payload plus its pad byte. saturating_add keeps a
        // hostile size from wrapping the cursor backwards into an infinite loop.
        let advance = size + (size & 1);
        pos = body_start.saturating_add(advance);
        if advance == 0 && id != b"data" {
            // A zero-length unknown chunk is legal; a stream of them is not
            // progress. body_start already moved us forward by 8, so this only
            // guards against a pathological cursor.
            continue;
        }
    }

    let fmt = fmt.ok_or(WavError::MissingChunk("fmt "))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;
    if data.is_empty() {
        return Ok(DecodedWav {
            samples: Vec::new(),
            sample_rate: fmt.sample_rate,
        });
    }

    let samples = decode_samples(data, &fmt)?;
    Ok(DecodedWav {
        samples,
        sample_rate: fmt.sample_rate,
    })
}

struct FmtChunk {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn parse_fmt(body: &[u8]) -> Result<FmtChunk, WavError> {
    // 16 bytes is the PCM minimum; 18 adds cbSize, 40 is EXTENSIBLE.
    if body.len() < 16 {
        return Err(WavError::Truncated);
    }
    let mut format_tag = u16::from_le_bytes([body[0], body[1]]);
    let channels = u16::from_le_bytes([body[2], body[3]]);
    let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let bits_per_sample = u16::from_le_bytes([body[14], body[15]]);

    // EXTENSIBLE carries the real format in the first two bytes of its
    // SubFormat GUID, at offset 24 of the fmt body.
    if format_tag == FMT_EXTENSIBLE {
        if body.len() < 26 {
            return Err(WavError::Unsupported(
                "WAVE_FORMAT_EXTENSIBLE without a SubFormat GUID".into(),
            ));
        }
        format_tag = u16::from_le_bytes([body[24], body[25]]);
    }

    if channels == 0 {
        return Err(WavError::Unsupported("zero channels".into()));
    }
    if sample_rate == 0 {
        return Err(WavError::Unsupported("zero sample rate".into()));
    }

    Ok(FmtChunk {
        format_tag,
        channels,
        sample_rate,
        bits_per_sample,
    })
}

fn decode_samples(data: &[u8], fmt: &FmtChunk) -> Result<Vec<f32>, WavError> {
    let channels = fmt.channels as usize;

    // Per-sample decoders, all normalising into [-1.0, 1.0].
    let (bytes_per_sample, convert): (usize, fn(&[u8]) -> f32) =
        match (fmt.format_tag, fmt.bits_per_sample) {
            // 8-bit PCM is UNSIGNED with a 128 midpoint — the one PCM depth
            // that is not two's complement.
            (FMT_PCM, 8) => (1, |b| (b[0] as f32 - 128.0) / 128.0),
            (FMT_PCM, 16) => (2, |b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0),
            (FMT_PCM, 24) => (3, |b| {
                // Sign-extend 24-bit little-endian into i32.
                let v = ((b[2] as i32) << 24 | (b[1] as i32) << 16 | (b[0] as i32) << 8) >> 8;
                v as f32 / 8_388_608.0
            }),
            (FMT_PCM, 32) => (4, |b| {
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
            }),
            (FMT_IEEE_FLOAT, 32) => (4, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            (FMT_IEEE_FLOAT, 64) => (8, |b| {
                f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32
            }),
            (tag, bits) => {
                return Err(WavError::Unsupported(format!(
                    "format tag {tag} at {bits} bits per sample"
                )))
            }
        };

    let frame_bytes = bytes_per_sample * channels;
    if frame_bytes == 0 {
        return Err(WavError::Unsupported("zero-size frame".into()));
    }

    // Whole frames only — a trailing partial frame is dropped rather than
    // read past the end.
    let frames = data.len() / frame_bytes;
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * frame_bytes;
        // Downmix by averaging: a stereo phone recording becomes the mono
        // whisper wants, instead of the left channel plus interleaved noise.
        let mut acc = 0.0f32;
        for c in 0..channels {
            let s = base + c * bytes_per_sample;
            acc += convert(&data[s..s + bytes_per_sample]);
        }
        out.push(acc / channels as f32);
    }
    Ok(out)
}

// ── Silence ──────────────────────────────────────────────────────────────────

/// What one poll of the VAD implies for a speculative transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// Nothing to do.
    None,
    /// A silence run just began — worth transcribing what we have so far,
    /// overlapping inference with the rest of the silence wait.
    SpawnSpeculative,
    /// Speech resumed, so any in-flight speculative result is stale.
    DiscardSpeculative,
    /// Silence held long enough: end of utterance.
    Confirmed,
}

/// Debounced end-of-speech detector that also drives speculative inference.
///
/// Feed it one RMS reading per `poll_ms`. It reports the start of each silence
/// run exactly once (not on every poll), which is what makes the speculative
/// transcription fire once per pause rather than continuously.
#[derive(Debug, Clone)]
pub struct SpeculativeVad {
    silent_for_ms: u64,
    silence_ms: u64,
    poll_ms: u64,
}

impl SpeculativeVad {
    pub fn new(silence_ms: u64, poll_ms: u64) -> Self {
        Self {
            silent_for_ms: 0,
            silence_ms,
            poll_ms,
        }
    }

    pub fn on_rms(&mut self, rms: f32, silence_threshold: f32) -> VadEvent {
        if rms < silence_threshold {
            let was_speaking = self.silent_for_ms == 0;
            self.silent_for_ms += self.poll_ms;
            if self.silent_for_ms >= self.silence_ms {
                VadEvent::Confirmed
            } else if was_speaking {
                VadEvent::SpawnSpeculative
            } else {
                VadEvent::None
            }
        } else {
            let was_silent = self.silent_for_ms != 0;
            self.silent_for_ms = 0;
            if was_silent {
                VadEvent::DiscardSpeculative
            } else {
                VadEvent::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── round trip ───────────────────────────────────────────────────────

    #[test]
    fn encode_then_decode_round_trips() {
        let src: Vec<f32> = (0..800).map(|i| (i as f32 / 40.0).sin() * 0.7).collect();
        let wav = encode_wav_mono_16k(&src);
        let got = decode_wav(&wav).expect("our own encoder must decode");
        assert_eq!(got.sample_rate, 16_000);
        assert_eq!(got.samples.len(), src.len());
        for (a, b) in src.iter().zip(&got.samples) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn scaling_matches_the_two_implementations_this_replaces() {
        // whisper used `* 32_767.0`, piper `* i16::MAX as f32`. Same number —
        // this pins it so a future edit cannot quietly change level.
        assert_eq!(f32_to_pcm16(&[1.0]), 32_767i16.to_le_bytes());
        assert_eq!(f32_to_pcm16(&[-1.0]), (-32_767i16).to_le_bytes());
        assert_eq!(f32_to_pcm16(&[0.0]), 0i16.to_le_bytes());
        // Out-of-range input clamps rather than wrapping.
        assert_eq!(f32_to_pcm16(&[9.0]), 32_767i16.to_le_bytes());
        assert_eq!(f32_to_pcm16(&[-9.0]), (-32_767i16).to_le_bytes());
    }

    #[test]
    fn the_header_is_the_canonical_44_bytes() {
        let wav = encode_wav_pcm16(&[0, 0, 0, 0], 22_050);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 48);
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 22_050);
    }

    // ── the shapes a real phone emits, which used to decode to noise ─────

    /// Builds a WAV with arbitrary extra chunks before `data`.
    fn wav_with(fmt_body: Vec<u8>, extra_chunks: &[(&[u8; 4], Vec<u8>)], data: Vec<u8>) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&(fmt_body.len() as u32).to_le_bytes());
        body.extend_from_slice(&fmt_body);
        if fmt_body.len() % 2 == 1 {
            body.push(0);
        }
        for (id, payload) in extra_chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        body.extend_from_slice(b"data");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);

        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn pcm_fmt(channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;
        let mut f = Vec::new();
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&byte_rate.to_le_bytes());
        f.extend_from_slice(&block_align.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        f
    }

    /// The classic breakage: the old decoder assumed `data` began at byte 44,
    /// so a LIST chunk shifted every sample and produced noise.
    #[test]
    fn a_list_chunk_before_data_no_longer_shifts_the_samples() {
        let pcm = f32_to_pcm16(&[0.5, -0.5, 0.25, -0.25]);
        let wav = wav_with(
            pcm_fmt(1, 16_000, 16),
            &[(b"LIST", b"INFOISFT\0Recorder".to_vec())],
            pcm,
        );
        let got = decode_wav(&wav).unwrap();
        assert_eq!(got.samples.len(), 4);
        assert!((got.samples[0] - 0.5).abs() < 1e-3, "{:?}", got.samples);
        assert!((got.samples[1] + 0.5).abs() < 1e-3);
    }

    #[test]
    fn an_18_byte_fmt_chunk_is_accepted() {
        let mut fmt = pcm_fmt(1, 16_000, 16);
        fmt.extend_from_slice(&0u16.to_le_bytes()); // cbSize
        let wav = wav_with(fmt, &[], f32_to_pcm16(&[0.5]));
        assert_eq!(decode_wav(&wav).unwrap().samples.len(), 1);
    }

    #[test]
    fn wave_format_extensible_resolves_through_its_subformat() {
        let mut fmt = pcm_fmt(1, 48_000, 16);
        fmt[0..2].copy_from_slice(&FMT_EXTENSIBLE.to_le_bytes());
        fmt.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        fmt.extend_from_slice(&16u16.to_le_bytes()); // valid bits
        fmt.extend_from_slice(&3u32.to_le_bytes()); // channel mask
        fmt.extend_from_slice(&FMT_PCM.to_le_bytes()); // SubFormat GUID head
        fmt.extend_from_slice(&[0u8; 14]);
        let wav = wav_with(fmt, &[], f32_to_pcm16(&[0.5, -0.5]));
        let got = decode_wav(&wav).unwrap();
        assert_eq!(got.sample_rate, 48_000);
        assert_eq!(got.samples.len(), 2);
    }

    #[test]
    fn stereo_is_downmixed_by_averaging_not_truncated() {
        // L = +0.5, R = -0.5 -> mono 0.0
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&f32_to_pcm16(&[0.5]));
        pcm.extend_from_slice(&f32_to_pcm16(&[-0.5]));
        let wav = wav_with(pcm_fmt(2, 16_000, 16), &[], pcm);
        let got = decode_wav(&wav).unwrap();
        assert_eq!(got.samples.len(), 1, "one frame, not two");
        assert!(got.samples[0].abs() < 1e-3, "got {}", got.samples[0]);
    }

    #[test]
    fn twenty_four_bit_pcm_sign_extends() {
        // -0.5 and +0.5 as 24-bit LE.
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&[0x00, 0x00, 0xC0]); // -4194304 / 8388608 = -0.5
        pcm.extend_from_slice(&[0x00, 0x00, 0x40]); // +4194304 / 8388608 = +0.5
        let wav = wav_with(pcm_fmt(1, 16_000, 24), &[], pcm);
        let got = decode_wav(&wav).unwrap();
        assert!((got.samples[0] + 0.5).abs() < 1e-3, "{:?}", got.samples);
        assert!((got.samples[1] - 0.5).abs() < 1e-3, "{:?}", got.samples);
    }

    #[test]
    fn eight_bit_pcm_is_unsigned_around_128() {
        let wav = wav_with(pcm_fmt(1, 8_000, 8), &[], vec![128, 255, 0]);
        let got = decode_wav(&wav).unwrap();
        assert!(got.samples[0].abs() < 1e-6, "128 is the midpoint");
        assert!(got.samples[1] > 0.9);
        assert!(got.samples[2] < -0.9);
    }

    #[test]
    fn ieee_float_32_is_read_directly() {
        let mut fmt = pcm_fmt(1, 44_100, 32);
        fmt[0..2].copy_from_slice(&FMT_IEEE_FLOAT.to_le_bytes());
        let mut pcm = Vec::new();
        pcm.extend_from_slice(&0.25f32.to_le_bytes());
        pcm.extend_from_slice(&(-0.75f32).to_le_bytes());
        let wav = wav_with(fmt, &[], pcm);
        let got = decode_wav(&wav).unwrap();
        assert!((got.samples[0] - 0.25).abs() < 1e-6);
        assert!((got.samples[1] + 0.75).abs() < 1e-6);
    }

    #[test]
    fn an_odd_sized_chunk_pad_byte_is_honoured() {
        let pcm = f32_to_pcm16(&[0.5, -0.5]);
        // 3-byte payload forces a pad byte before `data`.
        let wav = wav_with(pcm_fmt(1, 16_000, 16), &[(b"JUNK", vec![1, 2, 3])], pcm);
        assert_eq!(decode_wav(&wav).unwrap().samples.len(), 2);
    }

    // ── hostile and malformed input ──────────────────────────────────────

    #[test]
    fn non_riff_input_is_rejected_not_guessed_at() {
        assert_eq!(decode_wav(b"").unwrap_err(), WavError::NotRiff);
        assert_eq!(
            decode_wav(b"not a wav file").unwrap_err(),
            WavError::NotRiff
        );
        // An MP3 frame header, which a phone might well send.
        assert_eq!(
            decode_wav(&[0xFF, 0xFB, 0x90, 0x00]).unwrap_err(),
            WavError::NotRiff
        );
    }

    #[test]
    fn riff_without_a_fmt_chunk_reports_the_missing_chunk() {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        assert_eq!(
            decode_wav(&out).unwrap_err(),
            WavError::MissingChunk("fmt ")
        );
    }

    /// A chunk size far larger than the buffer must clamp, not panic — this is
    /// the shape a hostile upload takes.
    #[test]
    fn a_lying_chunk_size_cannot_read_past_the_buffer() {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&u32::MAX.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&u32::MAX.to_le_bytes()); // claims 4 GiB
        out.extend_from_slice(&pcm_fmt(1, 16_000, 16));
        let err = decode_wav(&out).unwrap_err();
        assert!(matches!(err, WavError::MissingChunk(_)), "{err:?}");
    }

    #[test]
    fn a_truncated_fmt_chunk_is_an_error_not_a_panic() {
        let wav = wav_with(vec![1, 0, 1, 0], &[], vec![]);
        assert_eq!(decode_wav(&wav).unwrap_err(), WavError::Truncated);
    }

    #[test]
    fn zero_channels_or_rate_is_rejected() {
        let z = wav_with(pcm_fmt(0, 16_000, 16), &[], vec![0, 0]);
        assert!(matches!(decode_wav(&z), Err(WavError::Unsupported(_))));
        let r = wav_with(pcm_fmt(1, 0, 16), &[], vec![0, 0]);
        assert!(matches!(decode_wav(&r), Err(WavError::Unsupported(_))));
    }

    #[test]
    fn an_unsupported_depth_names_itself_rather_than_returning_noise() {
        let wav = wav_with(pcm_fmt(1, 16_000, 12), &[], vec![0, 0, 0]);
        match decode_wav(&wav) {
            Err(WavError::Unsupported(d)) => assert!(d.contains("12"), "{d}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped_not_read_past() {
        // 5 bytes of 16-bit stereo = one whole frame (4 bytes) + 1 stray.
        let wav = wav_with(pcm_fmt(2, 16_000, 16), &[], vec![0, 0, 0, 0, 7]);
        assert_eq!(decode_wav(&wav).unwrap().samples.len(), 1);
    }

    #[test]
    fn an_empty_data_chunk_yields_no_samples_rather_than_an_error() {
        let wav = wav_with(pcm_fmt(1, 16_000, 16), &[], vec![]);
        let got = decode_wav(&wav).unwrap();
        assert!(got.samples.is_empty());
        assert_eq!(got.sample_rate, 16_000);
    }

    /// Every prefix of a valid file must fail cleanly. This is the cheap
    /// stand-in for a fuzzer over the parser.
    #[test]
    fn no_prefix_of_a_valid_wav_can_panic() {
        let full = wav_with(
            pcm_fmt(2, 44_100, 24),
            &[(b"LIST", b"INFO".to_vec()), (b"JUNK", vec![9; 7])],
            vec![1; 60],
        );
        for n in 0..full.len() {
            let _ = decode_wav(&full[..n]); // must not panic
        }
    }

    // ── level, rate, silence ─────────────────────────────────────────────

    #[test]
    fn rms_of_silence_is_zero_and_of_full_scale_is_one() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0; 16]), 0.0);
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn resampling_is_identity_at_the_target_rate() {
        let s = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&s, 16_000), s);
        // Degenerate inputs must not divide by zero or panic.
        assert_eq!(resample_to_16k(&s, 0), s);
        assert!(resample_to_16k(&[], 44_100).is_empty());
    }

    #[test]
    fn downsampling_halves_the_length() {
        let s: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        assert_eq!(resample_to_16k(&s, 32_000).len(), 50);
    }

    #[test]
    fn the_vad_reports_a_silence_run_once_then_confirms() {
        const SILENCE: f32 = 0.001;
        const SPEECH: f32 = 0.5;
        const T: f32 = 0.01;
        let mut vad = SpeculativeVad::new(300, 100);

        assert_eq!(vad.on_rms(SPEECH, T), VadEvent::None);
        assert_eq!(vad.on_rms(SILENCE, T), VadEvent::SpawnSpeculative);
        assert_eq!(vad.on_rms(SILENCE, T), VadEvent::None, "only once per run");
        assert_eq!(vad.on_rms(SILENCE, T), VadEvent::Confirmed);
    }

    #[test]
    fn resumed_speech_discards_the_speculative_result() {
        const SILENCE: f32 = 0.001;
        const SPEECH: f32 = 0.5;
        const T: f32 = 0.01;
        let mut vad = SpeculativeVad::new(1_000, 100);
        assert_eq!(vad.on_rms(SILENCE, T), VadEvent::SpawnSpeculative);
        assert_eq!(vad.on_rms(SPEECH, T), VadEvent::DiscardSpeculative);
        assert_eq!(vad.on_rms(SPEECH, T), VadEvent::None);
        assert_eq!(vad.on_rms(SILENCE, T), VadEvent::SpawnSpeculative);
    }

    /// Regression against a real file, not a synthetic one.
    ///
    /// GIAP's own `tests/blobs/jfk.wav` fixture carries a 26-byte `LIST` chunk
    /// between `fmt ` and `data`, so its payload begins at byte 78. The decoder
    /// this replaces started reading at a hardcoded 44 and therefore prepended
    /// 17 samples of chunk header as audio. It was even and thus stayed i16
    /// aligned, so the rest of the file decoded correctly — which is precisely
    /// why it went unnoticed. The existing whisper-side test hand-rolled a
    /// `data` search to dodge the same bug the production path had.
    #[test]
    fn the_real_jfk_fixture_has_a_pre_data_chunk_and_still_decodes() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("jfk.wav fixture missing — skipping");
            return;
        };

        // The fixture really does have the shape this test is about.
        let data_at = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("fixture must have a data chunk")
            + 8;
        assert_ne!(
            data_at, 44,
            "fixture no longer exercises the pre-data-chunk case"
        );

        let got = decode_wav(&bytes).expect("real-world WAV must decode");
        assert_eq!(got.sample_rate, 16_000);

        // Length must match the declared data chunk, not the distance from 44.
        assert_eq!(got.samples.len(), (bytes.len() - data_at) / 2);

        // And the first samples must be real audio, not chunk header bytes
        // reinterpreted as PCM. jfk.wav opens on near-silence.
        assert!(
            got.samples[..64].iter().all(|s| s.abs() < 0.05),
            "leading samples look like chunk bytes, not audio"
        );

        // What the old decoder would have produced, for contrast.
        let old_len = (bytes.len() - 44) / 2;
        assert_eq!(
            old_len - got.samples.len(),
            (data_at - 44) / 2,
            "the old path emitted this many junk samples"
        );
    }
}
