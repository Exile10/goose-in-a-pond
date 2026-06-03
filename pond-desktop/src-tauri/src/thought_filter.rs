// Ported from crates/pond-api/src/thought_filter.rs
//
// pond-desktop is NOT in the root Cargo workspace, so we can't import pond-api.
// This is an exact copy of the server-side ThoughtFilter that strips all
// thinking/reasoning tag formats from streaming text.

/// Paired tags whose entire contents (and the tags themselves) are dropped.
const PAIRED_TAGS: &[(&str, &str)] = &[
    ("<|channel>thought", "<channel|>"),
    ("<|tool_call>",      "<tool_call|>"),
    ("<think>",           "</think>"),
    ("<thought>",         "</thought>"),
];

/// Standalone sentinels that get silently dropped wherever they appear.
const STANDALONE_SENTINELS: &[&str] = &[
    "<eos>", "<|eos|>", "<end_of_turn>",
    "</think>", "</thought>",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Normal,
    InsideBlock(&'static str),
}

/// Stateful per-stream filter. Reuse a single instance across all chunks of
/// one response; allocate a new one per response.
pub struct ThoughtFilter {
    state: State,
    buf: String,
}

impl ThoughtFilter {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            buf: String::new(),
        }
    }

    /// Feed a chunk; returns the (possibly empty) substring that should be
    /// forwarded downstream right now.
    pub fn push(&mut self, chunk: &str) -> String {
        self.buf.push_str(chunk);
        let mut out = String::new();
        loop {
            match &self.state {
                State::Normal => {
                    let earliest_pair = PAIRED_TAGS
                        .iter()
                        .filter_map(|&(open, close)| self.buf.find(open).map(|i| (i, open, close)))
                        .min_by_key(|&(i, _, _)| i);

                    if let Some((i, open, close)) = earliest_pair {
                        out.push_str(&strip_standalones(&self.buf[..i]));
                        self.buf.drain(..i + open.len());
                        self.state = State::InsideBlock(close);
                    } else {
                        let look = max_normal_lookahead();
                        let safe = safe_emit_len(&self.buf, look);
                        out.push_str(&strip_standalones(&self.buf[..safe]));
                        self.buf.drain(..safe);
                        break;
                    }
                }
                State::InsideBlock(close) => {
                    let close_tag = *close;
                    if let Some(i) = self.buf.find(close_tag) {
                        self.buf.drain(..i + close_tag.len());
                        self.state = State::Normal;
                    } else {
                        let safe = safe_emit_len(&self.buf, close_tag.len());
                        self.buf.drain(..safe);
                        break;
                    }
                }
            }
        }
        out
    }

    /// Stream-end flush.
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.buf);
        match self.state {
            State::Normal => strip_standalones(&pending),
            State::InsideBlock(_) => String::new(),
        }
    }
}

fn max_normal_lookahead() -> usize {
    let opens = PAIRED_TAGS.iter().map(|(o, _)| o.len()).max().unwrap_or(0);
    let stand = STANDALONE_SENTINELS.iter().map(|s| s.len()).max().unwrap_or(0);
    opens.max(stand)
}

fn strip_standalones(s: &str) -> String {
    let mut out = s.to_string();
    for sentinel in STANDALONE_SENTINELS {
        if out.contains(sentinel) {
            out = out.replace(sentinel, "");
        }
    }
    out
}

fn safe_emit_len(s: &str, tag_len: usize) -> usize {
    let mut cut = s.len().saturating_sub(tag_len.saturating_sub(1));
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> String {
        let mut f = ThoughtFilter::new();
        let mut out = String::new();
        for c in chunks {
            out.push_str(&f.push(c));
        }
        out.push_str(&f.flush());
        out
    }

    #[test]
    fn passes_through_when_no_tags() {
        assert_eq!(run(&["Hello, ", "world!"]), "Hello, world!");
    }

    #[test]
    fn strips_channel_thought() {
        assert_eq!(
            run(&["<|channel>thought reasoning<channel|>Hi!"]),
            "Hi!",
        );
    }

    #[test]
    fn strips_think_block() {
        assert_eq!(
            run(&["<think>reasoning</think>Answer."]),
            "Answer.",
        );
    }

    #[test]
    fn strips_thought_block() {
        assert_eq!(
            run(&["<thought>reasoning</thought>Answer."]),
            "Answer.",
        );
    }

    #[test]
    fn strips_split_across_chunks() {
        assert_eq!(run(&["<thi", "nk>reason</thi", "nk>ok"]), "ok");
    }

    #[test]
    fn strips_eos_sentinel() {
        assert_eq!(run(&["Hello!<eos>"]), "Hello!");
    }

    #[test]
    fn strips_orphaned_close_tags() {
        assert_eq!(run(&["Hello!</think>"]), "Hello!");
    }
}
