//! Context budget management for constrained LLM inference. On a Jetson Orin Nano with
//! a 7B Q4 model the window is ~8K tokens, so after the system prompt and the response
//! reserve roughly 2,500 tokens (4 chars/token) are usable for history.

const CHARS_PER_TOKEN: usize = 4;
const MIN_USABLE_HISTORY_CHARS: usize = 256;

// ── Compaction profile ──────────────────────────────────────────────────────

/// Per-model compaction parameters derived from the effective context window. Goose
/// auto-compacts at ~80% of its configured context_limit, which leaves almost no
/// headroom on a tiny KV cache (3K Jetson, 8K macOS Metal). Build one with
/// [`CompactionProfile::from_context_window`], from the window the adapter reports.
#[derive(Debug, Clone)]
pub struct CompactionProfile {
    /// Fraction of context at which proactive compaction should trigger (0.0-1.0).
    /// Goose uses this via GOOSE_CONTEXT_LIMIT; we also use it for GIAP-side
    /// budget calculations.
    pub compaction_threshold: f32,
    /// Max tokens to allocate for memory injection into the system prompt.
    pub memory_token_budget: usize,
    /// Max number of memory fragments to inject per turn.
    pub max_memory_fragments: usize,
    /// Max tokens for the full system prompt (base + extras + memories).
    pub system_prompt_budget: usize,
    /// Max tokens to allocate for conversation history injection (per request).
    pub history_token_budget: usize,
    /// Tokens held back for the model's OWN output — reasoning plus answer. Nothing else
    /// reserves this, so the prompt must be budgeted against `context_window - this`: a
    /// thinking block that overruns mid-generation returns ContextLengthExceeded, and
    /// goose then compacts reactively through a path GOOSE_AUTO_COMPACT_THRESHOLD cannot stop.
    pub output_reserve_tokens: usize,
    /// The effective context window this profile was derived from.
    pub context_window_tokens: usize,
    /// The window the PREAMBLE budgets were derived from. Equal to
    /// `context_window_tokens` except where the preamble is re-prefilled locally every
    /// turn, when it is the clamped `ContextGovernor::prompt_window`. Carrying both keeps
    /// the asymmetry a property of the profile — see [`CompactionProfile::for_windows`].
    pub prompt_window_tokens: usize,
}

impl CompactionProfile {
    /// Tokens the whole PROMPT may occupy: the window minus the output reserve. The
    /// ceiling the engine's reported `prompt_tokens` is compared against, so it covers
    /// preamble plus history; for the history budget alone use
    /// [`CompactionProfile::usable_history_tokens`].
    pub fn usable_prompt_tokens(&self) -> usize {
        self.context_window_tokens
            .saturating_sub(self.output_reserve_tokens)
    }

    /// Tokens HISTORY may occupy: the usable prompt space minus the preamble this
    /// profile already promised the system prompt and the memory block. Subtracting
    /// only the output reserve would let history claim a preamble that is still sent.
    /// Saturating, not floored: `turn_trimmer` owns the floor keeping the turn alive.
    pub fn usable_history_tokens(&self) -> usize {
        self.usable_prompt_tokens()
            .saturating_sub(self.system_prompt_budget + self.memory_token_budget)
    }
}

/// One point on the budget curve: the profile that this exact window produces. 8,192
/// and 32,768 are pinned deliberately, since `prompt_window` clamps every local provider
/// to 8,192 and the name heuristic hands qwen and mistral 32,768, so the 8,192..12,288
/// and 32,768..65,536 segments are flat.
struct ProfileAnchor {
    window: usize,
    compaction_threshold: f32,
    memory_token_budget: usize,
    max_memory_fragments: usize,
    system_prompt_budget: usize,
    history_token_budget: usize,
    output_reserve_tokens: usize,
}

/// The budget curve, ascending by window. Below the first anchor and above the
/// last the curve is flat, which reproduces the old function's open-ended first
/// and last tiers.
static PROFILE_ANCHORS: [ProfileAnchor; 6] = [
    // Jetson-class. A thinking block alone measured 306 tokens on this device;
    // 768 covers reasoning plus a real answer, and it is the floor for every
    // smaller window too (invariant 2: the output reserve is never zero).
    ProfileAnchor {
        window: 4_096,
        compaction_threshold: 0.60,
        memory_token_budget: 200,
        max_memory_fragments: 3,
        system_prompt_budget: 1_500,
        history_token_budget: 1_200,
        output_reserve_tokens: 768,
    },
    // The local prompt clamp, and the macOS Metal default.
    ProfileAnchor {
        window: 8_192,
        compaction_threshold: 0.70,
        memory_token_budget: 500,
        max_memory_fragments: 5,
        system_prompt_budget: 3_000,
        history_token_budget: 4_000,
        output_reserve_tokens: 1_024,
    },
    // Old tier-2 ceiling. Same values as 8,192, so 8,192..12,288 is flat.
    ProfileAnchor {
        window: 12_288,
        compaction_threshold: 0.70,
        memory_token_budget: 500,
        max_memory_fragments: 5,
        system_prompt_budget: 3_000,
        history_token_budget: 4_000,
        output_reserve_tokens: 1_024,
    },
    // What the name heuristic gives qwen and mistral.
    ProfileAnchor {
        window: 32_768,
        compaction_threshold: 0.75,
        memory_token_budget: 1_500,
        max_memory_fragments: 10,
        system_prompt_budget: 6_000,
        history_token_budget: 20_000,
        output_reserve_tokens: 2_048,
    },
    // Old tier-3 ceiling. Same values as 32,768, so 32,768..65,536 is flat.
    ProfileAnchor {
        window: 65_536,
        compaction_threshold: 0.75,
        memory_token_budget: 1_500,
        max_memory_fragments: 10,
        system_prompt_budget: 6_000,
        history_token_budget: 20_000,
        output_reserve_tokens: 2_048,
    },
    // Large-context HTTP models (gemma-4 by name heuristic).
    ProfileAnchor {
        window: 128_000,
        compaction_threshold: 0.80,
        memory_token_budget: 4_000,
        max_memory_fragments: 15,
        system_prompt_budget: 10_000,
        history_token_budget: 80_000,
        output_reserve_tokens: 4_096,
    },
];

/// Interpolate a token budget. The `t <= 0.0` and `t >= 1.0` short-circuits make
/// anchor reproduction structural rather than a matter of floating-point luck:
/// landing exactly on an anchor must return that anchor's integer, not something
/// one ULP away that rounds the other direction.
fn lerp_budget(a: usize, b: usize, t: f64) -> usize {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    (a as f64 + (b as f64 - a as f64) * t).round() as usize
}

fn lerp_threshold(a: f32, b: f32, t: f64) -> f32 {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    (a as f64 + (b as f64 - a as f64) * t) as f32
}

