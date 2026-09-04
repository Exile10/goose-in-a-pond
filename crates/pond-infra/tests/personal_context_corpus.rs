//! Drives the synthetic household corpus through the REAL ingest path.
//!
//! Why this exists before any host-device connector does: PAI-8's open work is
//! a set of numbers nobody has measured. What does a household's context cost
//! in prompt tokens on a Jetson-class budget? Does the redactor fire on a
//! Kenyan phone number, and does it stay silent on an IP address and a git
//! SHA? How many of the eight `SourceKind`s can the pipeline even accept today?
//! All three are answerable from a corpus and none of them need an OS API.
//!
//! The redactor and the pipeline are the production ones -- `RuleRedactor` is
//! the only `Redactor` in production, and `IngestPipeline` is the only writer of
//! `context_items`. Only the repository is a mock, which costs the storage
//! round-trip and buys a hermetic test that runs in `ci.yml`'s fast-crate list
//! (`pond-infra` is in it; `pond-server` is not).
//!
//! `include_str!` rather than a runtime read, deliberately: a fixture that
//! moves must fail the build, not silently produce an empty corpus that passes
//! every assertion below.
//!
//! Run the report with:
//!     cargo test -p pond-infra --test personal_context_corpus -- --nocapture

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use pond_core::context::domain::{
    ContextItem, ContextSource, ItemKind, SourceAvailability, SourceKind, SourceParts, SourceStatus,
};
use pond_core::context::ingest::{IngestPipeline, RawItem};
use pond_core::context::mocks::mock_context_repository::MockContextRepository;
use pond_core::context::retrieval::{
    estimated_tokens, rank_by_relevance, render_block, select_within_budget, split_preamble_budget,
};
use pond_infra::rule_redactor::RuleRedactor;
use serde_json::Value;

const SOURCES: &str = include_str!("../../../fixtures/personal-context/sources.json");
const ITEMS: &str = include_str!("../../../fixtures/personal-context/household.jsonl");
const EXPECTATIONS: &str = include_str!("../../../fixtures/personal-context/expectations.jsonl");
const MANIFEST: &str = include_str!("../../../fixtures/personal-context/manifest.json");
const QUERIES: &str = include_str!("../../../fixtures/personal-context/queries.jsonl");

/// The corpus's anchored moment, not the wall clock. Every recency score below
/// would drift with the calendar otherwise, and a fixture whose measurements
/// change daily is not a fixture.
fn corpus_now() -> DateTime<Utc> {
    let m: Value = serde_json::from_str(MANIFEST).expect("manifest");
    m["corpus_now"]
        .as_str()
        .expect("corpus_now")
        .parse()
        .expect("corpus_now parses")
}

fn sources() -> Vec<ContextSource> {
    let raw: Vec<Value> = serde_json::from_str(SOURCES).expect("sources.json");
    raw.iter()
        .map(|s| {
            let kind = SourceKind::parse(s["kind"].as_str().unwrap())
                .expect("fixture names a SourceKind that no longer exists");
            ContextSource::from_parts(SourceParts {
                id: s["id"].as_str().unwrap().to_string(),
                kind,
                provider: s["provider"].as_str().unwrap().to_string(),
                profile_id: s["profile_id"].as_str().unwrap().to_string(),
                scopes: s["scopes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect(),
                cursor: None,
                last_sync: None,
                status: SourceStatus::Connected,
                secret_ref: None,
                created_at: corpus_now(),
            })
            .expect("fixture source is valid")
        })
        .collect()
}

/// One JSONL line to a `RawItem` plus its routing key.
///
/// Note what is NOT read here, because there is nothing to read: no line carries
/// a `profile_id`. The owner comes from the source, per the ingest invariant.
fn items() -> Vec<(String, RawItem)> {
    ITEMS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("corpus line");
            assert!(
                v.get("profile_id").is_none(),
                "a corpus line named its own owner; the ingest invariant says the \
                 source decides whose data this is"
            );
            let raw = RawItem {
                external_id: v["external_id"].as_str().unwrap().to_string(),
                kind: ItemKind::parse(v["kind"].as_str().unwrap())
                    .expect("fixture names an ItemKind that no longer exists"),
                occurred_at: v["occurred_at"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .expect("timestamp"),
                title: v["title"].as_str().unwrap().to_string(),
                body: v["body"].as_str().unwrap().to_string(),
                participants: v["participants"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap().to_string())
                    .collect(),
            };
            (v["source_id"].as_str().unwrap().to_string(), raw)
        })
        .collect()
}

