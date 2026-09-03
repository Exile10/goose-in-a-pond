//! Streaming filter that strips two flavours of Harmony-style markup from
//! token streams before they reach the SSE consumer:
//!
//!   1. Reasoning-channel preambles: `<|channel>thought ... <channel|>...`
//!   2. Inline tool-call markup:     `<|tool_call> ... <tool_call|>`
//!      (and the Gemma-style end-of-sequence sentinel `<eos>` that some
//!      models keep emitting after they stop generating useful tokens)
//!
//! Rationale: Llamafile + Ollama implement `LlmProvider::stream_complete`
//! natively, so the per-token output bypasses the post-`complete()` strip in
//! `pond-adapters-local-inference::strip_thinking_tokens`. Reasoning-capable
//! models (Gemma 4, gpt-oss, similar Harmony-channel families) leak their
//! thinking preamble — and now their inline tool-call gibberish — straight
//! to the user.
//!
//! This filter withholds only the bytes that could still turn out to be the
//! start of a marker -- the longest suffix of its buffer that is a proper
//! prefix of some open tag or sentinel. Ordinary prose matches nothing, so it
//! is forwarded the instant it arrives; a marker split across chunks is still
//! caught on the next push. Everything between a matched pair is suppressed,
//! and whatever remains (post-close-tag) is flushed on stream end.
//!
//! The holdback being *conditional* is the whole point. An earlier version
//! withheld a fixed 16 bytes on every push regardless of content, which made
//! the visible answer permanently trail generation by that much and froze the
//! chat mid-word whenever generation slowed. `plain_text_is_emitted_with_no_holdback`
//! is the guard.

/// Paired tags whose entire contents (and the tags themselves) are dropped.
/// Each pair = (open marker, close marker). The first pair encountered wins
/// — we don't expect nesting in practice.
const PAIRED_TAGS: &[(&str, &str)] = &[
    ("<|channel>thought", "<channel|>"),
    ("<|tool_call>", "<tool_call|>"),
    ("<think>", "</think>"),
    // `<thinking>` is a DISTINCT literal, not a prefix match for `<think>` --
    // the closing `>` makes them disjoint, so order here does not matter.
    // `pond-core`'s twin filter has carried this pair for a while; this one did
    // not, so a model using the longer spelling had its entire reasoning block
    // rendered to the user as the answer.
    ("<thinking>", "</thinking>"),
    ("<thought>", "</thought>"),
];