impl CompactionProfile {
    /// Derive a compaction profile from the effective context window in tokens:
    /// piecewise-linear over [`PROFILE_ANCHORS`], flat outside them, guarded by
    /// `TIER_FIXTURES`. Every budget is monotonically non-decreasing in the window, and
    /// the interior no longer over-commits: 16,384 sums to 12,729 rather than 29,548.
    pub fn from_context_window(context_tokens: usize) -> Self {
        let first = &PROFILE_ANCHORS[0];
        let last = &PROFILE_ANCHORS[PROFILE_ANCHORS.len() - 1];

        let (lo, hi, t) = if context_tokens <= first.window {
            (first, first, 0.0)
        } else if context_tokens >= last.window {
            (last, last, 0.0)
        } else {
            // The table is ascending and `context_tokens` is strictly inside it,
            // so this always finds an index >= 1.
            let i = PROFILE_ANCHORS
                .iter()
                .position(|a| a.window >= context_tokens)
                .expect("context_tokens is below the last anchor");
            let lo = &PROFILE_ANCHORS[i - 1];
            let hi = &PROFILE_ANCHORS[i];
            let t = (context_tokens - lo.window) as f64 / (hi.window - lo.window) as f64;
            (lo, hi, t)
        };

        Self {
            compaction_threshold: lerp_threshold(
                lo.compaction_threshold,
                hi.compaction_threshold,
                t,
            ),
            memory_token_budget: lerp_budget(lo.memory_token_budget, hi.memory_token_budget, t),
            max_memory_fragments: lerp_budget(lo.max_memory_fragments, hi.max_memory_fragments, t),
            system_prompt_budget: lerp_budget(lo.system_prompt_budget, hi.system_prompt_budget, t),
            history_token_budget: lerp_budget(lo.history_token_budget, hi.history_token_budget, t),
            output_reserve_tokens: lerp_budget(
                lo.output_reserve_tokens,
                hi.output_reserve_tokens,
                t,
            ),
            context_window_tokens: context_tokens,
            prompt_window_tokens: context_tokens,
        }
    }

    /// The asymmetric profile: history budgeted from the full window, preamble from the
    /// clamped prompt-side window, since the preamble is the KV prefix a local provider
    /// re-prefills every turn (PAI-3 invariant 1). What the clamp frees goes to history,
    /// so the total is unchanged; `prompt_window >= context_window` changes nothing.
    pub fn for_windows(context_window: usize, prompt_window: usize) -> Self {
        let full = Self::from_context_window(context_window);
        if prompt_window >= context_window {
            return full;
        }
        let capped = Self::from_context_window(prompt_window);
        let freed = (full.system_prompt_budget + full.memory_token_budget)
            .saturating_sub(capped.system_prompt_budget + capped.memory_token_budget);
        Self {
            memory_token_budget: capped.memory_token_budget,
            max_memory_fragments: capped.max_memory_fragments,
            system_prompt_budget: capped.system_prompt_budget,
            history_token_budget: full.history_token_budget + freed,
            prompt_window_tokens: prompt_window,
            ..full
        }
    }

    /// The same profile with part of the history budget held back for a second agent
    /// claiming the same window (PAI-6 P4). Only `history_token_budget` moves: shrinking
    /// the preamble would move the KV prefix, a 3.7 s re-prefill on the Orin. Reserved out
    /// of `min(declared, usable)` so both claims fit; a fraction outside `0.0..=1.0` takes all.
    pub fn with_history_reserved(&self, fraction: f32) -> Self {
        let fraction = if fraction.is_finite() && (0.0..=1.0).contains(&fraction) {
            fraction
        } else {
            1.0
        };
        if fraction <= 0.0 {
            return self.clone();
        }
        let claimable = self.history_token_budget.min(self.usable_history_tokens());
        // `ceil`, so an awkward ratio errs toward reserving MORE.
        let reserved = (claimable as f64 * fraction as f64).ceil() as usize;
        Self {
            history_token_budget: claimable.saturating_sub(reserved),
            ..self.clone()
        }
    }

    /// Whether the system prompt should use a compact format. Reads the PROMPT window,
    /// not the context window: this is a preamble decision, and a 32K KV cache must not
    /// buy a local provider a more verbose prefix. A hard step at 12288, deliberately
    /// not interpolated: this is a format switch, not a budget.
    pub fn use_compact_prompt(&self) -> bool {
        self.prompt_window_tokens <= 12288
    }
}

// ── Reasoning effort (PAI-5 P4) ─────────────────────────────────────────────

/// How much room the model is told it may spend thinking before it answers: a
/// preference with three settings, not a token count. It chooses a share of the budget
/// [`reasoning_budget_tokens`] derives. `reasoning_effort_strings_agree_with_settings`
/// pins the string forms against `user_data::domain::settings::REASONING_EFFORTS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// Least thinking. The on-device default: reasoning tokens are decode
    /// tokens, and decode on the Orin is memory-bandwidth-bound at roughly
    /// `102 / model_GB` tok/s, so every thinking token is silence before the
    /// answer starts.
    Brief,
    /// The middle setting.
    Balanced,
    /// Most thinking. The right choice on an HTTP provider, where the tokens
    /// are both cheap and fast.
    Thorough,
}

impl ReasoningEffort {
    /// Every variant, in increasing order of spend. Used by the agreement test
    /// and by anything that needs to enumerate the setting.
    pub const ALL: &'static [ReasoningEffort] = &[
        ReasoningEffort::Brief,
        ReasoningEffort::Balanced,
        ReasoningEffort::Thorough,
    ];

    /// Parse the stored setting string. An unrecognised value falls back to
    /// [`ReasoningEffort::Brief`], the SMALLEST budget, so a typo that reached the
    /// store cannot buy a bigger think than anybody chose. The rejection is at the
    /// edge: `PUT /api/v1/settings` returns 422 outside `REASONING_EFFORTS`.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "balanced" => ReasoningEffort::Balanced,
            "thorough" => ReasoningEffort::Thorough,
            "brief" => ReasoningEffort::Brief,
            other => {
                if !other.is_empty() {
                    tracing::warn!(
                        "unrecognised reasoning_effort {other:?} — falling back to \"brief\""
                    );
                }
                ReasoningEffort::Brief
            }
        }
    }

    /// The stored string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReasoningEffort::Brief => "brief",
            ReasoningEffort::Balanced => "balanced",
            ReasoningEffort::Thorough => "thorough",
        }
    }
}

/// The floor under any thinking budget. Never zero: that is `thinking_mode = "off"`'s
/// job, which removes the whole `<thinking>` section. A zero budget inside a section
/// that still asks the model to reason is a contradiction a small model resolves at
/// random.
const MIN_REASONING_BUDGET_TOKENS: usize = 32;

/// Tokens a `<thinking>` block may run to, on this profile, at this effort. Derived
/// from [`CompactionProfile::output_reserve_tokens`], not the window: reasoning tokens
/// are decoded into the same reserve as the answer. Shares are 1/8, 1/4 and 1/2, so
/// the answer keeps at least half the reserve at every effort (PAI-5 section 3.3).
pub fn reasoning_budget_tokens(profile: &CompactionProfile, effort: ReasoningEffort) -> usize {
    let reserve = profile.output_reserve_tokens;
    let share = match effort {
        ReasoningEffort::Brief => reserve / 8,
        ReasoningEffort::Balanced => reserve / 4,
        ReasoningEffort::Thorough => reserve / 2,
    };
    share.max(MIN_REASONING_BUDGET_TOKENS)
}

// ── PAI-5 P5: a reserve derived from what reasoning actually costs ──────────

/// Fewest observations before this pond's own behaviour may move the reserve. Below
/// this the anchor stands: a few short thinking blocks are not evidence that reasoning
/// is cheap here, and the anchor came from a measured mid-generation overrun on the
/// Orin, so this must never be talked below it.
pub const MIN_REASONING_SAMPLES: usize = 20;

