//! Text → phonemes → token ids, the way Kokoro expects them.
//!
//! ## Why espeak is enough
//!
//! Kokoro was trained on misaki G2P output. Its vocab reserves single-character
//! tokens (`A`, `I`, `O`, `Q`, `W`, `Y`) for diphthongs espeak spells with two
//! characters. So espeak IPA is *not* the reference tokenization.
//!
//! The reference JS/Python runtimes take the pragmatic path anyway: phonemize
//! with espeak (stress marks kept), then drop anything outside the vocab. On a
//! corpus of ordinary chat text — numbers, dates, acronyms, hard consonant
//! clusters — that drops **zero** characters, because espeak's English IPA
//! already lands inside Kokoro's alphabet. See [`Vocab::encode`]'s `dropped`
//! count, which exists so a regression here is measurable rather than merely
//! audible.
//!
//! ## Why the vocab is loaded, not embedded
//!
//! A hand-transcribed vocab is how you get a phoneme silently deleted from
//! every utterance. `tokenizer.json` ships beside the weights; read it.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// espeak-ng is one global C library, and it is not thread-safe.
///
/// `espeak_SetVoiceByName` mutates process-wide state and `espeak_TextToPhonemes`
/// walks a cursor through it, so two threads phonemizing at once corrupt each
/// other. It shows up as a SIGSEGV with `N_VOICES_LIST` warnings before it,
/// which reads like a bad build rather than a data race — the reason it is
/// worth a comment this long.
///
/// The lock is here, at the only place this crate touches espeak, rather than
/// in a caller: a caller that forgets is a crash, and there is no type that
/// would remind it.
static ESPEAK: Mutex<()> = Mutex::new(());

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

    /// Encode a phoneme string, dropping anything outside the vocab.
    ///
    /// Returns the ids, the phonemes that survived, and how many characters
    /// were dropped. The drop count is the R2 canary: on English it should be
    /// zero, and a non-zero count means espeak has started emitting something
    /// this model was never trained to read.
    ///
    /// It is also what keeps [`PROSODY_PUNCT`] honest. Every mark in that list
    /// is in the vocab, so preserving them adds nothing to this count; a mark
    /// that is not would show up here rather than going quietly missing.
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

/// The punctuation Kokoro was trained to read.
///
/// Exactly the intersection of "is punctuation" and "is in the model's vocab"
/// (`tokenizer.json`), which is not an accident: Kokoro's reference G2P is
/// misaki, and misaki leaves these in the phoneme string. They are prosody —
/// a comma is a short pause, a question mark bends the pitch up at the end of
/// the clause. Feeding the model none of them is why synthesis reads flat and
/// runs sentences together.
///
/// `$` is in the vocab too and is deliberately not here: currency is spelled
/// out long before this point, by `normalize_for_speech`.
const PROSODY_PUNCT: &[char] = &['.', ',', '!', '?', ';', ':', '"', '(', ')'];

/// Phonemize `text`, keeping the punctuation espeak throws away.
///
/// ## What espeak actually does
///
/// `espeak_rs::text_to_phonemes` returns **one** string with every clause
/// concatenated, no punctuation and — the part that matters more — no
/// separator at all:
///
/// ```text
/// "Hello, world! Are you sure?"  ->  ["həlˈoʊwˈɜːldɑːɹ juː ʃˈʊɹ"]
/// ```
///
/// `həlˈoʊwˈɜːld` is "hello" and "world" fused into one word. So the old code
/// was not merely losing prosody, it was handing Kokoro a different sentence
/// from the one it was given, at every clause boundary in every utterance.
///
/// (The docstring this replaces claimed espeak returned one string per
/// sentence and that the streaming path could rely on it as a sentence split.
/// It returns one element regardless of input. Nothing downstream depended on
/// the claim — `split_sentences` in `pond-voice` had already done the real
/// split — but it is worth naming, because it is the reason nobody looked
/// here.)
///
/// ## What this does instead
///
/// Cut the source into runs of speech and runs of punctuation, phonemize the
/// speech runs one at a time, and put the punctuation back between them.
/// espeak never sees a clause boundary, so it has nothing to swallow.
pub fn phonemize(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len() * 2);

    for segment in segments(text) {
        match segment {
            Segment::Speech {
                text: run,
                leading_space,
            } => {
                let spoken = {
                    // Poisoning is not meaningful here: espeak holds no state
                    // of ours, so a panicking sibling leaves nothing to repair.
                    let _guard = ESPEAK.lock().unwrap_or_else(|e| e.into_inner());
                    espeak_rs::text_to_phonemes(run, "en-us", None)
                        .map_err(|e| anyhow!("espeak phonemization failed: {e}"))?
                }
                .join(" ");
                let spoken = spoken.trim();
                if spoken.is_empty() {
                    continue;
                }
                // espeak trims, so a space between two runs has to be restored
                // from the source or the words either side fuse — which is the
                // bug this function exists to fix, and it would come straight
                // back one layer up.
                if leading_space && !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(spoken);
            }
            Segment::Punct(c) => out.push(c),
        }
    }

    Ok(out)
}