/// Standalone sentinels that get silently dropped wherever they appear in
/// the stream. Some models (Gemma-family especially) keep emitting `<eos>`
/// after the real reply ends; the chat UI then renders them literally.
const STANDALONE_SENTINELS: &[&str] = &[
    "<eos>",
    "<|eos|>",
    "<end_of_turn>",
    // Orphaned close tags (model emitted close without a matching open):
    "</think>",
    "</thinking>",
    "</thought>",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Normal,
    /// Inside a paired-tag block. The `&'static str` holds the close tag we
    /// are looking for, so we don't have to remember which open tag matched.
    InsideBlock(&'static str),
}

/// Stateful per-stream filter. Reuse a single instance across all chunks of
/// one response; allocate a new one per response.
pub struct ThoughtFilter {
    state: State,
    buf: String,
    /// Per-block accumulator for the body of a paired-tag envelope.  The
    /// `<|channel>thought ...` block is discarded; the `<|tool_call> ...`
    /// block is captured here so the SSE handler can surface a "the model
    /// tried to call X but didn't use the proper protocol" notice instead
    /// of silently dropping it.
    block_body: String,
    /// Open tag of the block we are currently inside, so we can decide
    /// whether to keep the body (tool_call) or discard it (channel/thought).
    inside_open_tag: Option<&'static str>,
    /// Tool-call envelope bodies completed since the last `take_tool_calls`.
    captured_tool_calls: Vec<String>,
    /// When true, thinking/reasoning blocks are captured (not just discarded)
    /// so they can be forwarded as SSE thinking events.
    capture_thinking: bool,
    /// Thinking blocks captured since the last `take_thinking`.
    captured_thinking: Vec<String>,
}

impl Default for ThoughtFilter {
    fn default() -> Self {
        Self {
            state: State::Normal,
            buf: String::new(),
            block_body: String::new(),
            inside_open_tag: None,
            captured_tool_calls: Vec::new(),
            capture_thinking: false,
            captured_thinking: Vec::new(),
        }
    }
}

impl ThoughtFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a filter that captures thinking blocks for forwarding as events.
    pub fn with_thinking_capture(mut self) -> Self {
        self.capture_thinking = true;
        self
    }

    /// Feed a chunk; returns the (possibly empty) substring that should be
    /// forwarded downstream right now. Tokens that overlap a partial tag are
    /// held back until the next call resolves the ambiguity.
    pub fn push(&mut self, chunk: &str) -> String {
        self.buf.push_str(chunk);
        let mut out = String::new();
        loop {
            match &self.state {
                State::Normal => {
                    // Find the earliest open tag among the paired set.
                    let earliest_pair = PAIRED_TAGS
                        .iter()
                        .filter_map(|&(open, close)| self.buf.find(open).map(|i| (i, open, close)))
                        .min_by_key(|&(i, _, _)| i);

                    if let Some((i, open, close)) = earliest_pair {
                        // Strip standalone sentinels from the chunk we're about
                        // to emit (they can appear anywhere in Normal text).
                        out.push_str(&strip_standalones(&self.buf[..i]));
                        self.buf.drain(..i + open.len());
                        self.inside_open_tag = Some(open);
                        self.block_body.clear();
                        self.state = State::InsideBlock(close);
                    } else {
                        // No paired tag visible. Hold back only a tail that
                        // could still become one -- normally nothing at all.
                        let safe = safe_emit_len(&self.buf, &NORMAL_MARKERS);
                        out.push_str(&strip_standalones(&self.buf[..safe]));
                        self.buf.drain(..safe);
                        break;
                    }
                }
                State::InsideBlock(close) => {
                    let close_tag = *close;
                    if let Some(i) = self.buf.find(close_tag) {
                        // Capture the body of paired-tag envelopes:
                        // - tool_call: always captured for UI surfacing
                        // - channel/thought: captured only when capture_thinking is on
                        self.block_body.push_str(&self.buf[..i]);
                        if matches!(self.inside_open_tag, Some("<|tool_call>")) {
                            let body = std::mem::take(&mut self.block_body);
                            let trimmed = body.trim();
                            if !trimmed.is_empty() {
                                self.captured_tool_calls.push(trimmed.to_string());
                            }
                        } else if self.capture_thinking {
                            // Thinking block — capture for SSE thinking events
                            let body = std::mem::take(&mut self.block_body);
                            let trimmed = body
                                .trim()
                                .strip_prefix("thought")
                                .unwrap_or(body.trim())
                                .trim();
                            if !trimmed.is_empty() {
                                self.captured_thinking.push(trimmed.to_string());
                            }
                        } else {
                            self.block_body.clear();
                        }
                        self.inside_open_tag = None;
                        self.buf.drain(..i + close_tag.len());
                        self.state = State::Normal;
                    } else {
                        // Discard everything but a tail that may start close.
                        let safe = safe_emit_len(&self.buf, &[close_tag]);
                        // Accumulate the safely-discarded portion for tool_call
                        // envelopes so we can surface it once close arrives.
                        self.block_body.push_str(&self.buf[..safe]);
                        self.buf.drain(..safe);
                        break;
                    }
                }
            }
        }
        out
    }

    /// Stream-end flush. Anything still buffered in Normal state is emitted
    /// (after stripping standalone sentinels); anything buffered inside a
    /// paired-tag block is dropped (the model never closed it).
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.buf);
        match self.state {
            State::Normal => strip_standalones(&pending),
            State::InsideBlock(close) => {
                // Dropping model output, so say so. The stream ended with a
                // reasoning or tool-call envelope still open, which means either
                // the model was cut off or it emitted an open marker it never
                // closed. Either way bytes are being discarded, and discarding
                // them silently is what made this class of bug invisible.
                tracing::warn!(
                    close_marker = close,
                    dropped_bytes = pending.len() + self.block_body.len(),
                    "stream ended inside an unclosed block; its body is discarded",
                );
                String::new()
            }
        }
    }

    /// Drain any tool-call envelopes captured since the last call. Returns
    /// the raw body text (everything between `<|tool_call>` and `<tool_call|>`),
    /// without the wrapping markers. Use [`parse_tool_envelope`] to extract
    /// the tool name and JSON arguments.
    pub fn take_tool_calls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.captured_tool_calls)
    }

    /// Drain any thinking/reasoning blocks captured since the last call.
    /// Only populated when `with_thinking_capture()` was called. Returns
    /// the reasoning text with the "thought" prefix stripped.
    pub fn take_thinking(&mut self) -> Vec<String> {
        std::mem::take(&mut self.captured_thinking)
    }
}

