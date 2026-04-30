//! Stateful streaming filter that strips reasoning/thinking markup from
//! token streams before they reach the user (TTS or display).
//!
//! Handles all known tag formats:
//!   1. `<|channel>thought ... <channel|>` (Gemma 4)
//!   2. `<|tool_call> ... <tool_call|>`   (Harmony tool markup)
//!   3. `<think> ... </think>`            (Qwen3, DeepSeek-R1)
//!   4. `<thought> ... </thought>`        (alternate)
//!
//! Also strips standalone sentinels: `<eos>`, `<|eos|>`, `<end_of_turn>`,
//! and orphaned close tags (`</think>`, `</thought>`).
//!
//! Reuse a single instance across all chunks of one response.

/// Paired tags whose entire contents (and the tags themselves) are dropped.
const PAIRED_TAGS: &[(&str, &str)] = &[
    ("<|channel>thought", "<channel|>"),
    ("<|tool_call>",      "<tool_call|>"),
    ("<think>",           "</think>"),
    ("<thought>",         "</thought>"),
];

/// Standalone sentinels silently dropped wherever they appear.
const STANDALONE_SENTINELS: &[&str] = &[
    "<eos>", "<|eos|>", "<end_of_turn>",
    "</think>", "</thought>",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Normal,
    InsideBlock(&'static str),
}

/// Stateful per-stream filter.
pub struct ThoughtFilter {
    state: State,
    buf: String,
}

impl Default for ThoughtFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ThoughtFilter {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            buf: String::new(),
        }
    }

    /// Feed a chunk; returns the visible text that should be forwarded.
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

    /// Stream-end flush. Emits remaining buffered text in Normal state;
    /// drops it if inside an unclosed block.
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
    fn passthrough() {
        assert_eq!(run(&["Hello, ", "world!"]), "Hello, world!");
    }

    #[test]
    fn strips_channel_thought() {
        assert_eq!(run(&["<|channel>thought reasoning<channel|>Hi!"]), "Hi!");
    }

    #[test]
    fn strips_think() {
        assert_eq!(run(&["<think>reasoning</think>Answer."]), "Answer.");
    }

    #[test]
    fn strips_thought() {
        assert_eq!(run(&["<thought>reasoning</thought>Answer."]), "Answer.");
    }

    #[test]
    fn strips_tool_call() {
        assert_eq!(run(&["<|tool_call>call:x{}<tool_call|>"]), "");
    }

    #[test]
    fn strips_split_across_chunks() {
        assert_eq!(run(&["<thi", "nk>reason</thi", "nk>ok"]), "ok");
    }

    #[test]
    fn strips_channel_split_across_chunks() {
        assert_eq!(run(&["<|chan", "nel>thought reason<chan", "nel|>Hi"]), "Hi");
    }

    #[test]
    fn strips_eos() {
        assert_eq!(run(&["Hello!<eos>"]), "Hello!");
    }

    #[test]
    fn strips_orphaned_close() {
        assert_eq!(run(&["Hello!</think>"]), "Hello!");
    }

    #[test]
    fn drops_unclosed_block() {
        assert_eq!(run(&["<think>never closes"]), "");
    }

    #[test]
    fn text_before_and_after() {
        assert_eq!(run(&["Sure! <think>hmm</think>Here."]), "Sure! Here.");
    }

    #[test]
    fn per_token_streaming() {
        let raw = "<think>step by step</think>The answer is 7.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "The answer is 7.");
    }

    #[test]
    fn gemma4_per_token() {
        let raw = "<|channel>thought The user said hi.<channel|>Hello!";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "Hello!");
    }
}
