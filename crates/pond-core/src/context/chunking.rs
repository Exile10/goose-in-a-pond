//! Splitting a long text into passages worth embedding on their own: one vector over a whole mail
//! describes its signature block as much as its point. A [`Chunk`] is a SPAN, never a copy — an
//! orphaned index row carrying a snippet is deleted data that outlived its deletion. Cuts prefer,
//! in order, a blank line, a line break, a sentence end, a space; mid-word is the last resort.

/// A passage of a source text, addressed by where it is rather than by content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Byte offset into the source text, counting from 0.
    pub start: usize,
    /// Byte length of the passage.
    pub len: usize,
}

impl Chunk {
    /// The passage itself, for a caller that has the source text in hand.
    ///
    /// `None` when the span misses a character boundary, which means the text changed since the
    /// chunk was computed: a stale span is better refused than sliced into invalid UTF-8.
    pub fn slice<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.get(self.start..self.start.checked_add(self.len)?)
    }
}

/// Target passage size in bytes.
///
/// 500 is chosen for short-form mail: long enough to carry an argument rather than a fragment,
/// short enough that one message yields several passages.
pub const DEFAULT_CHUNK_BYTES: usize = 500;

/// How much of the previous passage each one repeats.
///
/// Without overlap a sentence straddling a cut is findable from neither passage. 50 bytes is
/// about a sentence: enough to keep a straddler whole, small enough not to double the corpus.
pub const DEFAULT_OVERLAP_BYTES: usize = 50;

/// Split `text` into overlapping passages.
///
/// Text shorter than one chunk yields one chunk covering all of it, so callers need no special
/// case. Empty or whitespace-only text yields nothing.
pub fn chunk(text: &str, size: usize, overlap: usize) -> Vec<Chunk> {
    let size = size.max(1);
    // Overlap is capped at HALF the chunk, which bounds the output at roughly `2 * len / size`
    // passages. A cap of `size - 1` also terminates but advances one byte per step: a 5 KB body
    // yields 4,901 chunks instead of about 100, useless without ever failing.
    let overlap = overlap.min(size / 2);

    if text.trim().is_empty() {
        return Vec::new();
    }
    if text.len() <= size {
        return vec![Chunk {
            start: 0,
            len: text.len(),
        }];
    }

    let mut out = Vec::new();
    let mut start = 0usize;

    while start < text.len() {
        let remaining = text.len() - start;
        if remaining <= size {
            out.push(Chunk {
                start,
                len: remaining,
            });
            break;
        }

        let hard_end = start + size;
        let end = boundary_at_or_before(text, start, hard_end);
        out.push(Chunk {
            start,
            len: end - start,
        });

        // Step back by the overlap, then forward to a character boundary so the
        // next span is sliceable.
        let next = end.saturating_sub(overlap).max(start + 1);
        start = ceil_char_boundary(text, next);
    }

    out
}

/// The nicest cut at or before `hard_end`, never before `start + 1`.
fn boundary_at_or_before(text: &str, start: usize, hard_end: usize) -> usize {
    let hard_end = floor_char_boundary(text, hard_end.min(text.len()));
    let window = &text[start..hard_end];
    // Only look back a quarter of the chunk: a cut that gives up too much text
    // to find a prettier boundary produces short, thin passages.
    let floor = window.len().saturating_sub(window.len() / 4);

    for pattern in ["\n\n", "\n", ". ", " "] {
        if let Some(at) = window.rfind(pattern) {
            let cut = at + pattern.len();
            if cut >= floor && cut > 0 {
                return floor_char_boundary(text, start + cut);
            }
        }
    }
    hard_end
}

fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    if i >= text.len() {
        return text.len();
    }
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i.min(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(text: &str, chunks: &[Chunk]) -> bool {
        // Every byte of the text appears in at least one chunk.
        let mut seen = vec![false; text.len()];
        for c in chunks {
            for b in c.start..(c.start + c.len).min(text.len()) {
                seen[b] = true;
            }
        }
        seen.into_iter().all(|b| b)
    }

    #[test]
    fn short_text_is_one_chunk() {
        let t = "Rent is due on the first.";
        let c = chunk(t, 500, 50);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].slice(t), Some(t));
    }

    #[test]
    fn empty_or_blank_text_yields_nothing() {
        assert!(chunk("", 500, 50).is_empty());
        assert!(chunk("   \n\n  ", 500, 50).is_empty());
    }

    /// The property that matters most: no byte of the source is unreachable.
    /// A gap is a passage nothing can ever find.
    #[test]
    fn chunks_cover_the_whole_text() {
        let t = "word ".repeat(600);
        let c = chunk(&t, 500, 50);
        assert!(c.len() > 1);
        assert!(cover(&t, &c), "chunks left a hole");
    }

    #[test]
    fn chunks_overlap_so_a_straddling_sentence_survives() {
        let t = "abcdefghij ".repeat(200);
        let c = chunk(&t, 500, 50);
        for pair in c.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(
                b.start < a.start + a.len,
                "consecutive chunks do not overlap: {a:?} then {b:?}"
            );
        }
    }

    #[test]
    fn every_chunk_is_sliceable() {
        // Multi-byte characters throughout, so a naive byte cut would panic.
        let t = "héllo wörld — this is a sentence. ".repeat(60);
        for c in chunk(&t, 500, 50) {
            assert!(c.slice(&t).is_some(), "chunk {c:?} is not on a boundary");
        }
    }

    /// A passage starting mid-word embeds worse and reads worse.
    #[test]
    fn cuts_prefer_a_boundary_over_the_hard_limit() {
        let t = format!("{}\n\n{}", "a".repeat(400), "b".repeat(400));
        let c = chunk(&t, 500, 50);
        // The first cut should land on the blank line, not at byte 500.
        assert_eq!(c[0].len, 402, "expected the cut at the paragraph break");
    }

    /// Overlap >= size would step backwards forever.
    #[test]
    fn a_silly_overlap_still_terminates() {
        let t = "x".repeat(5000);
        let c = chunk(&t, 100, 1000);
        // Bounded by 2 * len / size, not merely finite.
        assert!(
            c.len() <= 2 * t.len() / 100,
            "overlap clamping left {} chunks for {} bytes",
            c.len(),
            t.len()
        );
        assert!(cover(&t, &c));
    }

    /// A span computed against different text must refuse rather than slice
    /// blindly — the body may have been re-synced since.
    #[test]
    fn a_stale_span_refuses_rather_than_slicing_garbage() {
        let c = Chunk {
            start: 10,
            len: 500,
        };
        assert_eq!(c.slice("short"), None);
    }
}