/// Best-effort parser for the body of a Harmony-style `<|tool_call>...
/// <tool_call|>` envelope. Common shapes seen in the wild:
///
///   `call:NAME{ARGS_JSON}`
///   `NAME{ARGS_JSON}`
///   `NAME(ARGS_JSON)`
///   `{"name": "NAME", "arguments": {...}}`
///
/// Returns `(tool_name, args_json_str)` when a recognised shape parses;
/// otherwise `None`. The args string is left as-is so callers can pass it
/// straight to a JSON parser or surface the raw text in a UI message.
pub fn parse_tool_envelope(body: &str) -> Option<(String, String)> {
    let s = body.trim();
    // Strip an optional `call:` prefix.
    let s = s.strip_prefix("call:").unwrap_or(s);

    // Form 1/2/3: NAME followed by ({...}) or {...}
    if let Some(open_idx) = s.find(|c: char| c == '{' || c == '(') {
        let name = s[..open_idx].trim().trim_end_matches(':').to_string();
        if !name.is_empty() {
            // Lift {} out of () wrapping if present.
            let raw_args = &s[open_idx..];
            let args = if let Some(stripped) =
                raw_args.strip_prefix('(').and_then(|x| x.strip_suffix(')'))
            {
                stripped.to_string()
            } else {
                raw_args.to_string()
            };
            return Some((name, args));
        }
    }

    // Form 4: full JSON object with `name` + `arguments`.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        if let (Some(name), args) = (v.get("name").and_then(|n| n.as_str()), v.get("arguments")) {
            let args_str = args
                .map(|a| a.to_string())
                .unwrap_or_else(|| "{}".to_string());
            return Some((name.to_string(), args_str));
        }
    }

    None
}

/// Every marker that can begin in `State::Normal`: the paired-tag OPEN markers
/// plus the standalone sentinels. Close markers are absent on purpose -- inside
/// a block only one close marker matters and it is already known.
///
/// Derived from the two tables rather than written out again, so adding a tag
/// cannot leave a stale copy behind. Built once: `push` runs per token, so a
/// per-call `Vec` here would be a per-token allocation.
static NORMAL_MARKERS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    PAIRED_TAGS
        .iter()
        .map(|&(open, _)| open)
        .chain(STANDALONE_SENTINELS.iter().copied())
        .collect()
});

/// Remove every occurrence of every standalone sentinel from the input.
/// Cheap because the sentinel set is tiny.
fn strip_standalones(s: &str) -> String {
    let mut out = s.to_string();
    for sentinel in STANDALONE_SENTINELS {
        if out.contains(sentinel) {
            out = out.replace(sentinel, "");
        }
    }
    out
}

