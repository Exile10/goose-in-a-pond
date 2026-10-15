//! Lexical passage ranking: BM25 over candidate texts against a question, plus an answer-type
//! boost. No model and no I/O, so a tool can choose which part of what it fetched answers the
//! question it was asked.
//!
//! A port of `_terms` and `_bm25` in `experiments/knowledge-tools/kt/variants.py` and of
//! `stem` and `boosted` in `kt/rankers.py`, where the bench measured them. IDF is computed over
//! the candidates given, so words every candidate shares (a page's own title, the search terms
//! themselves) weigh little.

use std::sync::LazyLock;

use regex::Regex;

/// Words too common to tell two passages apart; the prototype's list, unchanged.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "in", "on", "at", "to", "for", "and", "or", "is", "are", "was", "were",
    "be", "been", "who", "what", "when", "where", "which", "why", "how", "did", "do", "does",
    "about", "tell", "me", "there", "this", "that", "it", "its", "his", "her", "their", "they",
    "he", "she", "with", "from", "by", "as", "into", "most", "mostly", "new",
];

/// Suffixes [`stem`] strips, tried in this order; the first that fits is the only one removed.
const SUFFIXES: &[&str] = &[
    "ations", "ation", "ings", "ing", "edly", "ers", "ies", "ied", "ed", "es", "ly", "er", "s",
];

/// A suffix is stripped only if at least this many characters remain.
const MIN_STEM_CHARS: usize = 3;

/// After stripping, a stem is cut to this many characters.
const STEM_CHARS: usize = 5;

/// BM25 term-frequency saturation.
const K1: f64 = 1.2;

/// BM25 length normalisation: 0 ignores passage length, 1 normalises fully.
const B: f64 = 0.75;

/// Appended to a passage [`clip`] had to cut mid-sentence.
const ELLIPSIS: &str = "\u{2026}";

/// What shape of text answers a kind of question: when the question pattern matches, every
/// passage matching the answer pattern gains the top BM25 score. This is retrieval inside a tool
/// the model already chose to call, not routing: it decides which fetched passage to quote,
/// never which tool runs. The pairs are cumulative.
static ANSWER_TYPES: LazyLock<Vec<(Regex, Regex)>> = LazyLock::new(|| {
    [
        (
            r"(?i)\b(what time|when|open|opening|close|closing|hours|leave|leaves|depart|arrive|schedule|timetable)\b",
            r"(?i)\b\d{1,2}([:.]\d{2})?\s?(am|pm)\b|\b\d{1,2}:\d{2}\b",
        ),
        (
            r"(?i)\b(how much|price|cost|costs|fee|fare|rate|exchange)\b",
            r"(?i)(ksh|kes|usd|\$|£|€|shillings?)\s?\d|\d[\d,]*(\.\d+)?\s?(ksh|shillings?|percent|%)|\d+\.\d{2}\b",
        ),
        (
            r"(?i)\b(when|date|year|day)\b",
            r"(?i)\b(1[89]|20)\d{2}\b|\b\d{1,2} (january|february|march|april|may|june|july|august|september|october|november|december)\b",
        ),
        (r"(?i)\b(how many|how far|percent|rate)\b", r"\d"),
    ]
    .into_iter()
    .map(|(question, answer)| {
        (
            Regex::new(question).expect("answer-type question pattern"),
            Regex::new(answer).expect("answer-type answer pattern"),
        )
    })
    .collect()
});

/// One candidate's score. `index` is its position in the slice given to [`rank`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ranked {
    pub index: usize,
    pub score: f64,
}

/// Every candidate, best first; equal scores keep candidate order. A question with no content
/// words and no answer type scores every candidate 0, so the order is the candidates' own.
pub fn rank<S: AsRef<str>>(question: &str, candidates: &[S]) -> Vec<Ranked> {
    let mut ranked: Vec<Ranked> = scores(question, candidates)
        .into_iter()
        .enumerate()
        .map(|(index, score)| Ranked { index, score })
        .collect();
    // Stable, so ties stay in candidate order.
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    ranked
}

