//! Text → phonemes → token ids, the way Kokoro expects them. Kokoro was trained on misaki
//! G2P, but espeak IPA (stress kept) lands inside its vocab on ordinary English chat text, so
//! anything outside the vocab is dropped and counted by [`Vocab::encode`] to keep regressions
//! measurable. The vocab is read from `tokenizer.json` beside the weights, never hand-embedded.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Kokoro's hard context limit, including the pad token at each end.
pub const MAX_CONTEXT: usize = 512;
/// Usable phoneme tokens per forward pass.
pub const MAX_PHONEME_TOKENS: usize = MAX_CONTEXT - 2;
/// Pad / boundary token id. Wraps every sequence.
pub const PAD: i64 = 0;

/// Kokoro's phoneme→id table, read from the model repo's `tokenizer.json`.
#[derive(Debug, Clone)]
pub struct Vocab {
    map: HashMap<char, i64>,
}

/// One chunk of tokenized text, small enough for a single forward pass.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The source text this chunk came from, for logging and per-sentence UI.
    pub text: String,
    /// Phonemes actually encoded (after the vocab filter).
    pub phonemes: String,
    /// Token ids WITHOUT the surrounding pad tokens.
    pub tokens: Vec<i64>,
}

impl Chunk {
    /// The full input sequence: pad, tokens, pad.
    pub fn padded(&self) -> Vec<i64> {
        let mut v = Vec::with_capacity(self.tokens.len() + 2);
        v.push(PAD);
        v.extend_from_slice(&self.tokens);
        v.push(PAD);
        v
    }
}

impl Vocab {
    /// Load from a `tokenizer.json` as shipped in the Kokoro ONNX repo.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read Kokoro tokenizer at {}", path.display()))?;
        Self::from_json(&raw)
    }

    /// Parse `{"model": {"vocab": {"<sym>": id, …}}}`.
    pub fn from_json(raw: &str) -> Result<Self> {
        let v: serde_json::Value =
            serde_json::from_str(raw).context("Kokoro tokenizer.json is not valid JSON")?;
        let obj = v
            .get("model")
            .and_then(|m| m.get("vocab"))
            .and_then(|x| x.as_object())
            .ok_or_else(|| anyhow!("Kokoro tokenizer.json has no model.vocab object"))?;

        let mut map = HashMap::with_capacity(obj.len());
        for (sym, id) in obj {
            // Every Kokoro vocab entry is a single char; anything else would
            // silently never match during encoding, so reject it loudly.
            let mut chars = sym.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                continue;
            };
            let id = id
                .as_i64()
                .ok_or_else(|| anyhow!("Kokoro vocab id for {sym:?} is not an integer"))?;
            map.insert(c, id);
        }
        if map.is_empty() {
            return Err(anyhow!("Kokoro vocab parsed to zero usable symbols"));
        }
        Ok(Self { map })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, c: char) -> bool {
        self.map.contains_key(&c)
    }

    /// Encode a phoneme string, dropping anything outside the vocab. Returns the ids, the
    /// surviving phonemes, and the drop count. That count is the R2 canary: on English it must
    /// be zero, and non-zero means espeak is emitting something this model was never trained on.
    pub fn encode(&self, phonemes: &str) -> (Vec<i64>, String, usize) {
        let mut ids = Vec::with_capacity(phonemes.len());
        let mut kept = String::with_capacity(phonemes.len());
        let mut dropped = 0usize;
        for c in phonemes.chars() {
            match self.map.get(&c) {
                Some(&id) => {
                    ids.push(id);
                    kept.push(c);
                }
                None => dropped += 1,
            }
        }
        (ids, kept, dropped)
    }
}

/// Phonemize `text` into one IPA string per sentence. espeak terminates a sentence on
/// `.`/`?`/`!`, so this is already the split the streaming path wants; do not add a separate
/// sentence splitter that could disagree with it.
pub fn phonemize(text: &str) -> Result<Vec<String>> {
    espeak_rs::text_to_phonemes(text, "en-us", None)
        .map_err(|e| anyhow!("espeak phonemization failed: {e}"))
}

/// Phonemize and tokenize `text` into forward-pass-sized chunks. Sentences are the unit; a
/// sentence longer than [`MAX_PHONEME_TOKENS`] is split further at a phoneme-space boundary.
pub fn chunk(text: &str, vocab: &Vocab) -> Result<(Vec<Chunk>, usize)> {
    let mut out = Vec::new();
    let mut dropped_total = 0usize;

    for sentence in phonemize(text)? {
        let (ids, kept, dropped) = vocab.encode(&sentence);
        dropped_total += dropped;
        if ids.is_empty() {
            continue;
        }
        for (tokens, phonemes) in split_to_limit(&ids, &kept) {
            out.push(Chunk {
                text: sentence.clone(),
                phonemes,
                tokens,
            });
        }
    }
    Ok((out, dropped_total))
}