/// The step a derived reserve is rounded up to. `turn_profile` runs per turn, so an
/// unquantised reserve would move the trim point on most turns; it moves where history
/// is cut, not the rendered preamble, which [`reasoning_budget_words`] owns. Guarded by
/// `a_growing_sample_set_does_not_move_the_reserve_every_turn`.
pub const RESERVE_QUANTUM_TOKENS: usize = 256;

/// The most of the window the output reserve may take from observation alone.
///
/// Without it a verbose model on a small window leaves no room for history. The
/// anchor may exceed this ([`observed_output_reserve`]): a measurement beats a policy.
pub const MAX_OBSERVED_RESERVE_SHARE: f32 = 0.25;

/// The percentile of observed reasoning cost the reserve is sized to. Not the mean:
/// half the turns would overrun, and an overrun is `ContextLengthExceeded` mid-generation,
/// which goose answers by compacting through a path GIAP's own threshold cannot disable.
/// Not the max either: one pathological turn would tax every later turn's history.
const REASONING_PERCENTILE: f64 = 0.95;

/// Size the output reserve from observed `reasoning_tokens` (PAI-5 P5). **Pass only
/// measured turns**: a `None` folded in as `0` votes for a smaller reserve. Order is
/// anchor if under [`MIN_REASONING_SAMPLES`], else `2 * p95` (Thorough may take half
/// the reserve) quantised up, clamped to the share, then floored at the anchor last.
pub fn observed_output_reserve(samples: &[u32], anchor: usize, window: usize) -> usize {
    if samples.len() < MIN_REASONING_SAMPLES {
        return anchor;
    }

    let mut sorted: Vec<u32> = samples.to_vec();
    sorted.sort_unstable();
    // Nearest-rank: the smallest observation at or above the percentile. On 20
    // samples that is the 19th, so one outlier does not set the reserve and the
    // top 5% still does.
    let rank = ((sorted.len() as f64) * REASONING_PERCENTILE).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    let p95 = sorted[index] as usize;

    let needed = p95.saturating_mul(2);
    let quantised = needed
        .div_ceil(RESERVE_QUANTUM_TOKENS)
        .saturating_mul(RESERVE_QUANTUM_TOKENS);

    let ceiling = ((window as f32) * MAX_OBSERVED_RESERVE_SHARE) as usize;
    quantised.min(ceiling).max(anchor)
}

/// The window the compact tier is sampled at when only `compact_prompt` is known.
///
/// `ContextGovernor::prompt_window` clamps every local provider to exactly 8,192,
/// so this is the most-executed window and a regression test pins the curve at it.
const COMPACT_TIER_SAMPLE_WINDOW: usize = 8_192;

/// The window the roomy tier is sampled at when only `compact_prompt` is known.
///
/// 32,768 is what the model-name heuristic hands to qwen and mistral, and the
/// profile curve is flat from there to 65,536.
const ROOMY_TIER_SAMPLE_WINDOW: usize = 32_768;

/// Words a `<thinking>` block may run to — the prompt-side form of
/// [`reasoning_budget_tokens`]. Words, not tokens: `budget_as_words` rounds DOWN,
/// keeping the ask inside the budget. Takes a bool because the prompt renderer sees
/// The number of words to ask the model for, from a token budget.
///
/// Models take a word count far more reliably than a token count, and roughly
/// four tokens cover three English words. Rounded down, so the ask lands inside
/// the budget rather than at it.
///
/// Lived in `resummarisation` until that module went with the re-summarisation
/// pass it gated. Its only remaining caller is the reasoning budget below,
/// which is a prompt concern and was always the odd one out there.
pub fn budget_as_words(budget_tokens: usize) -> usize {
    budget_tokens * 3 / 4
}

/// only `PromptState::compact_prompt`, so the curve is sampled at the two windows above.
pub fn reasoning_budget_words(effort: ReasoningEffort, compact_prompt: bool) -> usize {
    let window = if compact_prompt {
        COMPACT_TIER_SAMPLE_WINDOW
    } else {
        ROOMY_TIER_SAMPLE_WINDOW
    };
    let profile = CompactionProfile::from_context_window(window);
    budget_as_words(reasoning_budget_tokens(&profile, effort))
}

/// Available history budget in characters, after system prompt and tool schema overhead.
///
/// An upper bound on injected conversation history. Never returns less than
/// [`MIN_USABLE_HISTORY_CHARS`], so the most recent turn survives a large overhead.
pub fn available_history_chars(
    profile: &CompactionProfile,
    system_prompt_chars: usize,
    tool_schema_chars: usize,
) -> usize {
    let total_budget_chars = profile.history_token_budget * CHARS_PER_TOKEN;
    let overhead = system_prompt_chars + tool_schema_chars;
    total_budget_chars
        .saturating_sub(overhead)
        .max(MIN_USABLE_HISTORY_CHARS)
}

/// Maximum assistant tool-output size kept verbatim in history. **Bytes**: every use
/// compares against `String::len()` or feeds `truncate_at_byte_budget`. Distinct from
/// `shared::services::chat.rs`'s `TOOL_RESULT_MAX_CHARS`, which is genuinely chars and
/// governs the NDJSON stream contract rather than the model's context.
pub const TOOL_RESULT_MAX_BYTES: usize = 1_500;

