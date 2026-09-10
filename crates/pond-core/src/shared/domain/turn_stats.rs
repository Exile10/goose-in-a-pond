//! Per-turn inference performance statistics.
//!
//! Aggregated across every inference the agentic loop ran for one user turn
//! (tool round-trips mean a turn can contain several inferences). Raw values
//! come from the provider (`ProviderStats` on the local llama.cpp engine);
//! derived rates are computed here so every consumer — SSE, event log, voice
//! console, UI — shows the same numbers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnStats {
    /// Engine-level time to first generated token of the FIRST inference.
    pub ttft_ms: Option<u64>,
    /// Cold model-load time, when a load happened during this turn.
    pub model_load_ms: Option<u64>,
    /// Prefill time (template + tokenize + prompt decode), summed across
    /// the turn's inferences.
    pub prefill_ms: Option<u64>,
    /// Generation (decode) time, summed across the turn's inferences.
    pub decode_ms: Option<u64>,
    /// Prompt size of the FINAL inference — the turn's real context load.
    pub prompt_tokens: u32,
    /// Prompt tokens this turn actually DECODED, summed across its inferences.
    ///
    /// Distinct from `prompt_tokens` in both directions, and that is the point.
    /// A turn reusing a KV prefix presents a 7,600-token prompt and decodes 55
    /// of them; a cold turn with three tool round-trips decodes its prompt once
    /// and reuses it twice. `prefill_tok_per_sec` was previously
    /// `prompt_tokens / prefill_ms` — one inference's tokens over every
    /// inference's time — which read 3,940 tok/s on a reuse turn that decoded
    /// 55 tokens and 220 tok/s on a cold turn that decoded 6,807. Wrong by an
    /// order of magnitude in both directions, and in opposite directions, so it
    /// could not even be corrected by a constant.
    ///
    /// `0` with a non-zero `prefill_ms` is meaningful: everything was cached and
    /// the time went on template application and tokenization.
    #[serde(default)]
    pub prefilled_tokens: u32,
    /// Prompt tokens served from a retained KV cache instead of decoded, summed
    /// across the turn's inferences.
    ///
    /// `None` means the backend reports nothing (every HTTP provider); `Some(0)`
    /// means it supports prefix reuse and reused nothing, which on turn 2+ is a
    /// prefix that stopped being token-stable and is worth chasing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_prefix_tokens: Option<u32>,
    /// Generated tokens, summed across the turn's inferences.
    pub completion_tokens: u32,
    /// Tokens spent on reasoning the user never sees, summed across the turn.
    ///
    /// GIAP-derived: counted from the structured thinking channel with the
    /// [`TokenCounter`](crate::models::ports::token_counter::TokenCounter) port,
    /// because no provider reports it. See `UsageStats::reasoning_tokens` for
    /// why it is *not* subtracted from `completion_tokens`, and why `None`
    /// ("nobody counted") is deliberately distinct from `Some(0)` ("no
    /// reasoning this turn").
    ///
    /// Deliberately excluded from `finalize_rates`: `decode_tok_per_sec` is a
    /// rate over what the provider reported, and mixing a GIAP-derived count
    /// into a provider-reported rate would make the throughput number a
    /// different quantity depending on which model answered.
    pub reasoning_tokens: Option<u32>,

    /// Attempts BEYOND the first that this turn needed -- 0 for an ordinary
    /// turn, 1 when the model produced only reasoning, ended, and had to be
    /// steered back with `EMPTY_TURN_STEER`.
    ///
    /// Counted because it is the hidden half of what thinking costs. A silent
    /// turn is not a slow turn: it is the whole turn again, prefill included,
    /// and gemma-4-E2B does it reliably for certain phrasings. Until this
    /// existed the only visible symptom was that the pond felt slow, with the
    /// reason buried in a DEBUG line nobody roots their log at.
    ///
    /// `#[serde(default)]` because this type rides `AgentStreamEvent::Done`,
    /// which crosses a PROCESS boundary: `pond-server chat --json-events` emits
    /// it as NDJSON and the desktop's voice child consumes it. Without the
    /// attribute a new consumer refuses every event an older binary produced,
    /// and the pair is version-skewed for exactly as long as it takes someone
    /// to rebuild both halves. Two existing tests failed on this the moment the
    /// field was added, which is the only reason it was noticed.
    ///
    /// Defaulting to 0 does not fabricate a measurement the way a
    /// `reasoning_tokens` default would. Nothing deserialized ever reaches
    /// `turn_metrics`: that row is built from the in-process `TurnStats` the
    /// adapter itself constructed, and a turn with no stats at all writes NULL
    /// through `TurnMetrics::reengagements`, which stays an `Option` precisely
    /// so the unmeasured case survives the database.
    #[serde(default)]
    pub reengagements: u32,
    pub prefill_tok_per_sec: Option<f32>,
    pub decode_tok_per_sec: Option<f32>,
    /// Prompt tokens of the final inference (same basis as `prompt_tokens`),
    /// kept separate so UI can show `used/limit` without re-deriving.
    pub context_used_tokens: Option<u32>,
    /// The engine's actual context window (n_ctx) for this turn.
    pub context_limit_tokens: Option<u32>,
    /// Number of inferences the agentic loop ran (1 + tool round-trips).
    pub inference_count: u32,
    /// Speculative-decoding acceptance rate, when a draft model was active.
    pub draft_accept_rate: Option<f32>,
}