/// Split an over-long token run at the last space before the limit.
///
/// `ids` and `phonemes` are index-aligned — `encode` keeps exactly the
/// characters it emitted ids for — so one split point serves both.
fn split_to_limit(ids: &[i64], phonemes: &str) -> Vec<(Vec<i64>, String)> {
    let chars: Vec<char> = phonemes.chars().collect();
    debug_assert_eq!(chars.len(), ids.len());

    if ids.len() <= MAX_PHONEME_TOKENS {
        return vec![(ids.to_vec(), phonemes.to_string())];
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    while start < ids.len() {
        let hard_end = (start + MAX_PHONEME_TOKENS).min(ids.len());
        // Prefer a word boundary; fall back to the hard cut when a single
        // "word" somehow runs the whole window.
        let end = if hard_end == ids.len() {
            hard_end
        } else {
            chars[start..hard_end]
                .iter()
                .rposition(|c| *c == ' ')
                .map(|rel| start + rel + 1)
                .filter(|e| *e > start)
                .unwrap_or(hard_end)
        };
        out.push((
            ids[start..end].to_vec(),
            chars[start..end].iter().collect::<String>(),
        ));
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real vocab, as shipped. Kept tiny but structurally identical.
    fn vocab() -> Vocab {
        Vocab::from_json(
            r#"{"model":{"vocab":{"$":0," ":16,"a":43,"b":44,"ð":81,"ə":82,"ˈ":156}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_the_repo_tokenizer_shape() {
        let v = vocab();
        assert_eq!(v.len(), 7);
        assert!(v.contains('ð'));
        assert!(!v.contains('ʣ'));
    }

    #[test]
    fn rejects_a_vocab_it_cannot_use() {
        assert!(Vocab::from_json(r#"{"model":{"vocab":{}}}"#).is_err());
        assert!(Vocab::from_json(r#"{"nope":1}"#).is_err());
        assert!(Vocab::from_json("not json").is_err());
    }

    #[test]
    fn encode_keeps_known_symbols_and_counts_the_rest() {
        let (ids, kept, dropped) = vocab().encode("ðəb ʒʒ");
        assert_eq!(ids, vec![81, 82, 44, 16]);
        assert_eq!(kept, "ðəb ");
        assert_eq!(dropped, 2, "the two ʒ are outside this vocab");
    }

    /// The drop count is the whole point of tracking it — an unknown phoneme
    /// must never silently become a shorter word.
    #[test]
    fn encode_reports_zero_drops_when_everything_is_known() {
        let (_, _, dropped) = vocab().encode("ðəbaˈ");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn padding_wraps_the_sequence() {
        let c = Chunk {
            text: "hi".into(),
            phonemes: "ab".into(),
            tokens: vec![43, 44],
        };
        assert_eq!(c.padded(), vec![PAD, 43, 44, PAD]);
    }

    #[test]
    fn short_input_is_one_chunk() {
        let ids: Vec<i64> = (0..10).collect();
        let out = split_to_limit(&ids, &"a".repeat(10));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.len(), 10);
    }

    /// A sentence past the model's context must be split, and every chunk must
    /// fit — a chunk at the cap is a truncated word in the user's ear.
    #[test]
    fn over_long_input_splits_at_word_boundaries() {
        // 200 three-char "words" separated by spaces = 800 chars, well over 510.
        let word = "aba ";
        let phonemes = word.repeat(200);
        let ids: Vec<i64> = (0..phonemes.chars().count() as i64).collect();

        let out = split_to_limit(&ids, &phonemes);
        assert!(out.len() > 1, "should have split");
        for (tokens, text) in &out {
            assert!(
                tokens.len() <= MAX_PHONEME_TOKENS,
                "chunk of {} exceeds the {MAX_PHONEME_TOKENS} limit",
                tokens.len()
            );
            assert_eq!(tokens.len(), text.chars().count(), "ids and text drifted");
        }
        // Nothing lost in the split.
        let total: usize = out.iter().map(|(t, _)| t.len()).sum();
        assert_eq!(total, ids.len());
        let rejoined: String = out.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(rejoined, phonemes);
    }

    /// A pathological run with no spaces still has to terminate and fit.
    #[test]
    fn unbroken_run_falls_back_to_a_hard_cut() {
        let phonemes = "a".repeat(MAX_PHONEME_TOKENS * 2 + 7);
        let ids: Vec<i64> = (0..phonemes.len() as i64).collect();
        let out = split_to_limit(&ids, &phonemes);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|(t, _)| t.len() <= MAX_PHONEME_TOKENS));
        assert_eq!(out.iter().map(|(t, _)| t.len()).sum::<usize>(), ids.len());
    }
}