/// Shrink an oversized tool result to `max_chars`-ish, keeping BOTH ends: a tool
/// result's tail carries the totals and closing summary, so ~60% head plus ~40% tail
/// keeps the conclusion. Returns `None` when `text` already fits. Splits on char
/// boundaries, and may exceed `max_chars` by the length of the truncation marker.
pub fn truncate_head_tail(text: &str, max_chars: usize) -> Option<String> {
    if text.len() <= max_chars {
        return None;
    }
    // Degenerate budgets: a head-only cut is all that fits.
    if max_chars < 64 {
        let mut end = max_chars.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return Some(format!(
            "{}\n[... truncated {} chars ...]",
            &text[..end],
            text.len() - end
        ));
    }

    let head_len = max_chars * 3 / 5;
    let tail_len = max_chars - head_len;

    let mut head_end = head_len;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(tail_len);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    // A pathological multi-byte boundary walk could cross the head — then there
    // is nothing left to keep from the tail.
    if tail_start <= head_end {
        return Some(format!(
            "{}\n[... truncated {} chars ...]",
            &text[..head_end],
            text.len() - head_end
        ));
    }

    let dropped = tail_start - head_end;
    Some(format!(
        "{}\n[... truncated {dropped} chars ...]\n{}",
        &text[..head_end],
        &text[tail_start..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate_head_tail (C3) ──────────────────────────────────────────

    #[test]
    fn text_within_budget_is_left_alone() {
        assert!(truncate_head_tail("short", TOOL_RESULT_MAX_BYTES).is_none());
        let exact = "x".repeat(TOOL_RESULT_MAX_BYTES);
        assert!(truncate_head_tail(&exact, TOOL_RESULT_MAX_BYTES).is_none());
    }

    /// The point of head+tail: the CONCLUSION at the end survives, which a
    /// head-only truncation would have thrown away.
    #[test]
    fn both_ends_survive_and_the_marker_states_the_loss() {
        let text = format!("HEAD-MARKER{}TAIL-MARKER", "x".repeat(50_000));
        let out = truncate_head_tail(&text, TOOL_RESULT_MAX_BYTES).unwrap();
        assert!(out.starts_with("HEAD-MARKER"), "{}", &out[..40]);
        assert!(out.ends_with("TAIL-MARKER"), "{}", &out[out.len() - 40..]);
        assert!(out.contains("[... truncated "));
        // Far smaller than the original, and close to the budget.
        assert!(out.len() < TOOL_RESULT_MAX_BYTES + 64, "len {}", out.len());
        // The stated loss is accurate.
        let dropped: usize = out
            .split("[... truncated ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert_eq!(
            dropped,
            text.len() - (out.len() - format!("\n[... truncated {dropped} chars ...]\n").len())
        );
    }

    #[test]
    fn multibyte_text_is_never_split_mid_char() {
        // Every char is 4 bytes, so naive byte slicing would panic.
        let text = "\u{1F600}".repeat(2_000);
        let out = truncate_head_tail(&text, TOOL_RESULT_MAX_BYTES).unwrap();
        assert!(out.contains("[... truncated "));
        // Round-trips as valid UTF-8 with no replacement chars introduced.
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn a_tiny_budget_degrades_to_a_head_cut() {
        let text = "y".repeat(500);
        let out = truncate_head_tail(&text, 10).unwrap();
        assert!(out.starts_with("yyyyyyyyyy"));
        assert!(out.contains("[... truncated 490 chars ...]"));
    }

    // ── CompactionProfile tests ─────────────────────────────────────────

    #[test]
    fn compaction_profile_jetson_3k() {
        let p = CompactionProfile::from_context_window(3072);
        assert!((p.compaction_threshold - 0.60).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 200);
        assert_eq!(p.max_memory_fragments, 3);
        assert_eq!(p.system_prompt_budget, 1500);
        assert_eq!(p.history_token_budget, 1200);
        assert!(p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_macos_8k() {
        let p = CompactionProfile::from_context_window(8192);
        assert!((p.compaction_threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 500);
        assert_eq!(p.max_memory_fragments, 5);
        assert_eq!(p.system_prompt_budget, 3000);
        assert_eq!(p.history_token_budget, 4000);
        assert!(p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_32k() {
        let p = CompactionProfile::from_context_window(32768);
        assert!((p.compaction_threshold - 0.75).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 1500);
        assert_eq!(p.max_memory_fragments, 10);
        assert_eq!(p.system_prompt_budget, 6000);
        assert_eq!(p.history_token_budget, 20000);
        assert!(!p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_128k() {
        let p = CompactionProfile::from_context_window(128_000);
        assert!((p.compaction_threshold - 0.80).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 4000);
        assert_eq!(p.max_memory_fragments, 15);
        assert_eq!(p.system_prompt_budget, 10000);
        assert_eq!(p.history_token_budget, 80000);
        assert!(!p.use_compact_prompt());
    }

    #[test]
    fn available_history_chars_subtracts_overhead() {
        let profile = CompactionProfile::from_context_window(8192);
        // 4000 tokens * 4 chars/token = 16000 chars total budget
        let avail = available_history_chars(&profile, 1000, 500);
        assert_eq!(avail, 16_000 - 1500);
    }

    #[test]
    fn available_history_chars_clamps_to_min_when_overhead_exceeds_budget() {
        let profile = CompactionProfile::from_context_window(3072);
        // 1200 * 4 = 4800 budget, overhead 10000 → should clamp to MIN_USABLE_HISTORY_CHARS
        let avail = available_history_chars(&profile, 6000, 4000);
        assert_eq!(avail, MIN_USABLE_HISTORY_CHARS);
    }

    #[test]
    fn compaction_profile_boundary_4096() {
        // 4096 is the upper boundary of the Jetson tier
        let p = CompactionProfile::from_context_window(4096);
        assert!((p.compaction_threshold - 0.60).abs() < f32::EPSILON);
        assert_eq!(p.max_memory_fragments, 3);
    }

    #[test]
    fn compaction_profile_boundary_12288() {
        // 12288 is the upper boundary of the macOS tier
        let p = CompactionProfile::from_context_window(12288);
        assert!((p.compaction_threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(p.max_memory_fragments, 5);
    }

    #[test]
    fn compaction_profile_stores_context_window() {
        let p = CompactionProfile::from_context_window(8192);
        assert_eq!(p.context_window_tokens, 8192);
    }
    // ── P4: the continuous profile curve, with the tiers as fixtures ─────

    /// The four discrete tiers, verbatim as they were before P4. Kept as the
    /// reference implementation so the properties below are checked against the
    /// real prior behaviour rather than against numbers somebody transcribed.
    fn tiers_before_p4(context_tokens: usize) -> (f32, usize, usize, usize, usize, usize) {
        if context_tokens <= 4096 {
            (0.60, 200, 3, 1500, 1200, 768)
        } else if context_tokens <= 12288 {
            (0.70, 500, 5, 3000, 4000, 1024)
        } else if context_tokens <= 65536 {
            (0.75, 1500, 10, 6000, 20000, 2048)
        } else {
            (0.80, 4000, 15, 10000, 80000, 4096)
        }
    }

    fn budget_sum(p: &CompactionProfile) -> usize {
        p.output_reserve_tokens
            + p.system_prompt_budget
            + p.memory_token_budget
            + p.history_token_budget
    }

    /// Every window the discrete tiers were pinned at, with the values they produced:
    /// the machinery changed, these answers must not. Wider than the four tier
    /// boundaries because 8,192 is what `ContextGovernor::prompt_window` hands every
    /// local provider and 32,768 is what the name heuristic hands qwen and mistral.
    const TIER_FIXTURES: &[(usize, f32, usize, usize, usize, usize, usize)] = &[
        // window,   threshold, memory, frags, system, history, reserve
        (0, 0.60, 200, 3, 1500, 1200, 768),
        (3_072, 0.60, 200, 3, 1500, 1200, 768),
        (4_096, 0.60, 200, 3, 1500, 1200, 768),
        (8_192, 0.70, 500, 5, 3000, 4000, 1024),
        (12_288, 0.70, 500, 5, 3000, 4000, 1024),
        (32_768, 0.75, 1500, 10, 6000, 20000, 2048),
        (65_536, 0.75, 1500, 10, 6000, 20000, 2048),
        (128_000, 0.80, 4000, 15, 10000, 80000, 4096),
        (200_000, 0.80, 4000, 15, 10000, 80000, 4096),
    ];

    #[test]
    fn the_curve_reproduces_every_tier_fixture_exactly() {
        for &(w, thr, mem, frags, sys, hist, reserve) in TIER_FIXTURES {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                (p.compaction_threshold - thr).abs() < 1e-6,
                "window {w}: compaction_threshold {} != {thr}",
                p.compaction_threshold
            );
            assert_eq!(
                p.memory_token_budget, mem,
                "window {w}: memory_token_budget"
            );
            assert_eq!(
                p.max_memory_fragments, frags,
                "window {w}: max_memory_fragments"
            );
            assert_eq!(
                p.system_prompt_budget, sys,
                "window {w}: system_prompt_budget"
            );
            assert_eq!(
                p.history_token_budget, hist,
                "window {w}: history_token_budget"
            );
            assert_eq!(
                p.output_reserve_tokens, reserve,
                "window {w}: output_reserve_tokens"
            );
            assert_eq!(
                p.context_window_tokens, w,
                "window {w}: context_window_tokens"
            );
        }
    }

    /// The safety property. The curve is allowed to differ from the tiers in the
    /// interior - that is the point - but it may never promise MORE budget than
    /// the tiers already did, at any window. Checked against the old function
    /// itself, at every integer window in the range that matters.
    #[test]
    fn the_curve_never_promises_more_budget_than_the_tiers_did() {
        for w in 4_096..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            let (_, mem, _, sys, hist, reserve) = tiers_before_p4(w);
            let before = mem + sys + hist + reserve;
            let after = budget_sum(&p);
            assert!(
                after <= before,
                "window {w}: curve promises {after} tokens, tiers promised {before}"
            );
        }
    }

    /// The tiers over-committed at every window that was not a boundary. The
    /// curve may still over-commit - the 8,192 fixture itself sums to 8,524,
    /// and correcting THAT is P5's asymmetric budgeting, not this phase - but
    /// wherever it does, the tiers did too, and by more.
    #[test]
    fn the_curve_over_commits_strictly_less_often_than_the_tiers() {
        let mut curve_violations = 0usize;
        let mut tier_violations = 0usize;
        for w in 4_096..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            let (_, mem, _, sys, hist, reserve) = tiers_before_p4(w);
            let tier_sum = mem + sys + hist + reserve;
            if budget_sum(&p) > w {
                curve_violations += 1;
                assert!(
                    tier_sum > w,
                    "window {w}: the curve over-commits where the tiers did not"
                );
            }
            if tier_sum > w {
                tier_violations += 1;
            }
        }
        assert_eq!(curve_violations, 2_116, "curve over-commitment count");
        assert_eq!(tier_violations, 54_245, "tier over-commitment count");
    }

    /// The Orin's real operating point, which no tier fixture covers: the local
    /// model registry pins n_ctx at 16,384 there. The tiers declared 29,548 tokens
    /// of budget against that window, 1.8x over, with only `turn_trimmer`'s history
    /// clamp between it and a mid-generation context overrun.
    #[test]
    fn the_orins_pinned_window_stops_promising_more_than_the_window_holds() {
        let p = CompactionProfile::from_context_window(16_384);
        let sum = budget_sum(&p);
        assert!(
            sum <= p.context_window_tokens,
            "budgets sum to {sum} against a {}-token window",
            p.context_window_tokens
        );
        assert_eq!(sum, 12_729, "the Orin budget sum moved; re-derive it");
        assert_eq!(p.output_reserve_tokens, 1_229);
        assert_eq!(p.system_prompt_budget, 3_600);
        assert_eq!(p.memory_token_budget, 700);
        assert_eq!(p.max_memory_fragments, 6);
        assert_eq!(p.history_token_budget, 7_200);

        // What it was before, for the record.
        let (_, mem, _, sys, hist, reserve) = tiers_before_p4(16_384);
        assert_eq!(mem + sys + hist + reserve, 29_548);
    }

    /// The stated purpose of the phase, as an assertion.
    #[test]
    fn a_24k_model_and_a_64k_model_no_longer_share_a_bucket() {
        let k24 = CompactionProfile::from_context_window(24_576);
        let k64 = CompactionProfile::from_context_window(65_536);
        assert!(
            k24.history_token_budget < k64.history_token_budget,
            "24K got {} history tokens, 64K got {}",
            k24.history_token_budget,
            k64.history_token_budget
        );
        assert_eq!(k24.history_token_budget, 13_600);
        assert_eq!(k64.history_token_budget, 20_000);
    }

    /// Invariant 2 of the design: the output reserve is never zero and never
    /// optional. It is the only thing between a long thinking block and a
    /// mid-generation context overrun.
    #[test]
    fn the_output_reserve_is_never_below_its_floor_at_any_window() {
        for w in [0, 1, 512, 3_072, 4_096, 6_000, 8_192, 100_000, 1_000_000] {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                p.output_reserve_tokens >= 768,
                "window {w}: reserve {}",
                p.output_reserve_tokens
            );
        }
    }

    /// A bigger window must never buy less of anything. Without this, raising
    /// `context_window_override` by one token could cost the user history.
    #[test]
    fn every_budget_is_non_decreasing_in_the_window() {
        let mut prev = CompactionProfile::from_context_window(4_096);
        for w in 4_097..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                p.compaction_threshold >= prev.compaction_threshold,
                "threshold at {w}"
            );
            assert!(
                p.memory_token_budget >= prev.memory_token_budget,
                "memory at {w}"
            );
            assert!(
                p.max_memory_fragments >= prev.max_memory_fragments,
                "fragments at {w}"
            );
            assert!(
                p.system_prompt_budget >= prev.system_prompt_budget,
                "system at {w}"
            );
            assert!(
                p.history_token_budget >= prev.history_token_budget,
                "history at {w}"
            );
            assert!(
                p.output_reserve_tokens >= prev.output_reserve_tokens,
                "reserve at {w}"
            );
            prev = p;
        }
    }

    /// `use_compact_prompt` is a bool, so it has no continuous form. It stays a
    /// hard step at 12,288 and must not be quietly folded into the curve.
    #[test]
    fn use_compact_prompt_is_still_a_hard_step_at_12288() {
        assert!(CompactionProfile::from_context_window(12_288).use_compact_prompt());
        assert!(!CompactionProfile::from_context_window(12_289).use_compact_prompt());
    }

    // ── P5: asymmetric budgeting - preamble capped, working set scaled ───

    /// A local provider's prompt window is clamped to 8,192 whatever the KV cache
    /// holds, so quadrupling the window from 8,192 to 32,768 must buy HISTORY and
    /// nothing else: the preamble is the KV prefix, re-prefilled every turn, so
    /// growing it grows TTFT permanently (PAI-3 invariant 1).
    #[test]
    fn growing_the_window_buys_history_and_never_preamble() {
        let small = CompactionProfile::for_windows(8_192, 8_192);
        let big = CompactionProfile::for_windows(32_768, 8_192);

        assert_eq!(
            big.system_prompt_budget, small.system_prompt_budget,
            "a 4x window bought a bigger system prompt: {} vs {}",
            big.system_prompt_budget, small.system_prompt_budget
        );
        assert_eq!(
            big.memory_token_budget, small.memory_token_budget,
            "a 4x window bought a bigger memory block: {} vs {}",
            big.memory_token_budget, small.memory_token_budget
        );
        assert_eq!(
            big.max_memory_fragments, small.max_memory_fragments,
            "a 4x window bought more memory fragments"
        );
        assert_eq!(
            big.use_compact_prompt(),
            small.use_compact_prompt(),
            "a 4x window flipped the prompt to the verbose tier"
        );

        assert!(
            big.history_token_budget > small.history_token_budget,
            "a 4x window bought no extra history: {} vs {}",
            big.history_token_budget,
            small.history_token_budget
        );
        // The exact numbers, so a silent re-tune is visible in the diff.
        assert_eq!(small.history_token_budget, 4_000);
        assert_eq!(big.history_token_budget, 24_000);
        assert_eq!(big.system_prompt_budget, 3_000);
        assert_eq!(big.memory_token_budget, 500);
    }

    /// The redistribution is exactly that - a redistribution. Nothing is
    /// invented, so P4's "never promises more budget than the tiers did"
    /// property survives untouched.
    #[test]
    fn capping_the_preamble_moves_tokens_to_history_and_creates_none() {
        for window in [8_193usize, 12_288, 16_384, 32_768, 65_536, 128_000] {
            let symmetric = CompactionProfile::from_context_window(window);
            let asymmetric = CompactionProfile::for_windows(window, 8_192);
            assert_eq!(
                budget_sum(&asymmetric),
                budget_sum(&symmetric),
                "window {window}: the split changed the TOTAL budget"
            );
            assert!(
                asymmetric.history_token_budget >= symmetric.history_token_budget,
                "window {window}: capping the preamble cost history"
            );
        }
    }

    /// The symmetric case must be bit-identical to the old constructor, or every
    /// HTTP provider silently re-tunes. `prompt_window >= context_window` is the
    /// path they take.
    #[test]
    fn an_unclamped_prompt_window_reproduces_from_context_window_exactly() {
        for &(w, ..) in TIER_FIXTURES {
            let a = CompactionProfile::from_context_window(w);
            for prompt in [w, w + 1, w * 2 + 1] {
                let b = CompactionProfile::for_windows(w, prompt);
                assert_eq!(budget_sum(&a), budget_sum(&b), "window {w} prompt {prompt}");
                assert_eq!(a.history_token_budget, b.history_token_budget, "window {w}");
                assert_eq!(a.system_prompt_budget, b.system_prompt_budget, "window {w}");
                assert_eq!(a.memory_token_budget, b.memory_token_budget, "window {w}");
                assert_eq!(a.max_memory_fragments, b.max_memory_fragments, "window {w}");
                assert_eq!(a.prompt_window_tokens, b.prompt_window_tokens, "window {w}");
                assert_eq!(a.use_compact_prompt(), b.use_compact_prompt(), "window {w}");
            }
        }
    }

    /// Invariant 1 as a range property rather than a spot check: across every
    /// window a local provider can resolve to, the preamble allowance is frozen
    /// at the clamp's values.
    #[test]
    fn the_preamble_allowance_is_flat_across_every_clamped_window() {
        let clamp = 8_192usize;
        let reference = CompactionProfile::from_context_window(clamp);
        for window in (clamp..=200_000).step_by(97) {
            let p = CompactionProfile::for_windows(window, clamp.min(window));
            assert_eq!(
                p.system_prompt_budget, reference.system_prompt_budget,
                "window {window}"
            );
            assert_eq!(
                p.memory_token_budget, reference.memory_token_budget,
                "window {window}"
            );
            assert_eq!(
                p.max_memory_fragments, reference.max_memory_fragments,
                "window {window}"
            );
            assert!(p.use_compact_prompt(), "window {window}");
        }
    }

    /// The over-commitment P4 deferred here. At 8,192 the declared budgets sum
    /// to 8,524 against an 8,192-token window, and the old history clamp
    /// (`usable_prompt_tokens`, window minus reserve only) let history claim
    /// 7,168 of that on top of a preamble that was still going to be sent.
    #[test]
    fn the_history_ceiling_subtracts_the_preamble_the_prompt_ceiling_does_not() {
        let p = CompactionProfile::from_context_window(8_192);
        // Unchanged: this is the ceiling the engine's own prompt_tokens is
        // measured against, so it must still cover preamble plus history.
        assert_eq!(p.usable_prompt_tokens(), 8_192 - 1_024);
        // New: history alone may not claim the preamble's room.
        assert_eq!(p.usable_history_tokens(), 7_168 - 3_000 - 500);
        assert!(
            p.usable_history_tokens() < p.history_token_budget,
            "the declared 4,000-token history budget was payable after all"
        );
    }

    /// A preamble allowance larger than the whole window leaves history nothing,
    /// and must saturate rather than wrap. `turn_trimmer` owns the floor that
    /// keeps the current turn alive.
    #[test]
    fn a_preamble_bigger_than_the_window_leaves_zero_history_room() {
        let p = CompactionProfile::from_context_window(1_024);
        assert_eq!(p.system_prompt_budget, 1_500);
        assert_eq!(p.usable_prompt_tokens(), 256);
        assert_eq!(p.usable_history_tokens(), 0);
    }

    // ── Reasoning effort (PAI-5 P4) ─────────────────────────────────────────

    /// Bidirectional, in the shape `VERDICTS` uses: a fourth effort added to
    /// either the enum or the settings constant alone fails here.
    #[test]
    fn reasoning_effort_strings_agree_with_settings() {
        use crate::user_data::domain::settings::REASONING_EFFORTS;

        let from_enum: Vec<&str> = ReasoningEffort::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            from_enum, REASONING_EFFORTS,
            "ReasoningEffort::ALL and settings::REASONING_EFFORTS have diverged"
        );
        for s in REASONING_EFFORTS {
            assert_eq!(
                ReasoningEffort::parse(s).as_str(),
                *s,
                "round trip failed for {s:?}"
            );
        }
    }

    /// The fallback direction is the whole safety argument: an unrecognised
    /// value must buy the SMALLEST think, never the largest.
    #[test]
    fn an_unrecognised_reasoning_effort_narrows_to_brief() {
        for bad in ["", "Thorough", "maximum", "high", "off", "true"] {
            assert_eq!(
                ReasoningEffort::parse(bad),
                ReasoningEffort::Brief,
                "{bad:?} must fall back to the smallest budget"
            );
        }
        // And the fallback really is the smallest, not merely a named variant.
        let p = CompactionProfile::from_context_window(8_192);
        let brief = reasoning_budget_tokens(&p, ReasoningEffort::Brief);
        for e in ReasoningEffort::ALL {
            assert!(
                brief <= reasoning_budget_tokens(&p, *e),
                "Brief is not the smallest budget — the parse fallback widens scope"
            );
        }
    }

    /// The budget is a share of the OUTPUT RESERVE, so it tracks the window and
    /// the device rather than a number somebody typed. Monotonic in effort,
    /// monotonic in window, and it always leaves the answer at least half the
    /// reserve.
    #[test]
    fn the_reasoning_budget_is_a_share_of_the_output_reserve() {
        for w in [3_072usize, 4_096, 8_192, 12_288, 32_768, 65_536, 128_000] {
            let p = CompactionProfile::from_context_window(w);
            let b = reasoning_budget_tokens(&p, ReasoningEffort::Brief);
            let m = reasoning_budget_tokens(&p, ReasoningEffort::Balanced);
            let t = reasoning_budget_tokens(&p, ReasoningEffort::Thorough);
            assert!(b <= m && m <= t, "window {w}: not monotonic in effort");
            assert!(
                b >= MIN_REASONING_BUDGET_TOKENS,
                "window {w}: budget fell to zero — that is thinking_mode's job"
            );
            assert!(
                t * 2 <= p.output_reserve_tokens,
                "window {w}: thinking may claim more than half the answer's room"
            );
        }
        // Bigger window, bigger think — the property section 3.3 asks for.
        let small = CompactionProfile::from_context_window(8_192);
        let big = CompactionProfile::from_context_window(65_536);
        assert!(
            reasoning_budget_tokens(&small, ReasoningEffort::Brief)
                < reasoning_budget_tokens(&big, ReasoningEffort::Brief)
        );
    }

    /// The prompt-side sample must equal the full evaluation at the two windows
    /// it claims to stand for. If the sample constants ever drift off the
    /// profile curve this is what says so.
    #[test]
    fn the_two_point_sample_matches_the_profile_it_stands_for() {
        for (compact, window) in [(true, 8_192usize), (false, 32_768)] {
            let p = CompactionProfile::from_context_window(window);
            assert_eq!(p.use_compact_prompt(), compact, "window {window}");
            for e in ReasoningEffort::ALL {
                assert_eq!(
                    reasoning_budget_words(*e, compact),
                    budget_as_words(reasoning_budget_tokens(&p, *e)),
                    "window {window}, effort {}",
                    e.as_str()
                );
            }
        }
    }

    /// Three efforts, three DIFFERENT word counts, on both tiers — and never
    /// zero. A budget that collapsed to one number would render an identical
    /// prompt at every effort and the setting would do nothing.
    #[test]
    fn every_effort_renders_a_distinct_non_zero_word_budget() {
        for compact in [true, false] {
            let words: Vec<usize> = ReasoningEffort::ALL
                .iter()
                .map(|e| reasoning_budget_words(*e, compact))
                .collect();
            assert!(
                words.iter().all(|w| *w > 0),
                "compact={compact}: a zero word budget reached the prompt"
            );
            assert!(
                words[0] < words[1] && words[1] < words[2],
                "compact={compact}: efforts do not produce distinct budgets: {words:?}"
            );
        }
        // The tight tier must ask for less than the roomy one at equal effort.
        for e in ReasoningEffort::ALL {
            assert!(
                reasoning_budget_words(*e, true) < reasoning_budget_words(*e, false),
                "effort {}: the on-device tier is not cheaper",
                e.as_str()
            );
        }
    }

    // ── PAI-6 P4: reserving history for a live subagent ─────────────────────

    /// Everything a reservation must NOT move. Shrinking the preamble allowances
    /// moves the KV prefix, costing a full re-prefill (3.7 s on the Orin) on the
    /// parent's next turn. Asserted field by field so a new `CompactionProfile`
    /// field is caught by `the_reservation_covers_every_field_of_the_profile`.
    #[test]
    fn a_reservation_takes_working_set_and_never_preamble() {
        let base = CompactionProfile::for_windows(32_768, 8_192);
        let reserved = base.with_history_reserved(0.3);

        assert!(
            reserved.history_token_budget < base.history_token_budget,
            "the history budget did not shrink: {} vs {}",
            reserved.history_token_budget,
            base.history_token_budget
        );
        assert_eq!(
            reserved.system_prompt_budget, base.system_prompt_budget,
            "the system prompt allowance moved, so the preamble is rebuilt and the KV prefix \
             moves with it"
        );
        assert_eq!(
            reserved.memory_token_budget, base.memory_token_budget,
            "the memory allowance moved"
        );
        assert_eq!(
            reserved.max_memory_fragments, base.max_memory_fragments,
            "the fragment count moved"
        );
        assert_eq!(
            reserved.output_reserve_tokens, base.output_reserve_tokens,
            "the output reserve moved - the model would have less room to answer because a \
             SUBAGENT is running"
        );
        assert_eq!(
            reserved.context_window_tokens, base.context_window_tokens,
            "the window itself moved"
        );
        assert_eq!(
            reserved.prompt_window_tokens, base.prompt_window_tokens,
            "the prompt-side window moved, which is the clamp the preamble is built from"
        );
        assert!(
            (reserved.compaction_threshold - base.compaction_threshold).abs() < f32::EPSILON,
            "the compaction threshold moved"
        );
        assert_eq!(
            reserved.use_compact_prompt(),
            base.use_compact_prompt(),
            "the prompt TIER flipped under a reservation, which rewrites the system prompt \
             wholesale"
        );
    }

    /// The tier switch at its most fragile point: a profile sitting just above
    /// the 12,288 boundary, with the whole budget reserved.
    #[test]
    fn the_prompt_tier_cannot_flip_however_much_is_reserved() {
        let roomy = CompactionProfile::from_context_window(32_768);
        assert!(!roomy.use_compact_prompt(), "fixture is not the roomy tier");
        assert!(
            !roomy.with_history_reserved(1.0).use_compact_prompt(),
            "reserving the whole history budget moved the profile into the compact prompt tier"
        );
    }

    /// No child, no change. This is the state of every turn on a pond that
    /// never delegates, so it has to be byte-identical rather than merely
    /// close.
    #[test]
    fn no_reservation_is_the_same_profile() {
        for window in [4_096, 8_192, 16_384, 128_000] {
            let base = CompactionProfile::for_windows(window, window.min(8_192));
            let same = base.with_history_reserved(0.0);
            assert_eq!(
                same.history_token_budget, base.history_token_budget,
                "window {window}: a zero reservation changed the history budget"
            );
            assert_eq!(same.system_prompt_budget, base.system_prompt_budget);
            assert_eq!(same.memory_token_budget, base.memory_token_budget);
            assert_eq!(same.usable_history_tokens(), base.usable_history_tokens());
        }
    }

    /// A fraction that is not a fraction reserves EVERYTHING.
    ///
    /// `AgentRole::new` refuses a `context_fraction` outside `(0, 1]`, so a bad value is
    /// a ledger defect; the failure narrows, since lost recall beats an overrun.
    #[test]
    fn a_fraction_that_is_not_a_fraction_reserves_the_whole_budget() {
        let base = CompactionProfile::from_context_window(8_192);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.5, -0.5, 47.0] {
            let reserved = base.with_history_reserved(bad);
            assert_eq!(
                reserved.history_token_budget, 0,
                "fraction {bad} left the parent {} tokens instead of reserving everything",
                reserved.history_token_budget
            );
        }
        // Vacuity control: a GOOD fraction must not be treated the same way, or
        // the assertion above is satisfied by a method that always returns 0.
        assert!(
            base.with_history_reserved(0.5).history_token_budget > 0,
            "an ordinary fraction also left the parent nothing, so the bad-input assertion above \
             proves nothing"
        );
    }

    /// The reservation comes out of what the window can actually give history,
    /// not out of what the profile declares. At 8,192 those differ by 332
    /// tokens and the difference is what makes two claims sum past the window.
    #[test]
    fn the_reservation_is_taken_from_the_clamped_budget_not_the_declared_one() {
        let base = CompactionProfile::from_context_window(8_192);
        assert_eq!(base.history_token_budget, 4_000, "declared");
        assert_eq!(
            base.usable_history_tokens(),
            3_668,
            "what the window allows"
        );

        let half = base.with_history_reserved(0.5);
        assert_eq!(
            half.history_token_budget, 1_834,
            "half of the CLAMPED 3,668 is what the parent keeps; half of the declared 4,000 \
             would leave it 2,000, and 2,000 + 2,000 + the preamble overruns the window"
        );
    }

    /// A structural tripwire for the field-by-field assertion above: if
    /// `CompactionProfile` grows a field, this fails until somebody decides
    /// whether a reservation may move it. It reads the struct declaration rather
    /// than a hand-maintained count, which would follow the field list vacuously.
    #[test]
    fn the_reservation_covers_every_field_of_the_profile() {
        let source = include_str!("context_budget.rs");
        const DECL: &str = "pub struct CompactionProfile {";
        let start = source
            .find(DECL)
            .expect("the profile struct is no longer declared here");
        let body = &source[start + DECL.len()..];
        let end = body.find("\n}").expect("unterminated struct");
        let fields: Vec<&str> = body[..end]
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let rest = line.strip_prefix("pub ")?;
                let name = rest.split(':').next()?.trim();
                (!name.is_empty()).then_some(name)
            })
            .collect();

        assert_eq!(
            fields.len(),
            8,
            "CompactionProfile now has {} fields ({fields:?}); \
             a_reservation_takes_working_set_and_never_preamble asserts on each of them by hand, \
             so decide whether a subagent reservation may move the new one and add it there",
            fields.len()
        );
    }

    // ── PAI-5 P5: the observed reserve ─────────────────────────────────────

    /// Twenty samples of `cost`, the minimum that lets observation speak.
    fn samples(cost: u32) -> Vec<u32> {
        vec![cost; MIN_REASONING_SAMPLES]
    }

    #[test]
    fn a_pond_with_too_little_evidence_keeps_the_measured_anchor() {
        for n in 0..MIN_REASONING_SAMPLES {
            let observed = vec![4_000u32; n];
            assert_eq!(
                observed_output_reserve(&observed, 768, 4_096),
                768,
                "{n} samples moved the reserve. Below the minimum the anchor stands, because a \
                 handful of observations is not evidence about this pond -- and these samples are \
                 huge, so a test that only tried SMALL ones would pass while the guard was gone"
            );
        }
        // Vacuity control: the same enormous cost DOES move it once there is
        // enough of it, so the loop above is the sample-count rule and not some
        // other refusal.
        assert!(observed_output_reserve(&samples(4_000), 768, 4_096) > 768);
    }

    /// The guard that keeps this feature from being worse than the bug.
    ///
    /// `turn_profile` builds a profile per turn, so a reserve that moved with each
    /// sample would re-prefill every turn: 4.19 s on the Orin at 4 096, 19.97 s at 16 384.
    #[test]
    fn a_growing_sample_set_does_not_move_the_reserve_every_turn() {
        // A conversation whose reasoning cost climbs monotonically, so the p95
        // keeps moving. A cycling sample set would let this pass with the
        // quantisation deleted; it is asserted against the unquantised count.
        let mut observed: Vec<u32> = Vec::new();
        let mut reserves: Vec<usize> = Vec::new();
        let mut raw: Vec<usize> = Vec::new();
        for turn in 0..120u32 {
            observed.push(200 + turn * 3);
            reserves.push(observed_output_reserve(&observed, 768, 32_768));

            // What the same derivation would produce with no quantisation: the
            // control this test is measured against.
            let mut sorted = observed.clone();
            sorted.sort_unstable();
            let rank = ((sorted.len() as f64) * REASONING_PERCENTILE).ceil() as usize;
            let index = rank.saturating_sub(1).min(sorted.len() - 1);
            raw.push(((sorted[index] as usize) * 2).max(768));
        }
        let changes = reserves.windows(2).filter(|w| w[0] != w[1]).count();
        let raw_changes = raw.windows(2).filter(|w| w[0] != w[1]).count();

        assert!(
            raw_changes > 40,
            "the control moved only {raw_changes} times, so this fixture does not exercise \
             quantisation at all and the assertion below would pass without it"
        );
        assert!(
            changes * 5 < raw_changes,
            "the reserve moved {changes} times across 120 turns where the unquantised derivation \
             moved {raw_changes}. Every move shifts the trim point, which truncates the \
             reusable KV prefix back to the preamble and re-prefills the history. Quantisation to \
             {RESERVE_QUANTUM_TOKENS} tokens is what bounds it; without it PAI-5 P5 pays that on \
             nearly every turn"
        );
        // Vacuity control: this must not pass because the reserve never moves at
        // all. A step change in reasoning cost has to be picked up.
        let cheap = observed_output_reserve(&samples(100), 256, 8_192);
        let dear = observed_output_reserve(&samples(900), 256, 8_192);
        assert!(
            dear > cheap,
            "the reserve is inert: {cheap} for 100-token reasoning and {dear} for 900. A constant \
             that never moves is the thing this phase replaced"
        );
    }

    #[test]
    fn the_reserve_is_sized_so_a_typical_thinking_block_still_fits_at_thorough() {
        // The relationship this phase rests on: `reasoning_budget_tokens` gives
        // Thorough half the reserve, so a reserve derived from observation must
        // leave an observed-typical block room at the most expensive effort.
        for cost in [120u32, 300, 640] {
            let reserve = observed_output_reserve(&samples(cost), 256, 32_768);
            let profile = CompactionProfile {
                output_reserve_tokens: reserve,
                ..CompactionProfile::from_context_window(32_768)
            };
            let budget = reasoning_budget_tokens(&profile, ReasoningEffort::Thorough);
            assert!(
                budget >= cost as usize,
                "reasoning costs {cost} tokens here and Thorough is budgeted {budget}. The block \
                 would be cut off or overrun the window mid-generation, which is the failure \
                 PAI-5 P5 exists to remove"
            );
        }
    }

    #[test]
    fn observation_can_never_lower_the_reserve_below_the_measured_anchor() {
        // A pond that has only ever seen trivial reasoning.
        let reserve = observed_output_reserve(&samples(8), 768, 4_096);
        assert_eq!(
            reserve, 768,
            "cheap reasoning talked the reserve below the anchor. The anchor came from an \
             observed mid-generation overrun on real hardware; observation may raise it and must \
             never lower it"
        );
    }

    #[test]
    fn a_verbose_model_cannot_reserve_the_whole_window_away_from_history() {
        let window = 4_096;
        let reserve = observed_output_reserve(&samples(9_000), 768, window);
        let share = reserve as f32 / window as f32;
        assert!(
            share <= MAX_OBSERVED_RESERVE_SHARE + f32::EPSILON,
            "a verbose model took {share} of the window ({reserve} of {window}). A conversation \
             with no room for history is not a conversation"
        );
    }

    #[test]
    fn one_outlier_does_not_set_the_reserve_but_the_top_of_the_range_does() {
        let mut mostly_cheap = vec![100u32; MIN_REASONING_SAMPLES];
        mostly_cheap[0] = 20_000;
        let with_outlier = observed_output_reserve(&mostly_cheap, 256, 32_768);
        let without = observed_output_reserve(&samples(100), 256, 32_768);
        assert_eq!(
            with_outlier, without,
            "a single 20,000-token turn moved the reserve. The mean would let it; a percentile \
             must not, or every pond is taxed forever by its worst turn"
        );

        // And the other direction, which the assertion above does not cover: a
        // genuinely dearer TOP of the range must move it. A p95 that ignored
        // everything above the median would also pass the first assertion.
        let mut top_heavy = vec![100u32; MIN_REASONING_SAMPLES];
        for slot in top_heavy.iter_mut().take(3) {
            *slot = 1_200;
        }
        assert!(
            observed_output_reserve(&top_heavy, 256, 32_768) > without,
            "three dear turns in twenty did not move the reserve; the percentile is reading too \
             low and turns will be cut off"
        );
    }
}