struct Expectation {
    external_id: String,
    findings: BTreeSet<String>,
    note: String,
}

fn expectations() -> Vec<Expectation> {
    EXPECTATIONS
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("expectation line");
            Expectation {
                external_id: v["external_id"].as_str().unwrap().to_string(),
                findings: v["expect_findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap().to_string())
                    .collect(),
                note: v["note"].as_str().unwrap().to_string(),
            }
        })
        .collect()
}

/// Ingest the whole corpus once. Returns the stored items and the per-kind
/// tally of accepted vs refused.
async fn ingest_corpus() -> (Vec<ContextItem>, BTreeMap<&'static str, (usize, usize)>) {
    let repo = Arc::new(MockContextRepository::new());
    let pipeline = IngestPipeline::new(repo.clone(), Arc::new(RuleRedactor::new()));
    let srcs = sources();
    let now = corpus_now();

    // Keyed by the stable string rather than the enum: `SourceKind` is not `Ord`,
    // and deriving it in core to sort a test's report would be the tail wagging.
    let mut tally: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    for (source_id, raw) in items() {
        let source = srcs
            .iter()
            .find(|s| s.id() == source_id)
            .expect("corpus line routes to a source that sources.json does not define");
        let entry = tally.entry(source.kind().as_str()).or_insert((0, 0));
        match pipeline.ingest(source, raw, now).await {
            Ok(_) => entry.0 += 1,
            Err(_) => entry.1 += 1,
        }
    }
    (repo.all_items(), tally)
}

/// Which of the eight source kinds the pipeline can accept today, counted
/// rather than asserted from the doc comment.
///
/// This is the measurement that turns "the ingest route is not built" into a
/// number: every `Mobile` and `Chat` item in the corpus is refused, and the
/// refusal count is exactly what PAI-8 P3 and the Files connector would unblock.
#[tokio::test]
async fn every_landed_kind_ingests_and_every_gated_kind_is_refused() {
    let (stored, tally) = ingest_corpus().await;

    let mut accepted_total = 0;
    let mut refused_total = 0;
    println!("\n-- ingest by source kind ------------------------------------");
    for (name, (ok, refused)) in &tally {
        let kind = SourceKind::parse(name).expect("tally key came from as_str");
        accepted_total += ok;
        refused_total += refused;
        println!(
            "  {:9} accepted {:>4}  refused {:>4}   ({:?})",
            name,
            ok,
            refused,
            kind.availability()
        );
        match kind.availability() {
            SourceAvailability::Landed => assert_eq!(
                *refused, 0,
                "a Landed kind refused {refused} items; the pipeline and the \
                 availability map disagree"
            ),
            _ => assert_eq!(
                *ok, 0,
                "a gated kind accepted {ok} items; SourceAvailability stopped \
                 being enforced at the pipeline"
            ),
        }
    }
    println!(
        "  total     accepted {accepted_total:>4}  refused {refused_total:>4}   \
         ({:.0}% of the corpus is unreachable today)",
        refused_total as f32 * 100.0 / (accepted_total + refused_total) as f32
    );

    assert_eq!(
        stored.len(),
        accepted_total,
        "a stored row was not accounted for"
    );
    assert!(
        refused_total > 0,
        "the corpus must exercise the gated kinds too"
    );
}