impl TurnStats {
    /// Derive the rate fields from the raw timing/token fields.
    pub fn finalize_rates(&mut self) {
        // Tokens actually decoded over the time spent decoding them. Using
        // `prompt_tokens` here — one inference's prompt over every inference's
        // prefill — is the defect this field exists to fix.
        if let (Some(prefill_ms), decoded) = (self.prefill_ms, self.prefilled_tokens) {
            if prefill_ms > 0 && decoded > 0 {
                self.prefill_tok_per_sec = Some(decoded as f32 * 1000.0 / prefill_ms as f32);
            }
        }
        if let Some(decode_ms) = self.decode_ms {
            if decode_ms > 0 && self.completion_tokens > 0 {
                self.decode_tok_per_sec =
                    Some(self.completion_tokens as f32 * 1000.0 / decode_ms as f32);
            }
        }
    }

    /// Percentage of the context window occupied by the final prompt, when
    /// both sides are known.
    pub fn context_pct(&self) -> Option<f32> {
        match (self.context_used_tokens, self.context_limit_tokens) {
            (Some(used), Some(limit)) if limit > 0 => Some(used as f32 * 100.0 / limit as f32),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_rates_computes_tok_per_sec() {
        let mut s = TurnStats {
            prefill_ms: Some(2000),
            decode_ms: Some(4000),
            prompt_tokens: 1000,
            prefilled_tokens: 1000,
            completion_tokens: 88,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, Some(500.0));
        assert_eq!(s.decode_tok_per_sec, Some(22.0));
    }

    /// The reuse turn that made the old formula unusable: a 7,600-token prompt
    /// of which the engine decoded 55. The rate is about what was decoded.
    #[test]
    fn a_reused_prefix_does_not_inflate_the_prefill_rate() {
        let mut s = TurnStats {
            prefill_ms: Some(2000),
            prompt_tokens: 7636,
            prefilled_tokens: 55,
            reused_prefix_tokens: Some(7581),
            ..Default::default()
        };
        s.finalize_rates();
        // The old formula gave 7636/2s = 3,818 tok/s for 55 decoded tokens.
        assert_eq!(s.prefill_tok_per_sec, Some(27.5));
    }

    /// And the opposite end: a turn whose prefill time is spread over several
    /// inferences must not be divided into one inference's prompt.
    #[test]
    fn several_inferences_sum_what_each_actually_decoded() {
        let mut s = TurnStats {
            prefill_ms: Some(4000),
            // Final inference's prompt, which is NOT the work done.
            prompt_tokens: 7000,
            // 6,800 decoded cold, then 100 more on each of two tool rounds.
            prefilled_tokens: 7000,
            inference_count: 3,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, Some(1750.0));
    }

    /// Everything cached: no rate, rather than a division that invents one.
    #[test]
    fn a_fully_cached_prompt_reports_no_prefill_rate() {
        let mut s = TurnStats {
            prefill_ms: Some(300),
            prompt_tokens: 7636,
            prefilled_tokens: 0,
            reused_prefix_tokens: Some(7636),
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, None);
    }

    #[test]
    fn finalize_rates_skips_zero_denominators() {
        let mut s = TurnStats {
            prefill_ms: Some(0),
            decode_ms: None,
            prompt_tokens: 100,
            completion_tokens: 5,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, None);
        assert_eq!(s.decode_tok_per_sec, None);
    }

    #[test]
    fn context_pct_needs_both_sides() {
        let s = TurnStats {
            context_used_tokens: Some(1536),
            context_limit_tokens: Some(3072),
            ..Default::default()
        };
        assert_eq!(s.context_pct(), Some(50.0));
        let empty = TurnStats::default();
        assert_eq!(empty.context_pct(), None);
    }

    /// PAI-5 P2. Reasoning is reported ALONGSIDE the provider's completion
    /// count, never folded into it — the provider's output count probably
    /// already includes the reasoning decode and nobody has measured which way,
    /// so subtracting would corrupt the one number the engine actually
    /// reported. `finalize_rates` must therefore give the same decode rate
    /// whether or not reasoning was counted.
    #[test]
    fn reasoning_does_not_move_the_completion_count_or_the_decode_rate() {
        let base = TurnStats {
            decode_ms: Some(4000),
            completion_tokens: 88,
            ..Default::default()
        };
        let mut without = base.clone();
        let mut with = TurnStats {
            reasoning_tokens: Some(500),
            ..base
        };
        without.finalize_rates();
        with.finalize_rates();
        assert_eq!(with.completion_tokens, without.completion_tokens);
        assert_eq!(with.decode_tok_per_sec, without.decode_tok_per_sec);
        assert_eq!(with.reasoning_tokens, Some(500));
    }

    /// "Nobody counted" and "counted, and it was zero" are different facts.
    /// PAI-5 P5 derives `output_reserve_tokens` from this data and must not
    /// read an unmeasured turn as a turn that did no thinking.
    #[test]
    fn unmeasured_reasoning_is_not_zero_reasoning() {
        let unmeasured: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":1,"completion_tokens":1,"inference_count":1}"#,
        )
        .unwrap();
        assert_eq!(unmeasured.reasoning_tokens, None);
        let measured: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":1,"completion_tokens":1,"inference_count":1,"reasoning_tokens":0}"#,
        )
        .unwrap();
        assert_eq!(measured.reasoning_tokens, Some(0));
    }