/// Byte index up to which `s` can be emitted right now.
///
/// Withholds only the longest suffix of `s` that is a *proper* prefix of some
/// marker in `markers`. That suffix is the only thing which could still turn
/// into a marker once more tokens arrive, so holding it back is sufficient to
/// catch a marker split across chunk boundaries -- and holding back anything
/// more is what stalls the stream. Returns `s.len()` when no suffix could begin
/// a marker, which is the common case for ordinary prose.
///
/// A complete marker is deliberately *not* a match: proper prefixes only. In
/// `State::Normal` a complete open tag has already been found by `buf.find`,
/// and a complete sentinel is removed by [`strip_standalones`], so treating a
/// whole marker as a partial would withhold it forever.
///
/// The returned index is always a UTF-8 char boundary, so callers can slice at
/// it. Cost is bounded by the longest marker (17 bytes) times the marker count,
/// per push -- this runs per token, including on a Jetson Orin Nano.
fn safe_emit_len(s: &str, markers: &[&str]) -> usize {
    let longest = markers.iter().map(|m| m.len()).max().unwrap_or(0);
    // A proper prefix is at most `longest - 1` bytes, so nothing before this
    // point can be part of a partial marker.
    let earliest = s.len().saturating_sub(longest.saturating_sub(1));

    // Walk forwards from the earliest possible start and take the FIRST hit,
    // which is the longest withheld tail. `i < s.len()` keeps the empty suffix
    // out of the running -- every marker "starts with" it, and matching it
    // would withhold the whole buffer.
    for i in earliest..s.len() {
        if !s.is_char_boundary(i) {
            continue;
        }
        let tail = &s[i..];
        if markers
            .iter()
            .any(|m| m.len() > tail.len() && m.starts_with(tail))
        {
            return i;
        }
    }
    s.len()
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
    fn passes_through_when_no_tags_present() {
        assert_eq!(run(&["Hello, ", "world!"]), "Hello, world!");
    }

    #[test]
    fn strips_full_thought_preamble_in_one_chunk() {
        assert_eq!(
            run(&["<|channel>thought reasoning here<channel|>Hi there!"]),
            "Hi there!",
        );
    }

    #[test]
    fn strips_thought_split_across_chunks() {
        let chunks = &[
            "<|chan",
            "nel>thought ",
            "reason",
            "<chan",
            "nel|>",
            "Hello!",
        ];
        assert_eq!(run(chunks), "Hello!");
    }

    #[test]
    fn strips_thought_split_token_boundaries() {
        // Mimic real per-token streaming where each chunk is a few chars.
        let raw = "<|channel>thought The user said hi.<channel|>Hello! I am Goose.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "Hello! I am Goose.");
    }

    #[test]
    fn drops_unclosed_thought_block_on_flush() {
        // No closing tag — model misbehaved; we conservatively drop the buffer.
        assert_eq!(run(&["<|channel>thought never closes"]), "");
    }

    #[test]
    fn handles_text_before_thought_block() {
        assert_eq!(
            run(&["Sure! <|channel>thought hmm<channel|>Here you go."]),
            "Sure! Here you go.",
        );
    }

    #[test]
    fn strips_inline_tool_call_markup() {
        // Real-world leak: model emits the Harmony tool-call envelope as
        // plain text instead of using the structural tool-call mechanism.
        let raw = "<|tool_call>call:giap__get_current_weather{}<tool_call|>";
        assert_eq!(run(&[raw]), "");
    }

    #[test]
    fn captures_tool_call_envelope_body() {
        let mut f = ThoughtFilter::new();
        let _ = f.push("<|tool_call>call:giap__get_current_weather{}<tool_call|>");
        let calls = f.take_tool_calls();
        assert_eq!(calls.len(), 1);
        let (name, args) = parse_tool_envelope(&calls[0]).expect("parse");
        assert_eq!(name, "giap__get_current_weather");
        assert_eq!(args, "{}");
    }

    #[test]
    fn parse_tool_envelope_handles_brace_form() {
        let (n, a) = parse_tool_envelope("weather{\"city\":\"Nairobi\"}").unwrap();
        assert_eq!(n, "weather");
        assert_eq!(a, "{\"city\":\"Nairobi\"}");
    }

    #[test]
    fn parse_tool_envelope_handles_paren_form() {
        let (n, a) = parse_tool_envelope("weather({\"city\":\"X\"})").unwrap();
        assert_eq!(n, "weather");
        assert_eq!(a, "{\"city\":\"X\"}");
    }

    #[test]
    fn parse_tool_envelope_handles_full_json_form() {
        let (n, a) = parse_tool_envelope(r#"{"name":"weather","arguments":{"x":1}}"#).unwrap();
        assert_eq!(n, "weather");
        assert!(a.contains("\"x\""));
    }

    #[test]
    fn strips_eos_sentinel_anywhere_in_stream() {
        assert_eq!(run(&["Hello!<eos>"]), "Hello!");
        assert_eq!(run(&["one<eos> two<eos>"]), "one two");
        // Split across chunks — flush handles the tail.
        assert_eq!(run(&["bye", "<eo", "s>"]), "bye");
    }

    #[test]
    fn strips_thought_then_tool_call_back_to_back() {
        let raw = "<|channel>thought planning<channel|>OK <|tool_call>call:x{}<tool_call|>";
        assert_eq!(run(&[raw]), "OK ");
    }

    // ── <think> / <thought> tag tests ──────────────────────────────────────

    #[test]
    fn strips_think_block_in_one_chunk() {
        assert_eq!(
            run(&["<think>reasoning here</think>The answer is 42."]),
            "The answer is 42.",
        );
    }

    #[test]
    fn strips_thought_block_in_one_chunk() {
        assert_eq!(
            run(&["<thought>internal reasoning</thought>Hello!"]),
            "Hello!",
        );
    }

    #[test]
    fn strips_think_block_split_across_chunks() {
        let chunks = &["<thi", "nk>reason", "ing</thi", "nk>answer"];
        assert_eq!(run(chunks), "answer");
    }

    #[test]
    fn strips_thought_block_split_across_chunks() {
        let chunks = &["<thou", "ght>reason", "</thou", "ght>ok"];
        assert_eq!(run(chunks), "ok");
    }

    #[test]
    fn strips_think_per_token_streaming() {
        let raw = "<think>Let me think step by step.</think>The answer is 7.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "The answer is 7.");
    }

    #[test]
    fn strips_mixed_channel_and_think_tags() {
        let raw = "<|channel>thought planning<channel|>text <think>more reasoning</think> final";
        assert_eq!(run(&[raw]), "text  final");
    }

    #[test]
    fn strips_orphaned_close_think_tag() {
        // Orphaned </think> without open — stripped as standalone sentinel.
        assert_eq!(run(&["Hello!</think>"]), "Hello!");
    }

    #[test]
    fn strips_orphaned_close_thought_tag() {
        assert_eq!(run(&["result</thought> done"]), "result done");
    }

    #[test]
    fn captures_think_block_when_capture_enabled() {
        let mut f = ThoughtFilter::new().with_thinking_capture();
        let out = f.push("<think>step by step reasoning</think>The answer.");
        let out2 = f.flush();
        assert_eq!(format!("{}{}", out, out2), "The answer.");
        let thinking = f.take_thinking();
        assert_eq!(thinking.len(), 1);
        assert_eq!(thinking[0], "step by step reasoning");
    }

    #[test]
    fn captures_thought_block_when_capture_enabled() {
        let mut f = ThoughtFilter::new().with_thinking_capture();
        let out = f.push("<thought>internal monologue</thought>Response.");
        let out2 = f.flush();
        assert_eq!(format!("{}{}", out, out2), "Response.");
        let thinking = f.take_thinking();
        assert_eq!(thinking.len(), 1);
        assert_eq!(thinking[0], "internal monologue");
    }

    #[test]
    fn drops_unclosed_think_block_on_flush() {
        assert_eq!(run(&["<think>never closes"]), "");
    }

    #[test]
    fn text_before_think_block() {
        assert_eq!(
            run(&["Sure! <think>hmm</think>Here you go."]),
            "Sure! Here you go.",
        );
    }

    #[test]
    fn idempotent_on_empty_chunks() {
        // An empty push emits nothing and changes nothing. Note that `push("hi")`
        // now emits "hi" immediately -- it cannot begin a marker, so there is
        // nothing to hold back. The buffer is only non-empty between a partial
        // marker and its resolution.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push(""), "");
        assert_eq!(f.push("hi"), "hi");
        assert_eq!(f.push(""), "");
        // Combined emitted + flush == original input.
        let mut g = ThoughtFilter::new();
        let mut total = g.push("hi");
        total.push_str(&g.flush());
        assert_eq!(total, "hi");
    }
    // ── Holdback behaviour ─────────────────────────────────────────────────
    //
    // The bug these pin: `safe_emit_len` used to withhold a fixed 16 bytes on
    // every push regardless of content, so the visible answer trailed
    // generation by that much and froze the chat mid-word.

    #[test]
    fn ordinary_text_is_emitted_with_no_holdback_on_the_very_first_push() {
        // The two strings the bug report caught frozen on screen. Both are
        // returned whole, before any flush().
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("Just let me kno"), "Just let me kno");

        let mut g = ThoughtFilter::new();
        assert_eq!(
            g.push("The current president of the United States"),
            "The current president of the United States",
        );

        // Down to a single character, which is what a real token stream looks
        // like at the start of a turn.
        let mut h = ThoughtFilter::new();
        assert_eq!(h.push("T"), "T");
    }

    #[test]
    fn every_prefix_of_tag_free_text_is_emitted_as_it_arrives() {
        // Kills the class rather than the instance: after feeding k characters,
        // the filter must have emitted exactly those k characters -- never
        // lagging behind by a lookahead window.
        let raw = "Hello! I can help with reminders, sensors and the news.";
        let mut f = ThoughtFilter::new();
        let mut emitted = String::new();
        for (k, c) in raw.chars().enumerate() {
            emitted.push_str(&f.push(&c.to_string()));
            let expected: String = raw.chars().take(k + 1).collect();
            assert_eq!(emitted, expected, "lagged after {} chars", k + 1);
        }
        assert_eq!(f.flush(), "", "nothing should be left to flush");
    }

    #[test]
    fn holds_back_only_a_suffix_that_could_begin_a_marker() {
        // A bare angle bracket followed by text that no marker starts with is
        // fully resolved, so none of it is withheld.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("a < b"), "a < b");

        // A genuine partial marker is withheld in full, then resolved.
        let mut g = ThoughtFilter::new();
        assert_eq!(g.push("text <thi"), "text ");
        assert_eq!(g.push("nk>hidden</think>shown"), "shown");
    }

    #[test]
    fn holds_the_longest_matching_suffix_not_a_shorter_one() {
        // "<think" is six bytes of a live partial; withholding only the final
        // "<" would emit "think" as text and then fail to match the tag.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("ok <think"), "ok ");
        assert_eq!(f.push(">reasoning</think>done"), "done");
    }

    #[test]
    fn a_complete_sentinel_is_not_withheld_as_a_partial() {
        // `<eos>` is a whole sentinel and no marker extends it, so it is
        // stripped immediately rather than held back forever.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("bye<eos>"), "bye");
    }

    #[test]
    fn a_complete_close_tag_that_extends_into_a_longer_one_resolves_next_push() {
        // "</think>" is complete, but it is also a proper prefix of
        // "</thinking>", so it must be held for exactly one push and then
        // resolved once the next byte proves which one it was.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("done</think>"), "done");
        assert_eq!(f.push(" more"), " more");
    }

    #[test]
    fn multibyte_text_is_never_split_mid_character() {
        // A non-ASCII tail cannot begin a marker (every marker is ASCII), so it
        // must pass straight through rather than being snapped to a boundary.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("Grüße, 世界"), "Grüße, 世界");

        // Fed one char at a time, the multibyte chars must survive intact.
        let raw = "héllo 世界 — ok";
        let mut g = ThoughtFilter::new();
        let mut out = String::new();
        for c in raw.chars() {
            out.push_str(&g.push(&c.to_string()));
        }
        out.push_str(&g.flush());
        assert_eq!(out, raw);
    }

    #[test]
    fn every_marker_is_ascii_so_a_partial_never_starts_mid_character() {
        // `safe_emit_len` relies on this: because every marker is pure ASCII,
        // a suffix matching a marker prefix can never begin inside a multi-byte
        // character. Adding a non-ASCII marker would invalidate that reasoning,
        // and this test is what would catch it.
        for (open, close) in PAIRED_TAGS {
            assert!(open.is_ascii(), "non-ASCII open marker: {open}");
            assert!(close.is_ascii(), "non-ASCII close marker: {close}");
        }
        for sentinel in STANDALONE_SENTINELS {
            assert!(sentinel.is_ascii(), "non-ASCII sentinel: {sentinel}");
        }
    }

    #[test]
    fn holdback_never_exceeds_the_longest_marker() {
        // Whatever the input, the withheld tail is bounded by the longest
        // marker, so the filter cannot accumulate unbounded state in Normal.
        let longest = NORMAL_MARKERS.iter().map(|m| m.len()).max().unwrap();
        for probe in ["plain text", "a < b", "x <thi", "<|channel", "<end_of_tur"] {
            let held = probe.len() - safe_emit_len(probe, &NORMAL_MARKERS);
            assert!(held < longest, "{probe:?} held {held} bytes");
        }
    }

    // ── The `<thinking>` spelling, previously missing from this filter ─────

    #[test]
    fn strips_the_long_thinking_spelling() {
        assert_eq!(run(&["<thinking>reasoning</thinking>Answer."]), "Answer.");
    }

    #[test]
    fn strips_the_long_thinking_spelling_split_across_chunks() {
        assert_eq!(run(&["<thin", "king>hmm</think", "ing>ok"]), "ok");
    }

    #[test]
    fn strips_orphaned_long_close_thinking_tag() {
        assert_eq!(run(&["result</thinking> done"]), "result done");
    }

    #[test]
    fn the_two_thinking_spellings_stay_disjoint() {
        // `<think>` requires `>` at index 6, so it can never match the head of
        // `<thinking>`. Each spelling must close with its own tag.
        assert_eq!(run(&["<think>a</think>X<thinking>b</thinking>Y"]), "XY");
    }
}