/// The redactor's accuracy on household prose, in both directions.
///
/// The false-negative half is easy and everyone writes it. The false-POSITIVE
/// half is the one that matters and the one nobody had: an ordinary sentence
/// wrongly matched is silently rewritten in the text the model reads, and for
/// the three Secret-class kinds the original is gone.
#[tokio::test]
async fn the_redactor_finds_what_the_corpus_plants_and_nothing_more() {
    let (stored, _) = ingest_corpus().await;
    let by_ext: BTreeMap<&str, &ContextItem> =
        stored.iter().map(|i| (i.external_id(), i)).collect();

    // Which source each item was routed to, so an expectation the pipeline never
    // reached can be told apart from one it got wrong.
    let srcs = sources();
    let source_of: BTreeMap<String, SourceKind> = items()
        .into_iter()
        .map(|(sid, r)| {
            let kind = srcs.iter().find(|s| s.id() == sid).expect("source").kind();
            (r.external_id, kind)
        })
        .collect();

    let mut failures: Vec<String> = Vec::new();
    let mut false_positive_items = 0;
    let mut checked_clean = 0;
    let mut blocked: Vec<String> = Vec::new();

    println!("\n-- redaction expectations -----------------------------------");
    for exp in expectations() {
        let item = match by_ext.get(exp.external_id.as_str()) {
            Some(i) => i,
            None => {
                // Not a failure when the item's source kind is one the pipeline
                // refuses: the expectation is about the redactor, and the redactor
                // was never reached. Recorded, because an unexercised rule is worth
                // knowing about -- see the note on `env-file-doc`.
                let kind = source_of
                    .get(&exp.external_id)
                    .copied()
                    .expect("expectation names an item the corpus does not contain");
                if kind.availability() == SourceAvailability::Landed {
                    failures.push(format!(
                        "{}: on a Landed source yet never stored",
                        exp.external_id
                    ));
                } else {
                    println!(
                        "  BLOCKED {:<20} expected {:?} -- {} source is {:?}",
                        exp.external_id,
                        exp.findings,
                        kind.as_str(),
                        kind.availability()
                    );
                    blocked.push(format!("{} ({})", exp.external_id, kind.as_str()));
                }
                continue;
            }
        };
        let actual: BTreeSet<String> = item
            .findings()
            .iter()
            .map(|f| f.as_str().to_string())
            .collect();

        if exp.findings.is_empty() {
            checked_clean += 1;
        }
        let verdict = if actual == exp.findings {
            "ok  "
        } else {
            if exp.findings.is_empty() {
                false_positive_items += 1;
            }
            failures.push(format!(
                "{}: expected {:?}, found {:?}\n      {}",
                exp.external_id, exp.findings, actual, exp.note
            ));
            "FAIL"
        };
        println!(
            "  {verdict} {:<20} expected {:?} found {:?}",
            exp.external_id, exp.findings, actual
        );
    }

    println!(
        "  {checked_clean} items asserted clean, {false_positive_items} false positives, \
         {} expectations blocked by a gated source kind",
        blocked.len()
    );
    if !blocked.is_empty() {
        println!("  blocked: {}", blocked.join(", "));
    }

    assert!(
        failures.is_empty(),
        "the redactor disagreed with the corpus on {} item(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// What `INGEST_REDACTION_LEVEL` actually does, made explicit.
///
/// At `RedactionLevel::Secrets` only the three Secret-class kinds are REPLACED.
/// Email, phone and postcode are detected, reported in `findings`, and left in
/// the stored text. That is a deliberate posture and not a bug -- but it is not
/// what "the corpus is redacted" sounds like, so it gets an assertion rather
/// than a sentence in a design document.
#[tokio::test]
async fn secret_class_findings_are_replaced_and_contact_details_are_not() {
    use pond_core::security::domain::redaction::RedactionKind;

    let (stored, _) = ingest_corpus().await;
    let raw_bodies: BTreeMap<String, String> = items()
        .into_iter()
        .map(|(_, r)| (r.external_id, r.body))
        .collect();

    let mut replaced = 0;
    let mut reported_only = 0;
    println!("\n-- what Secrets level replaced ------------------------------");
    for item in &stored {
        if item.findings().is_empty() {
            continue;
        }
        let raw = raw_bodies.get(item.external_id()).expect("raw body");
        for kind in item.findings() {
            if kind.sensitivity() == pond_core::security::domain::event::PrivacySensitivity::Secret
            {
                assert!(
                    item.body().contains(kind.placeholder()),
                    "{}: a {} was found and classifies Secret, but the stored body \
                     carries no placeholder -- the credential survived ingest",
                    item.external_id(),
                    kind.as_str()
                );
                replaced += 1;
                println!("  replaced  {:<20} {}", item.external_id(), kind.as_str());
            } else {
                assert!(
                    !item.body().contains(kind.placeholder()),
                    "{}: a {} was replaced at Secrets level. Either the level \
                     changed or the sensitivity mapping did; both change what the \
                     model reads",
                    item.external_id(),
                    kind.as_str()
                );
                reported_only += 1;
            }
        }
        if item.findings().iter().all(|k| {
            k.sensitivity() != pond_core::security::domain::event::PrivacySensitivity::Secret
        }) {
            assert_eq!(
                item.body(),
                raw,
                "{}: findings were all non-Secret yet the body changed",
                item.external_id()
            );
        }
    }
    println!(
        "  {replaced} secret spans replaced, {reported_only} contact details \
         reported and left in place"
    );
    assert!(
        replaced > 0,
        "the corpus must plant at least one real credential"
    );
    assert!(
        reported_only > 0,
        "the corpus must plant at least one contact detail, or the posture above \
         is untested"
    );
    let _ = RedactionKind::ALL;
}

/// The number this whole exercise exists to produce: what fraction of a real
/// household's context can be in the prompt at once.
///
/// The budget is not a free parameter -- `CompactionProfile::from_context_window`
/// sets `memory_token_budget`, `split_preamble_budget` gives the context corpus
/// a third of it, and `select_within_budget` spends that third. So the answer on
/// the Orin's pinned 16,384 window is arithmetic, and it is small.
///
/// Ranking here runs with `similarity: None` for every item, which is not a
/// limitation of the harness: it is the state of the deployed pond. The only
/// production `EmbeddingProvider` does not initialise on the Orin, so
/// `relevance_score` falls back to proximity and ingest recency -- exactly what
/// this measures.
#[tokio::test]
async fn a_households_context_does_not_fit_a_jetson_preamble() {
    use pond_core::models::services::context::context_budget::CompactionProfile;

    let (stored, _) = ingest_corpus().await;
    let now = corpus_now();

    let total_tokens: usize = stored.iter().map(estimated_tokens).sum();
    println!("\n-- corpus cost ----------------------------------------------");
    println!(
        "  {} ingestible items, {total_tokens} tokens rendered in full \
         ({:.1} tokens/item)",
        stored.len(),
        total_tokens as f32 / stored.len() as f32
    );

    let mut ranked: Vec<(ContextItem, Option<f32>)> =
        stored.iter().cloned().map(|i| (i, None)).collect();
    rank_by_relevance(&mut ranked, now);

    println!("\n-- what fits, by context window -----------------------------");
    for window in [4_096usize, 8_192, 16_384, 32_768] {
        let profile = CompactionProfile::from_context_window(window);
        let split = split_preamble_budget(profile.memory_token_budget, true);
        let kept = select_within_budget(&ranked, split.context_tokens);
        let spent: usize = kept.iter().map(|i| estimated_tokens(i)).sum();
        println!(
            "  window {:>6}  memory budget {:>4}  context share {:>4}  \
             fits {:>3} of {} items ({:.1}%), spends {:>4}",
            window,
            profile.memory_token_budget,
            split.context_tokens,
            kept.len(),
            stored.len(),
            kept.len() as f32 * 100.0 / stored.len() as f32,
            spent,
        );
        assert!(
            !kept.is_empty(),
            "no item fit a {window} window; select_within_budget promises at least one"
        );
    }

    // The Jetson tier, printed in full, because "eleven items" is abstract and
    // the eleven lines are not. This is what the model would actually see.
    let jetson = CompactionProfile::from_context_window(16_384);
    let split = split_preamble_budget(jetson.memory_token_budget, true);
    let kept = select_within_budget(&ranked, split.context_tokens);
    println!("\n-- the block, at the Orin's pinned 16,384 window -------------");
    println!("{}", render_block(&kept));

    let by_kind = kept
        .iter()
        .fold(BTreeMap::new(), |mut m: BTreeMap<&str, usize>, i| {
            *m.entry(i.source_kind().as_str()).or_default() += 1;
            m
        });
    println!("\n  kinds represented in the block: {by_kind:?}");
    println!(
        "  the other {} items are reachable only by retrieval, which is the \
         argument for PAI-3 rather than a bigger slice",
        stored.len() - kept.len()
    );

    // Guards the premise, not the number: if a whole household fits the
    // preamble, retrieval is not the problem PAI-3 says it is and this corpus
    // is too small to be evidence of anything.
    assert!(
        kept.len() * 4 < stored.len(),
        "the corpus is too small to exercise a budget: {} of {} items fit",
        kept.len(),
        stored.len()
    );
}

/// The punchline, and the reason the labelled query set exists.
///
/// `queries.jsonl` names, for twenty-five plausible household questions, the
/// items that actually answer them. Recency ordering is what a pond with no
/// working embedder has -- so this asks the only question that matters about
/// the current block: if the user asked one of these, would the answer be in
/// the prompt?
///
/// No embedder is needed to measure it, which is the point. When
/// `EmbeddingProvider` works on the target hardware, recall@k over this same
/// query set is the number to compare against, and it has to beat this one.
#[tokio::test]
async fn the_recency_slice_answers_almost_none_of_the_labelled_queries() {
    use pond_core::models::services::context::context_budget::CompactionProfile;

    let (stored, _) = ingest_corpus().await;
    let now = corpus_now();

    let mut ranked: Vec<(ContextItem, Option<f32>)> =
        stored.iter().cloned().map(|i| (i, None)).collect();
    rank_by_relevance(&mut ranked, now);

    let profile = CompactionProfile::from_context_window(16_384);
    let split = split_preamble_budget(profile.memory_token_budget, true);
    let kept = select_within_budget(&ranked, split.context_tokens);
    let in_block: BTreeSet<&str> = kept.iter().map(|i| i.external_id()).collect();
    let ingestible: BTreeSet<&str> = stored.iter().map(|i| i.external_id()).collect();

    let mut hits = 0usize;
    let mut answerable = 0usize;
    println!("\n-- can the block answer the question? ------------------------");
    for line in QUERIES.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).expect("query line");
        let query = v["query"].as_str().unwrap();
        let relevant: Vec<&str> = v["relevant"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();

        // A query whose every relevant item sits behind a gated source kind is
        // not a retrieval failure; it is unanswerable at any budget today.
        let reachable: Vec<&&str> = relevant
            .iter()
            .filter(|r| ingestible.contains(**r))
            .collect();
        if reachable.is_empty() {
            println!("  n/a  {query}  (every relevant item is on a gated source)");
            continue;
        }
        answerable += 1;
        let hit = reachable.iter().any(|r| in_block.contains(**r));
        if hit {
            hits += 1;
        }
        println!("  {}  {query}", if hit { "HIT " } else { "miss" });
    }

    println!(
        "\n  {hits} of {answerable} answerable queries have an answer in the block \
         ({:.0}%)",
        hits as f32 * 100.0 / answerable as f32
    );
    println!(
        "  the block holds {} of {} ingestible items, chosen by recency alone",
        kept.len(),
        stored.len()
    );

    // Guards the premise rather than the number. If a recency slice ever answers
    // most of this query set, either the corpus stopped being realistic or the
    // budget stopped being tight -- and in both cases the retrieval argument
    // this fixture exists to support would need re-making from scratch.
    assert!(
        hits * 2 < answerable,
        "a recency-ordered slice answered {hits} of {answerable} labelled queries; \
         that is too many for this corpus to be evidence that retrieval is needed"
    );
}