/// Each candidate's score, in candidate order: BM25, then the answer-type boost.
pub fn scores<S: AsRef<str>>(question: &str, candidates: &[S]) -> Vec<f64> {
    let query = query_stems(question);
    let mut scores = if query.is_empty() {
        vec![0.0; candidates.len()]
    } else {
        let units: Vec<Vec<String>> = candidates
            .iter()
            .map(|c| terms(c.as_ref()).iter().map(|t| stem(t)).collect())
            .collect();
        bm25(&units, &query)
    };
    let max = scores.iter().copied().fold(0.0_f64, f64::max);
    // Every score 0 boosts by 1, as the prototype does.
    let top = if max > 0.0 { max } else { 1.0 };
    for (question_shape, answer_shape) in ANSWER_TYPES.iter() {
        if !question_shape.is_match(question) {
            continue;
        }
        for (score, candidate) in scores.iter_mut().zip(candidates) {
            if answer_shape.is_match(candidate.as_ref()) {
                *score += top;
            }
        }
    }
    scores
}

/// The question's distinct stems, in the order they first appear.
pub fn query_stems(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for term in terms(question) {
        let s = stem(&term);
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Content words: folded to lowercase ASCII, split on anything that is not a letter or digit,
/// stopwords and single characters dropped.
pub fn terms(text: &str) -> Vec<String> {
    fold(text)
        .split(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
        .filter(|t| t.len() > 1 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

/// One suffix off (an `ies`/`ied` becomes `y`), then the first [`STEM_CHARS`] characters, so
/// "opening" meets "open" and "studies" meets "studied".
pub fn stem(term: &str) -> String {
    let mut stemmed = term.to_string();
    for suffix in SUFFIXES {
        let Some(base) = term.strip_suffix(suffix) else {
            continue;
        };
        if base.chars().count() < MIN_STEM_CHARS {
            continue;
        }
        stemmed = base.to_string();
        if matches!(*suffix, "ies" | "ied") {
            stemmed.push('y');
        }
        break;
    }
    stemmed.chars().take(STEM_CHARS).collect()
}

/// BM25 score of each unit (a passage's stems) against `query` (distinct stems).
pub fn bm25(units: &[Vec<String>], query: &[String]) -> Vec<f64> {
    let n = units.len() as f64;
    let total: usize = units.iter().map(Vec::len).sum();
    let avg = (total as f64 / units.len().max(1) as f64).max(1.0);
    let idf: Vec<f64> = query
        .iter()
        .map(|term| {
            let df = units.iter().filter(|u| u.contains(term)).count() as f64;
            ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
        })
        .collect();
    units
        .iter()
        .map(|unit| {
            let norm = K1 * ((1.0 - B) + B * unit.len() as f64 / avg);
            query
                .iter()
                .zip(&idf)
                .map(|(term, idf)| {
                    let tf = unit.iter().filter(|t| *t == term).count() as f64;
                    if tf == 0.0 {
                        0.0
                    } else {
                        idf * tf * (K1 + 1.0) / (tf + norm)
                    }
                })
                .sum()
        })
        .collect()
}

/// `text` cut to at most `max_bytes`: at the last sentence or clause end past half the budget,
/// else at a character boundary with an ellipsis. Never longer than `max_bytes`.
pub fn clip(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let cut = prefix_within(text, max_bytes);
    let stop = [". ", "; "]
        .iter()
        .filter_map(|sep| cut.rfind(sep))
        .max()
        .filter(|&at| at > max_bytes / 2);
    if let Some(at) = stop {
        return cut.get(..=at).unwrap_or(cut).to_string();
    }
    let room = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut out = prefix_within(text, room).trim_end().to_string();
    out.push_str(ELLIPSIS);
    out
}

/// The longest prefix of `text` within `max_bytes` that ends on a character boundary.
fn prefix_within(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

/// Lowercase with Latin diacritics removed, as Unicode NFKD followed by dropping combining marks
/// would leave the letters below; a table rather than a normalisation dependency.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        if is_combining_mark(c) {
            continue;
        }
        out.push(base_letter(c));
    }
    out
}

/// The combining diacritical marks block, which decomposed input carries its accents in.
fn is_combining_mark(c: char) -> bool {
    ('\u{0300}'..='\u{036F}').contains(&c)
}

/// The base letter of a lowercase precomposed Latin letter, or `c` itself.
fn base_letter(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ª' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'ď' => 'd',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
        'ĥ' => 'h',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ǐ' => 'i',
        'ĵ' => 'j',
        'ķ' => 'k',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' => 'l',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' | 'ŏ' | 'ő' | 'ǒ' | 'ơ' | 'º' => 'o',
        'ŕ' | 'ŗ' | 'ř' => 'r',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' | 'ſ' => 's',
        'ţ' | 'ť' | 'ț' => 't',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' | 'ư' => 'u',
        'ŵ' => 'w',
        'ý' | 'ÿ' | 'ŷ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn first(question: &str, candidates: &[&str]) -> usize {
        rank(question, candidates)[0].index
    }

    #[test]
    fn terms_are_folded_content_words() {
        assert_eq!(
            terms("Who is Ngũgĩ wa Thiong'o?"),
            strings(&["ngugi", "wa", "thiong"]),
            "stopwords and one-letter pieces go, diacritics fold"
        );
        assert_eq!(
            terms("NAIROBI, Kenya: 2024"),
            strings(&["nairobi", "kenya", "2024"])
        );
        // Decomposed input folds the same as precomposed.
        assert_eq!(terms("Ngu\u{0303}gi\u{0303}"), terms("Ngũgĩ"));
        assert!(terms("what is the").is_empty());
    }

    #[test]
    fn opening_meets_open() {
        assert_eq!(stem("opening"), "open");
        assert_eq!(stem("opening"), stem("open"));
        assert_eq!(stem("openings"), stem("open"));
    }

    #[test]
    fn one_suffix_comes_off_then_the_five_character_cut() {
        assert_eq!(stem("attacked"), "attac");
        assert_eq!(stem("attacks"), stem("attacked"));
        assert_eq!(stem("studies"), "study");
        assert_eq!(stem("studied"), stem("studies"));
        assert_eq!(stem("stations"), stem("station"));
        assert_eq!(stem("hours"), "hour");
        // Too short to lose "es": the next suffix that leaves three characters wins.
        assert_eq!(stem("ties"), "tie");
        assert_eq!(stem("mall"), "mall");
    }

    #[test]
    fn query_stems_are_distinct_and_in_order() {
        assert_eq!(
            query_stems("Marathon winners: which marathons did Kipchoge win?"),
            strings(&["marat", "winn", "kipch", "win"])
        );
    }

    /// Numbers from the prototype's `_bm25` on the same input, run with python3.
    #[test]
    fn bm25_matches_the_prototype() {
        let units = vec![
            strings(&["kipch", "won", "berli", "marat", "2023"]),
            strings(&["kipch", "kenya", "runne", "born", "1984", "kapsi"]),
            strings(&["berli", "city", "germa"]),
        ];
        let q = strings(&["berli", "marat", "won"]);
        let got = bm25(&units, &q);
        let want = [2.362_624_409_031_261_6, 0.0, 0.550_422_501_169_911];
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-12, "got {got:?}, want {want:?}");
        }
    }

    /// Numbers from the prototype's `boosted` on the same input, run with python3.
    #[test]
    fn the_boosted_scores_match_the_prototype() {
        let candidates = [
            "Junction Mall is a shopping centre on Ngong Road in Nairobi.",
            "Opening hours: Monday to Sunday, 9am to 9pm.",
            "The mall opened in 2004 and was expanded in 2019.",
        ];
        let got = scores("What time does Junction Mall open?", &candidates);
        let want = [
            1.358_226_528_070_815_7,
            1.828_230_157_316_551_4,
            1.008_788_277_405_481_5,
        ];
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-12, "got {got:?}, want {want:?}");
        }
    }

    /// BM25 with this module's stemmer and no answer-type boost.
    fn plain(question: &str, candidates: &[&str]) -> Vec<f64> {
        let units: Vec<Vec<String>> = candidates
            .iter()
            .map(|c| terms(c).iter().map(|t| stem(t)).collect())
            .collect();
        bm25(&units, &query_stems(question))
    }

    /// Plain BM25 picks the first candidate and the boost picks the second: the pair decides.
    fn the_boost_decides(question: &str, candidates: &[&str; 2]) {
        let plain = plain(question, candidates);
        assert!(plain[0] > plain[1], "BM25 alone must prefer 0: {plain:?}");
        assert_eq!(
            first(question, candidates),
            1,
            "{:?}",
            scores(question, candidates)
        );
    }

    #[test]
    fn the_passage_that_answers_beats_the_one_about_the_topic() {
        let candidates = [
            "Eliud Kipchoge is a Kenyan long-distance runner, born in Kapsisiywa.",
            "Kipchoge won the Berlin Marathon in 2015, 2017, 2018, 2022 and 2023.",
            "Berlin is the capital of Germany.",
        ];
        let ranked = rank("When did Kipchoge win the Berlin marathon?", &candidates);
        assert_eq!(ranked[0].index, 1, "{ranked:?}");
        assert!(ranked[0].score > ranked[1].score);
        assert_eq!(ranked.len(), candidates.len(), "every candidate comes back");
    }

    #[test]
    fn a_time_question_lifts_the_passage_with_a_clock_time() {
        let candidates = [
            "Junction Mall in Nairobi: shops, restaurants and parking at Junction Mall.",
            "The mall opens at 9am daily.",
        ];
        the_boost_decides("What time does Junction Mall open?", &candidates);
        // The same passages for a question of no answer type: BM25 alone decides.
        assert_eq!(first("Where is Junction Mall?", &candidates), 0);
    }

    #[test]
    fn a_price_question_lifts_the_passage_with_an_amount() {
        the_boost_decides(
            "How much is the matatu fare to Westlands?",
            &[
                "Matatu fares to Westlands depend on the matatu and the route to Westlands.",
                "A matatu ride costs KSh 80 off-peak.",
            ],
        );
    }

    #[test]
    fn a_date_question_lifts_the_passage_with_a_year_or_a_date() {
        the_boost_decides(
            "When did Kenya become a republic?",
            &[
                "Kenya, a republic: Kenya became a republic within the Commonwealth.",
                "The republic was declared on 12 December 1964.",
            ],
        );
    }

    #[test]
    fn a_count_question_lifts_the_passage_with_a_number() {
        the_boost_decides(
            "How many people live in Nairobi?",
            &[
                "Nairobi people love Nairobi, and people from all over live in Nairobi.",
                "About 4.4 million people live there.",
            ],
        );
    }

    #[test]
    fn a_question_with_no_answer_type_gets_no_boost() {
        let candidates = [
            "Safaricom was founded by Michael Joseph.",
            "Founded in 1997.",
        ];
        let question = "Who founded Safaricom?";
        assert_eq!(scores(question, &candidates), plain(question, &candidates));
    }

    #[test]
    fn ties_and_empty_questions_keep_candidate_order() {
        let candidates = ["first passage", "second passage", "third passage"];
        let ranked = rank("what is the", &candidates);
        let order: Vec<usize> = ranked.iter().map(|r| r.index).collect();
        assert_eq!(order, vec![0, 1, 2]);
        assert!(ranked.iter().all(|r| r.score == 0.0));
        assert!(rank::<&str>("anything", &[]).is_empty());
    }

    #[test]
    fn clip_keeps_short_text_whole() {
        assert_eq!(clip("Short.", 100), "Short.");
    }

    #[test]
    fn clip_prefers_a_sentence_end() {
        let text = "The mall opens at nine. It closes at ten on weekdays and later on Fridays.";
        assert_eq!(clip(text, 40), "The mall opens at nine.");
    }

    #[test]
    fn clip_never_exceeds_its_budget_and_never_splits_a_character() {
        let text =
            "Ngũgĩ wa Thiong'o wrote Weep Not, Child, Petals of Blood and Wizard of the Crow";
        for budget in 4..text.len() {
            let out = clip(text, budget);
            assert!(
                out.len() <= budget,
                "{budget}: {out:?} is {} bytes",
                out.len()
            );
        }
        let out = clip(text, 20);
        assert!(out.ends_with(ELLIPSIS), "{out:?}");
    }
}
