//! Byte-stream framing for a capture child that emits a bare concatenation of JPEGs
//! (ffmpeg `-c:v copy -f mjpeg`).

use std::time::{Duration, Instant};

use anyhow::{bail, Result};

/// A frame larger than this is a corrupt stream, not a picture.
pub const MAX_PART_BYTES: usize = 8 * 1024 * 1024;

/// Splits concatenated JPEGs by walking their marker segments, so an `FF D9` inside a header
/// cannot end a frame early.
#[derive(Default)]
pub struct JpegSplitter {
    buf: Vec<u8>,
    pos: usize,
    state: SplitState,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum SplitState {
    #[default]
    Soi,
    Marker,
    Entropy,
}

impl JpegSplitter {
    #[cfg(test)]
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Room for a read straight into the buffer.
    pub fn buffer_mut(&mut self) -> &mut Vec<u8> {
        &mut self.buf
    }

    /// The next complete JPEG, `Ok(None)` when more bytes are needed.
    pub fn next_jpeg(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if self.pos > MAX_PART_BYTES {
                bail!("the camera sent a JPEG over {MAX_PART_BYTES} bytes");
            }
            match self.state {
                SplitState::Soi => match find(&self.buf, &[0xFF, 0xD8]) {
                    Some(start) => {
                        self.buf.drain(..start);
                        self.pos = 2;
                        self.state = SplitState::Marker;
                    }
                    None => {
                        let keep = usize::from(self.buf.last() == Some(&0xFF));
                        self.buf.drain(..self.buf.len() - keep);
                        return Ok(None);
                    }
                },
                SplitState::Marker => {
                    let Some(&[ff, marker]) = self.buf.get(self.pos..self.pos + 2) else {
                        return Ok(None);
                    };
                    if ff != 0xFF {
                        self.resync();
                        continue;
                    }
                    match marker {
                        0xFF => self.pos += 1,
                        0xD9 => {
                            let rest = self.buf.split_off(self.pos + 2);
                            self.pos = 0;
                            self.state = SplitState::Soi;
                            return Ok(Some(std::mem::replace(&mut self.buf, rest)));
                        }
                        // A new picture before this one ended: this one was cut short.
                        0xD8 => {
                            self.buf.drain(..self.pos);
                            self.pos = 2;
                        }
                        0x01 | 0xD0..=0xD7 => self.pos += 2,
                        _ => {
                            let Some(&[hi, lo]) = self.buf.get(self.pos + 2..self.pos + 4) else {
                                return Ok(None);
                            };
                            let len = usize::from(u16::from_be_bytes([hi, lo]));
                            if len < 2 {
                                self.resync();
                                continue;
                            }
                            self.pos += 2 + len;
                            if marker == 0xDA {
                                self.state = SplitState::Entropy;
                            }
                        }
                    }
                }
                SplitState::Entropy => {
                    // Scan data stuffs every FF as FF 00; RSTn markers stay inside the scan.
                    let mut i = self.pos;
                    loop {
                        let Some(off) = self
                            .buf
                            .get(i..)
                            .and_then(|b| b.iter().position(|&x| x == 0xFF))
                        else {
                            self.pos = self.buf.len();
                            return Ok(None);
                        };
                        i += off;
                        let Some(&next) = self.buf.get(i + 1) else {
                            self.pos = i;
                            return Ok(None);
                        };
                        match next {
                            0x00 | 0xD0..=0xD7 => i += 2,
                            0xFF => i += 1,
                            _ => {
                                self.pos = i;
                                self.state = SplitState::Marker;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    fn resync(&mut self) {
        self.buf.drain(..1);
        self.pos = 0;
        self.state = SplitState::Soi;
    }
}

/// Admits frames at no more than a target rate, keeping to a schedule so jitter cannot drift it.
pub struct RateGate {
    interval: Duration,
    slack: Duration,
    next_due: Option<Instant>,
}

impl RateGate {
    pub fn new(fps: u32) -> Self {
        let interval = Duration::from_secs(1) / fps.max(1);
        Self {
            interval,
            // Tolerates jitter; at half an interval a camera at twice the rate would pass every frame.
            slack: interval * 2 / 5,
            next_due: None,
        }
    }

    pub fn admit(&mut self, now: Instant) -> bool {
        match self.next_due {
            Some(due) if now + self.slack < due => false,
            Some(due) => {
                let next = due + self.interval;
                // Behind by more than a frame after a stall: restart the schedule from now.
                self.next_due = Some(if next < now {
                    now + self.interval
                } else {
                    next
                });
                true
            }
            None => {
                self.next_due = Some(now + self.interval);
                true
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(seed: u8) -> Vec<u8> {
        let frame = pond_core::user_data::domain::vision::Frame {
            width: 16,
            height: 8,
            rgb: (0..16 * 8 * 3)
                .map(|i| (i as u8).wrapping_mul(seed))
                .collect(),
            captured_at: chrono::Utc::now(),
        };
        crate::snapshot::frame_to_jpeg_bytes(&frame).unwrap()
    }

    fn drain(s: &mut JpegSplitter) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| s.next_jpeg().unwrap()).collect()
    }

    #[test]
    fn splits_concatenated_jpegs_fed_a_byte_at_a_time() {
        let (a, b, c) = (jpeg(3), jpeg(7), jpeg(11));
        let stream = [a.clone(), b.clone(), c.clone()].concat();
        let mut s = JpegSplitter::default();
        let mut out = Vec::new();
        for byte in &stream {
            s.push(std::slice::from_ref(byte));
            out.extend(drain(&mut s));
        }
        assert_eq!(out, vec![a, b, c]);
    }

    #[test]
    fn an_end_marker_inside_a_header_does_not_end_the_frame() {
        // An APP1 segment whose payload holds FF D9.
        let real = jpeg(5);
        let mut with_app = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x06, 0xFF, 0xD9, 0xFF, 0xD9];
        with_app.extend_from_slice(&real[2..]);
        let mut s = JpegSplitter::default();
        s.push(&with_app);
        assert_eq!(drain(&mut s), vec![with_app]);
    }

    #[test]
    fn garbage_and_frames_cut_mid_picture_are_skipped() {
        let (a, b) = (jpeg(3), jpeg(9));
        let cut = a[..a.len() - 20].to_vec();
        let stream = [b"noise".to_vec(), cut, b.clone(), b"tail".to_vec()].concat();
        let mut s = JpegSplitter::default();
        s.push(&stream);
        assert_eq!(drain(&mut s), vec![b]);
    }

    #[test]
    fn an_endless_frame_is_an_error_not_unbounded_memory() {
        let mut s = JpegSplitter::default();
        s.push(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02]);
        s.push(&vec![0x11; MAX_PART_BYTES + 16]);
        assert!(s.next_jpeg().unwrap().is_none());
        assert!(s.next_jpeg().is_err());
    }

    #[test]
    fn the_gate_halves_a_stream_twice_the_target_rate_despite_jitter() {
        let start = Instant::now();
        let mut gate = RateGate::new(15);
        let admitted = (0..30)
            .filter(|i| {
                let jitter = if i % 3 == 0 { 4 } else { 0 };
                gate.admit(start + Duration::from_millis(i * 1000 / 30 + jitter))
            })
            .count();
        assert_eq!(admitted, 15);
    }

    #[test]
    fn the_gate_passes_a_stream_at_or_below_the_target_rate() {
        let start = Instant::now();
        let mut gate = RateGate::new(30);
        assert!((0..20).all(|i| gate.admit(start + Duration::from_millis(i * 70))));
        let mut gate = RateGate::new(5);
        assert!(gate.admit(start));
        assert!(
            gate.admit(start + Duration::from_millis(190)),
            "slightly early"
        );
        assert!(!gate.admit(start + Duration::from_millis(250)));
        assert!(gate.admit(start + Duration::from_secs(5)), "after a stall");
        assert!(gate.admit(start + Duration::from_millis(5200)));
    }
}
