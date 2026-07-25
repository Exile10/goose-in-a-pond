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
    /// Generated tokens, summed across the turn's inferences.
    pub completion_tokens: u32,
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
        if let (Some(prefill_ms), used) = (self.prefill_ms, self.prompt_tokens) {
            if prefill_ms > 0 && used > 0 {
                self.prefill_tok_per_sec = Some(used as f32 * 1000.0 / prefill_ms as f32);
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
            completion_tokens: 88,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, Some(500.0));
        assert_eq!(s.decode_tok_per_sec, Some(22.0));
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

    #[test]
    fn deserializes_without_optional_fields() {
        let json = r#"{"prompt_tokens": 10, "completion_tokens": 2, "inference_count": 1}"#;
        let s: TurnStats = serde_json::from_str(json).unwrap();
        assert_eq!(s.prompt_tokens, 10);
        assert_eq!(s.ttft_ms, None);
    }
}
