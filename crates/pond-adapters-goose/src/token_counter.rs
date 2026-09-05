//! Driven Adapter: token counting over Goose's tiktoken counter (`o200k_base`, GPT-4o's vocab).
//! Not exact for the Gemma/Qwen/Llama GGUFs GIAP runs (`is_exact` is false), but a real BPE beats
//! `chars/4` on JSON tool results and multi-byte scripts. Exact counts need a fork patch exposing
//! `LlamaModel::str_to_token`; deferred because `turn_trimmer`'s overshoot feedback bounds it.

use pond_core::models::ports::token_counter::TokenCounter;

/// Wraps Goose's tiktoken-backed counter, which carries its own LRU cache
/// keyed by a blake3 hash of the text — so re-counting an unchanged history
/// every turn is cheap.
pub struct TiktokenCounter {
    inner: goose::token_counter::TokenCounter,
}

impl TiktokenCounter {
    /// Builds the counter. `o200k_base` is embedded in `tiktoken_rs`, so this
    /// touches no network and works offline.
    pub async fn new() -> Result<Self, String> {
        Ok(Self {
            inner: goose::token_counter::TokenCounter::new().await?,
        })
    }
}

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> usize {
        self.inner.count_tokens(text)
    }

    /// Always false: see the module docs. A real tokenizer over the wrong
    /// vocabulary is not an exact count, and budget code relies on this being
    /// honest to decide how much margin to keep.
    fn is_exact(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "tiktoken-o200k"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::services::context::token_counting::HeuristicTokenCounter;

    #[tokio::test]
    async fn counts_plain_text_in_the_same_ballpark_as_the_heuristic() {
        let counter = TiktokenCounter::new()
            .await
            .expect("o200k_base is embedded in tiktoken_rs and must load offline");
        let text = "The quick brown fox jumps over the lazy dog, repeatedly and at length.";
        let tk = counter.count(text);
        let heuristic = HeuristicTokenCounter.count(text);
        assert!(tk > 0);
        // English prose is where chars/4 is at its best; they should not
        // diverge wildly, or one of them is broken.
        assert!(
            tk * 4 > heuristic && tk < heuristic * 4,
            "tiktoken={tk} heuristic={heuristic}"
        );
    }

    #[tokio::test]
    async fn multibyte_text_is_where_the_heuristic_is_worst() {
        let counter = TiktokenCounter::new()
            .await
            .expect("o200k_base is embedded in tiktoken_rs and must load offline");
        // `len()` is bytes, so the heuristic inflates this; a real BPE does not.
        let text = "これは日本語のテキストです。";
        assert!(
            counter.count(text) < HeuristicTokenCounter.count(text) * 3,
            "a real tokenizer should not inflate multi-byte text the way len()/4 does"
        );
    }

    #[tokio::test]
    async fn never_claims_to_be_exact() {
        let counter = TiktokenCounter::new()
            .await
            .expect("o200k_base is embedded in tiktoken_rs and must load offline");
        assert!(!counter.is_exact());
        assert_eq!(counter.name(), "tiktoken-o200k");
    }
}