    /// A payload from a binary that predates the field must still parse.
    ///
    /// This is not hypothetical tidiness. `AgentStreamEvent::Done` carries this
    /// struct as NDJSON out of `pond-server chat --json-events`, and the
    /// desktop's voice child is a SEPARATE process that can be older or newer
    /// than the server that spawned it. When `reengagements` was first added
    /// without `#[serde(default)]` it was a required field, and every event an
    /// older binary produced became a parse error — a silently dead voice
    /// stream, not a compile error.
    ///
    /// The counterpart assertion matters as much: a NEW payload that says 0
    /// must still read as 0, so the default cannot be hiding a producer that
    /// stopped sending the field.
    #[test]
    fn a_payload_without_the_re_engagement_count_still_parses() {
        let old: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":2,"inference_count":1}"#,
        )
        .expect("an event from a binary that predates the field must still deserialize");
        assert_eq!(old.reengagements, 0);

        let ordinary: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":2,"inference_count":1,"reengagements":0}"#,
        )
        .unwrap();
        assert_eq!(ordinary.reengagements, 0);

        let steered: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":2,"inference_count":1,"reengagements":2}"#,
        )
        .unwrap();
        assert_eq!(
            steered.reengagements, 2,
            "the default is swallowing a value that was actually sent"
        );
    }

    #[test]
    fn deserializes_without_optional_fields() {
        let json = r#"{"prompt_tokens": 10, "completion_tokens": 2, "inference_count": 1}"#;
        let s: TurnStats = serde_json::from_str(json).unwrap();
        assert_eq!(s.prompt_tokens, 10);
        assert_eq!(s.ttft_ms, None);
    }
}