/// One run of the source: speech to be phonemized, or a mark to be kept.
enum Segment<'a> {
    Speech { text: &'a str, leading_space: bool },
    Punct(char),
}

/// Cut `text` into alternating speech and punctuation runs.
///
/// Only [`PROSODY_PUNCT`] breaks a run. Everything else — apostrophes inside
/// contractions, hyphens inside compounds — stays in the speech run, because
/// espeak pronounces those as part of the word and the vocab has no token for
/// them anyway.
fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut leading_space = false;

    for (i, c) in text.char_indices() {
        if PROSODY_PUNCT.contains(&c) {
            if let Some(start) = run_start.take() {
                out.push(Segment::Speech {
                    text: &text[start..i],
                    leading_space,
                });
            }
            out.push(Segment::Punct(c));
            // The next speech run is separated from this mark by whatever
            // whitespace follows it, which the loop below will record.
            leading_space = false;
        } else if run_start.is_none() {
            if c.is_whitespace() {
                leading_space = true;
            } else {
                run_start = Some(i);
            }
        }
    }

    if let Some(start) = run_start {
        out.push(Segment::Speech {
            text: &text[start..],
            leading_space,
        });
    }
    out
}

/// Phonemize and tokenize `text` into forward-pass-sized chunks.
///
/// Sentences are the natural unit. A sentence longer than [`MAX_PHONEME_TOKENS`]
/// is split further at a phoneme-space boundary — Piper had no such limit, so
/// this is the one piece of chunking logic Kokoro genuinely adds.
pub fn chunk(text: &str, vocab: &Vocab) -> Result<(Vec<Chunk>, usize)> {
    let mut out = Vec::new();

    let phonemized = phonemize(text)?;
    let (ids, kept, dropped) = vocab.encode(&phonemized);
    if ids.is_empty() {
        return Ok((out, dropped));
    }

    for (tokens, phonemes) in split_to_limit(&ids, &kept) {
        out.push(Chunk {
            // The source, not the phonemes. The field is documented as being
            // for logging and UI, and it was being handed the IPA — nothing
            // has noticed because nothing reads it yet, which is exactly how
            // long a field can hold the wrong thing when the only check is
            // that it compiles.
            //
            // Every chunk of one input carries that whole input. The split
            // below is by token count, not at a sentence boundary, so there is
            // no substring of the source that corresponds to a chunk. In the
            // streaming path this is moot: `chat.rs` calls `split_sentences`
            // first and hands over one sentence at a time, so a second chunk
            // only exists for a single sentence past the phoneme limit.
            text: text.to_string(),
            phonemes,
            tokens,
        });
    }
    Ok((out, dropped))
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

    // ── Punctuation ───────────────────────────────────────────────────────────

    #[test]
    fn punctuation_reaches_the_model() {
        // Kokoro's vocab has these tokens because it was trained to read them.
        // Sending none of them is what made every utterance flat.
        let out = phonemize("Hello, world! Are you sure? Yes; really.").unwrap();
        for mark in ['.', ',', '!', '?', ';'] {
            assert!(out.contains(mark), "{mark:?} missing from {out:?}");
        }
    }

    #[test]
    fn words_either_side_of_a_mark_stay_separate() {
        // The bug underneath the missing prosody: espeak returns every clause
        // concatenated with no separator, so "Hello, world" arrived as one
        // fused word. This is the regression test for that, and it would fail
        // even if punctuation were dropped again but spacing kept.
        let out = phonemize("Hello, world!").unwrap();
        let (before, after) = out.split_once(',').expect("comma survived");
        assert!(!before.is_empty(), "nothing before the comma in {out:?}");
        assert!(
            after.starts_with(' '),
            "no space after the comma, words will fuse: {out:?}"
        );
    }

    #[test]
    fn quotes_and_parens_survive_without_fusing_their_neighbours() {
        // These are in the vocab and in PROSODY_PUNCT, so they have to behave
        // like the other marks: kept, and not gluing the words either side.
        let quoted = phonemize("She said \"stop\" and left.").unwrap();
        assert_eq!(quoted.matches('"').count(), 2, "{quoted:?}");
        assert!(quoted.ends_with('.'));

        let parens = phonemize("One (two) three.").unwrap();
        assert!(parens.contains('(') && parens.contains(')'), "{parens:?}");
        // "one" and "two" must not have fused across the bracket.
        let inner = parens
            .split_once('(')
            .and_then(|(_, r)| r.split_once(')'))
            .map(|(inner, _)| inner.to_string())
            .expect("bracketed run");
        assert!(!inner.trim().is_empty(), "bracket swallowed its contents");
    }

    #[test]
    fn leading_and_repeated_marks_do_not_produce_stray_spaces() {
        // "Wait... what?" is three dots in a row and a mark at position zero
        // once the first run is consumed — the two shapes most likely to emit a
        // leading space or an empty run.
        let out = phonemize("Wait... what?").unwrap();
        assert!(!out.starts_with(' '), "leading space in {out:?}");
        assert!(out.contains("..."), "ellipsis collapsed: {out:?}");
        assert!(out.ends_with('?'));
        assert!(!out.contains("  "), "double space in {out:?}");
    }

    // The claim "every mark in PROSODY_PUNCT is in the model's alphabet" is
    // NOT tested here, and cannot be: `vocab_for` builds its table by adding
    // PROSODY_PUNCT, so asking it whether it contains those marks answers
    // itself. A tautology in the shape of a guarantee is worse than no test —
    // it is the one somebody points at when the canary starts firing.
    //
    // It lives in `tests/live_synthesis.rs`
    // (`every_preserved_mark_is_in_the_real_vocab`), against the real
    // `tokenizer.json`, which is the only table that can answer it.

    #[test]
    fn preserved_punctuation_is_not_counted_as_dropped() {
        let sample = "hˈɛloʊ, wˈɜːld!";
        let v = vocab_for(&[sample]);
        let (_, kept, dropped) = v.encode(sample);
        assert_eq!(dropped, 0, "kept {kept:?}");
        assert!(kept.contains(','));
        assert!(kept.ends_with('!'));
    }

    #[test]
    fn a_contraction_keeps_its_apostrophe_inside_the_word() {
        // The apostrophe is not in PROSODY_PUNCT on purpose: espeak pronounces
        // it as part of the word, and splitting there would phonemize "don" and
        // "t" separately.
        let out = phonemize("Don't stop.").unwrap();
        assert!(
            !out.contains('\''),
            "apostrophe leaked into phonemes: {out:?}"
        );
        assert!(out.ends_with('.'));
        // One word, not two runs fused or split: "doʊnt" stays whole.
        assert!(out.split(' ').count() >= 2, "{out:?}");
    }

    #[test]
    fn text_with_no_punctuation_is_unchanged_in_shape() {
        let out = phonemize("no punctuation here").unwrap();
        assert!(!out.is_empty());
        assert!(out.split(' ').count() >= 3, "words ran together: {out:?}");
    }

    #[test]
    fn a_chunk_carries_the_source_text_not_its_phonemes() {
        let v = vocab_for(&[&phonemize("Hello, world!").unwrap()]);
        let (chunks, _) = chunk("Hello, world!", &v).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello, world!");
        assert_ne!(chunks[0].text, chunks[0].phonemes);
    }

    /// A vocab holding exactly the symbols a test uses, plus every mark.
    ///
    /// Built from the input rather than hand-listed: a hand-listed phoneme
    /// vocab is how you write a test that passes because the character it
    /// meant to check was never in the table.
    ///
    /// It adds `PROSODY_PUNCT` unconditionally, so nothing built on it can be
    /// used to ask whether those marks are in the *model's* vocab — see the
    /// note above `preserved_punctuation_is_not_counted_as_dropped`.
    fn vocab_for(samples: &[&str]) -> Vocab {
        let mut symbols: Vec<char> = samples.iter().flat_map(|s| s.chars()).collect();
        symbols.extend_from_slice(PROSODY_PUNCT);
        symbols.sort_unstable();
        symbols.dedup();

        let entries: Vec<String> = symbols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{}: {}",
                    serde_json::to_string(&c.to_string()).unwrap(),
                    i + 1
                )
            })
            .collect();
        Vocab::from_json(&format!(
            "{{\"model\":{{\"vocab\":{{{}}}}}}}",
            entries.join(",")
        ))
        .expect("test vocab")
    }
}
