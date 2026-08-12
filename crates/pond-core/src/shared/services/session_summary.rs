//! Idle rolling-summary refresh — the soft-quality half of hybrid compaction.
//!
//! A background loop (pond-server) calls [`SessionSummaryService::refresh`]
//! when the system has been idle past `summary_idle_secs`, never at startup,
//! and cancels it the moment a new turn starts — the same contract as memory
//! consolidation. The refreshed summary lives on the session row
//! (`sessions.rolling_summary`); the deterministic turn trimmer splices it
//! into the model's history as a `<conversation-summary>` block. Turns only
//! ever READ the summary; they never wait for one.
//!
//! # Two ways to produce that column, and one owner
//!
//! [`SessionSummaryService::refresh`] is the incremental one, and it runs on
//! every tier: previous summary plus the messages it does not cover, folded into
//! 3-5 sentences. [`SessionSummaryService::resummarise`] is PAI-4 P2's, and it
//! runs only on [`ModelClass::Large`]: it throws the chain away and rebuilds
//! from the source messages with a budget the window can afford. Both live here
//! because both write `sessions.rolling_summary`, and a second file writing that
//! column is exactly the drift this programme keeps paying for. The *decision*
//! to rebuild is not here — it is a pure gate in
//! [`models::services::context::resummarisation`](crate::models::services::context::resummarisation).

use std::sync::Arc;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::models::domain::message::{ChatMessage, Role};
use crate::models::ports::provider::LlmProvider;
use crate::models::ports::token_counter::TokenCounter;
use crate::models::services::context::context_budget::CompactionProfile;
use crate::models::services::context::model_class::ModelClass;
use crate::models::services::context::resummarisation::{
    budget_as_words, newest_affordable_start, should_resummarise, source_budget_tokens,
    GateDecision, ResummariseGateInputs, SkipReason,
};
use crate::models::services::context::token_counting::PER_MESSAGE_TOKEN_OVERHEAD;
use crate::user_data::ports::session_storage::SessionStorage;

/// Keep this many of the newest messages OUT of the summary — they stay
/// verbatim in the model's history, so summarizing them would duplicate.
const KEEP_RECENT_MESSAGES: usize = 6;

/// Don't bother refreshing for fewer than this many new messages.
const MIN_UNSUMMARIZED_MESSAGES: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Summary refreshed and persisted; covers through the given message id.
    Refreshed { through_message_id: String },
    /// Not enough unsummarized history to justify a refresh.
    NothingToDo,
    /// A new turn started; the refresh was abandoned before persisting.
    Cancelled,
}

/// What one large-tier re-summarisation pass did. PAI-4 P2.
#[derive(Debug, PartialEq, Eq)]
pub enum ResummariseOutcome {
    /// The summary was rebuilt from source and persisted, replacing a summary of
    /// `replaced_tokens` with one of `tokens`. The through-pointer is unchanged:
    /// a rebuild changes the *fidelity* of the covered span, never which
    /// messages are claimed to be covered.
    Rebuilt {
        tokens: usize,
        replaced_tokens: usize,
        source_messages: usize,
    },
    /// The model came back with something no longer than what is already
    /// stored, so nothing was persisted.
    ///
    /// This is not a cosmetic check. The gate fires only when the budget allows
    /// a doubling, so a rebuild that comes back shorter has not used the
    /// headroom the model call was spent on — and persisting it would both
    /// *destroy* record (a good summary replaced by "Summary.") and leave the
    /// gate open, so the pass would re-fire forever. One guard closes both.
    NotBetter {
        tokens: usize,
        replaced_tokens: usize,
    },
    /// The gate said no. Carries the first thing that was wrong.
    Skipped(SkipReason),
    /// A new turn started; the rebuild was abandoned before persisting.
    Cancelled,
}

/// How a role is spelled in a summarisation transcript.
///
/// Shared by both producers below so the two prompts describe a conversation the
/// same way — the model is being asked to merge one into the other, and two
/// spellings of "Assistant" would be a difference it has to reconcile.
fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "User",
        Role::Assistant => "Assistant",
        Role::System => "System",
        Role::Tool => "Tool",
    }
}

pub struct SessionSummaryService {
    provider: Arc<dyn LlmProvider>,
    storage: Arc<dyn SessionStorage>,
}

impl SessionSummaryService {
    pub fn new(provider: Arc<dyn LlmProvider>, storage: Arc<dyn SessionStorage>) -> Self {
        Self { provider, storage }
    }

    /// Merge the existing summary with the messages it does not yet cover
    /// (excluding the newest [`KEEP_RECENT_MESSAGES`]) into a refreshed 3-5
    /// sentence summary, and persist it with an advanced through-pointer.
    ///
    /// Checks `cancel` before and after the model call; a cancelled refresh
    /// persists nothing.
    pub async fn refresh(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RefreshOutcome> {
        if cancel.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        let (old_summary, through_id) = self
            .storage
            .get_rolling_summary(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("read rolling summary: {e}"))?;

        let messages = self
            .storage
            .get_messages(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("read session messages: {e}"))?;

        // Messages after the through-pointer, excluding the recent tail.
        let start = match &through_id {
            Some(id) => messages
                .iter()
                .position(|m| &m.id == id)
                .map(|i| i + 1)
                .unwrap_or(0),
            None => 0,
        };
        let end = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
        if end <= start || end - start < MIN_UNSUMMARIZED_MESSAGES {
            return Ok(RefreshOutcome::NothingToDo);
        }
        let window = &messages[start..end];
        let new_through_id = window.last().expect("window is non-empty").id.clone();

        let mut transcript = String::new();
        if let Some(old) = &old_summary {
            transcript.push_str("Existing summary of even earlier turns:\n");
            transcript.push_str(old);
            transcript.push_str("\n\nNewer turns to fold in:\n");
        }
        for m in window {
            transcript.push_str(&format!(
                "{}: {}\n",
                role_label(&m.message.role),
                m.message.content
            ));
        }

        let prompt = vec![ChatMessage::user(format!(
            "Merge the existing summary (if any) and the newer turns below into ONE \
             accurate summary of the conversation so far. Preserve important facts, \
             names, decisions, and open questions. Write 3-5 sentences maximum, \
             plain prose, no preamble.\n\n{transcript}"
        ))];
        let system =
            "You summarise conversations accurately and concisely for use as model context.";

        // The model call is the long pole; race it against cancellation so a
        // new turn reclaims the (serial, on-device) engine immediately.
        let response = tokio::select! {
            r = self.provider.complete(system, prompt) => r?,
            _ = cancel.cancelled() => return Ok(RefreshOutcome::Cancelled),
        };

        if cancel.is_cancelled() {
            return Ok(RefreshOutcome::Cancelled);
        }

        let summary = response.content.trim().to_string();
        if summary.is_empty() {
            return Ok(RefreshOutcome::NothingToDo);
        }
        self.storage
            .set_rolling_summary(session_id, &summary, &new_through_id)
            .await
            .map_err(|e| anyhow::anyhow!("persist rolling summary: {e}"))?;

        Ok(RefreshOutcome::Refreshed {
            through_message_id: new_through_id,
        })
    }

    /// PAI-4 P2 — rebuild the rolling summary from the source messages, for the
    /// large tier only.
    ///
    /// [`refresh`](Self::refresh) is a chain: every pass feeds the model its own
    /// previous output plus whatever is new, so detail lost on one pass never
    /// comes back. This throws the chain away and re-reads the conversation,
    /// spending a budget derived from the resolved window rather than the "3-5
    /// sentences" that has to be affordable on a Jetson. The rebuilt summary
    /// replaces the stored one **under the same through-pointer**: this changes
    /// how faithfully the covered span is represented, never which messages are
    /// claimed to be covered.
    ///
    /// # What holds invariant 1
    ///
    /// Nothing here — and that is deliberate. This method awaits a model call
    /// and must never be reached from a request handler or an SSE generator. The
    /// production caller is `run_compaction_pass` in `pond-api`, which is only
    /// ever entered from a detached task, holds the session's in-flight claim
    /// for the whole pass, and hands down a `cancel` token wired to
    /// `last_user_activity` so a user turn reclaims the engine immediately. A
    /// cancelled rebuild persists nothing.
    ///
    /// # Which failures cost what
    ///
    /// Every one of them resolves to "leave the stored summary exactly as it
    /// is", which is the behaviour every tier had before this phase. The design
    /// asks for a fall back to "a deterministic trim on any LLM failure so a
    /// turn is never interrupted" — and at this seam that fallback is structural
    /// rather than a branch to write: the deterministic trimmer runs on every
    /// turn regardless of what happened here, and this pass is not on a turn's
    /// path at all. Saying so is more honest than adding a call to the trimmer
    /// that nothing would be waiting on.
    pub async fn resummarise(
        &self,
        session_id: &str,
        class: ModelClass,
        profile: &CompactionProfile,
        counter: &dyn TokenCounter,
        cancel: &CancellationToken,
    ) -> Result<ResummariseOutcome> {
        // First, and before any I/O: on the two on-device tiers this method
        // costs not even a database read (PAI-4 invariant 3).
        if !class.permits_compaction_model_call() {
            return Ok(ResummariseOutcome::Skipped(
                SkipReason::TierForbidsModelCall,
            ));
        }
        if cancel.is_cancelled() {
            return Ok(ResummariseOutcome::Cancelled);
        }

        let (old_summary, through_id) = self
            .storage
            .get_rolling_summary(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("read rolling summary: {e}"))?;
        let (Some(old_summary), Some(through_id)) = (old_summary, through_id) else {
            // The incremental refresh produces this mechanism's input. Nothing
            // to rebuild until it has run at least once.
            return Ok(ResummariseOutcome::Skipped(SkipReason::NoSummaryYet));
        };
        let summary_tokens = counter.count(&old_summary);
        if summary_tokens == 0 {
            return Ok(ResummariseOutcome::Skipped(SkipReason::NoSummaryYet));
        }

        let messages = self
            .storage
            .get_messages(session_id)
            .await
            .map_err(|e| anyhow::anyhow!("read session messages: {e}"))?;
        let Some(idx) = messages.iter().position(|m| m.id == through_id) else {
            return Ok(ResummariseOutcome::Skipped(SkipReason::NothingCovered));
        };
        let covered = &messages[..=idx];

        // Counted WITH the per-message envelope, because the budget this is
        // compared against is the trimmer's and the trimmer counts it too. Two
        // sides of one comparison have to be measured the same way.
        let per_message: Vec<usize> = covered
            .iter()
            .map(|m| counter.count(&m.message.content) + PER_MESSAGE_TOKEN_OVERHEAD)
            .collect();
        let covered_tokens: usize = per_message.iter().sum();

        let budget_tokens = match should_resummarise(ResummariseGateInputs {
            class,
            summary_tokens,
            covered_tokens,
            history_token_budget: profile.history_token_budget,
        }) {
            GateDecision::Run { budget_tokens } => budget_tokens,
            GateDecision::Skip(reason) => return Ok(ResummariseOutcome::Skipped(reason)),
        };

        let start =
            newest_affordable_start(&per_message, source_budget_tokens(profile, budget_tokens));
        if start >= covered.len() {
            return Ok(ResummariseOutcome::Skipped(
                SkipReason::SourceTooLargeToRead,
            ));
        }
        let source = &covered[start..];

        let mut transcript = String::new();
        if start > 0 {
            // The span did not fit, so the oldest part of it is still
            // represented only by the summary being replaced. Carrying it into
            // the prompt is what keeps a bounded rebuild from LOSING record
            // relative to the chain it is improving on.
            transcript.push_str("Summary of even earlier turns, not reproduced below:\n");
            transcript.push_str(&old_summary);
            transcript.push_str("\n\n");
        }
        transcript.push_str("Conversation transcript:\n");
        for m in source {
            transcript.push_str(&format!(
                "{}: {}\n",
                role_label(&m.message.role),
                m.message.content
            ));
        }

        let words = budget_as_words(budget_tokens);
        let prompt = vec![ChatMessage::user(format!(
            "Write ONE summary of the conversation below, working from the transcript \
             itself rather than from any shorter summary of it. Preserve every fact, \
             name, number, decision, preference and open question a later turn could \
             need. No preamble, no commentary. Write at most {words} words.\n\n{transcript}"
        ))];
        let system =
            "You summarise conversations accurately and in detail, for use as model context.";

        let response = tokio::select! {
            r = self.provider.complete(system, prompt) => r?,
            _ = cancel.cancelled() => return Ok(ResummariseOutcome::Cancelled),
        };
        if cancel.is_cancelled() {
            return Ok(ResummariseOutcome::Cancelled);
        }

        let summary = response.content.trim().to_string();
        let tokens = counter.count(&summary);
        if tokens <= summary_tokens {
            // See `ResummariseOutcome::NotBetter`: this is both the guard
            // against replacing a good summary with a worse one and the reason
            // the gate cannot latch open.
            return Ok(ResummariseOutcome::NotBetter {
                tokens,
                replaced_tokens: summary_tokens,
            });
        }

        self.storage
            .set_rolling_summary(session_id, &summary, &through_id)
            .await
            .map_err(|e| anyhow::anyhow!("persist rebuilt summary: {e}"))?;

        Ok(ResummariseOutcome::Rebuilt {
            tokens,
            replaced_tokens: summary_tokens,
            source_messages: source.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::session::SessionMessage;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct StubProvider {
        reply: String,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LlmProvider for StubProvider {
        async fn complete(
            &self,
            _system_prompt: &str,
            messages: Vec<ChatMessage>,
        ) -> Result<ChatMessage> {
            self.calls.lock().unwrap().push(
                messages
                    .first()
                    .map(|m| m.content.clone())
                    .unwrap_or_default(),
            );
            Ok(ChatMessage::assistant(self.reply.clone()))
        }

        fn model_name(&self) -> String {
            "stub".to_string()
        }
    }

    async fn seed(storage: &InMemorySessionStorage, session: &str, n: usize) {
        storage.create_session(session.to_string()).await.unwrap();
        for i in 0..n {
            let role_user = i % 2 == 0;
            let msg = if role_user {
                ChatMessage::user(format!("question {i}"))
            } else {
                ChatMessage::assistant(format!("answer {i}"))
            };
            storage
                .add_message(
                    session.to_string(),
                    SessionMessage::new(format!("m{i}"), session.to_string(), msg),
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn refresh_summarizes_and_advances_pointer() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 12).await;
        let provider = Arc::new(StubProvider {
            reply: "They discussed twelve things.".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc.refresh("s", &CancellationToken::new()).await.unwrap();
        // 12 messages - 6 recent = window of 6 → through m5.
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                through_message_id: "m5".to_string()
            }
        );
        let (summary, through) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(summary.as_deref(), Some("They discussed twelve things."));
        assert_eq!(through.as_deref(), Some("m5"));
    }

    #[tokio::test]
    async fn refresh_merges_existing_summary_into_prompt() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 16).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks were discussed.", "m3")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "Ducks and geese.".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc.refresh("s", &CancellationToken::new()).await.unwrap();
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                through_message_id: "m9".to_string()
            }
        );
        let sent = provider.calls.lock().unwrap();
        assert!(sent[0].contains("Earlier: ducks were discussed."));
        assert!(sent[0].contains("question 4"));
    }

    #[tokio::test]
    async fn too_little_new_history_is_a_noop() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 8).await; // 8 - 6 recent = 2 < MIN 4
        let provider = Arc::new(StubProvider {
            reply: "unused".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());
        let outcome = svc.refresh("s", &CancellationToken::new()).await.unwrap();
        assert_eq!(outcome, RefreshOutcome::NothingToDo);
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    // -- PAI-4 P2: large-tier re-summarisation -------------------------------
    //
    // "And add the first test that actually executes it" is the clause the P2
    // respec kept verbatim, because 277 lines of `ContextCompactor` passed every
    // gate this repository has for as long as it existed and had no caller. So
    // these drive the real method against a real storage adapter and a stub
    // provider, and every one of them asserts what reached the provider or what
    // reached the store — never merely that a function returned.

    use crate::models::services::context::context_budget::CompactionProfile;
    use crate::models::services::context::model_class::ModelClass;
    use crate::models::services::context::token_counting::HeuristicTokenCounter;

    /// The large tier's floor, so the budgets under test are the smallest ones
    /// this mechanism is ever handed rather than the roomiest.
    fn large_profile() -> CompactionProfile {
        CompactionProfile::from_context_window(65_536)
    }

    /// Seed `n` messages of roughly `chars` characters each. The default `seed`
    /// helper produces messages of a dozen characters, which cannot get a
    /// session over a 20,000-token history budget in any realistic number of
    /// rows — and the gate under test is entirely about crossing that budget.
    ///
    /// The body is padded to a known length rather than left to `format!` so the
    /// fixture's token count is a property of the test rather than of how many
    /// digits the loop index happens to have.
    async fn seed_bulky(storage: &InMemorySessionStorage, session: &str, n: usize, chars: usize) {
        storage.create_session(session.to_string()).await.unwrap();
        for i in 0..n {
            let body = "lorem ipsum ".repeat(chars / 12);
            let msg = if i % 2 == 0 {
                ChatMessage::user(format!("question {i}: {body}"))
            } else {
                ChatMessage::assistant(format!("answer {i}: {body}"))
            };
            storage
                .add_message(
                    session.to_string(),
                    SessionMessage::new(format!("m{i}"), session.to_string(), msg),
                )
                .await
                .unwrap();
        }
    }

    /// The phase, executed. A long-lived large-tier session whose covered span
    /// has outgrown the history budget gets its summary rebuilt FROM THE SOURCE
    /// MESSAGES — which is the whole difference from `refresh`, so the test
    /// asserts the transcript that reached the model, not just the outcome.
    #[tokio::test]
    async fn a_large_tier_session_rebuilds_its_summary_from_the_source_messages() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed_bulky(&storage, "s", 200, 500).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks were discussed.", "m199")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "A much longer and more faithful account of everything that was \
                    said, naming the ducks, the geese and every open question."
                .to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        let ResummariseOutcome::Rebuilt {
            tokens,
            replaced_tokens,
            source_messages,
        } = outcome
        else {
            panic!("a saturated large-tier session did not rebuild: {outcome:?}");
        };
        assert_eq!(
            source_messages, 200,
            "the whole covered span fits the large tier's source budget, so a bounded \
             read means the budget arithmetic is wrong"
        );
        assert!(tokens > replaced_tokens);

        let sent = provider.calls.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "a rebuild is exactly one model call");
        assert!(
            sent[0].contains("question 0:") && sent[0].contains("answer 199:"),
            "the prompt did not carry the source transcript, so this rebuilt the \
             summary from the chain it exists to escape"
        );
        assert!(
            !sent[0].contains("Earlier: ducks were discussed."),
            "the whole span fit, so the summary being replaced must not be fed back in \
             — that is the lossy chain, reintroduced"
        );

        // Persisted under the SAME through-pointer: fidelity changed, coverage
        // did not.
        let (summary, through) = storage.get_rolling_summary("s").await.unwrap();
        assert!(summary.as_deref().unwrap().starts_with("A much longer"));
        assert_eq!(through.as_deref(), Some("m199"));
    }

    /// Invariant 3, executed rather than asserted about a gate in isolation:
    /// neither on-device tier reaches the provider, and neither touches the
    /// store. The fixture is the one that DOES rebuild above, so the only
    /// difference is the tier.
    #[tokio::test]
    async fn the_on_device_tiers_never_reach_the_provider_or_the_store() {
        for class in [ModelClass::Small, ModelClass::Medium] {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed_bulky(&storage, "s", 200, 500).await;
            storage
                .set_rolling_summary("s", "Earlier: ducks were discussed.", "m199")
                .await
                .unwrap();
            let provider = Arc::new(StubProvider {
                reply: "should never be asked for".to_string(),
                calls: Mutex::new(Vec::new()),
            });
            let svc = SessionSummaryService::new(provider.clone(), storage.clone());

            let outcome = svc
                .resummarise(
                    "s",
                    class,
                    &large_profile(),
                    &HeuristicTokenCounter,
                    &CancellationToken::new(),
                )
                .await
                .unwrap();

            assert_eq!(
                outcome,
                ResummariseOutcome::Skipped(SkipReason::TierForbidsModelCall),
                "the {} tier was allowed into the re-summarisation path",
                class.label()
            );
            assert!(
                provider.calls.lock().unwrap().is_empty(),
                "the {} tier made a compaction model call that competes with the next \
                 turn's prefill (PAI-4 invariant 3)",
                class.label()
            );
            let (summary, _) = storage.get_rolling_summary("s").await.unwrap();
            assert_eq!(summary.as_deref(), Some("Earlier: ducks were discussed."));
        }
    }

    /// A short session on a large model is left alone: the trimmer can still
    /// carry the covered span verbatim, so a model call would buy fidelity
    /// nobody is missing.
    #[tokio::test]
    async fn a_span_that_still_fits_the_history_budget_costs_no_model_call() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed_bulky(&storage, "s", 12, 500).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks.", "m11")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "unused".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ResummariseOutcome::Skipped(SkipReason::SpanStillFitsHistory)
        );
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    /// The incremental refresh produces this mechanism's input. With no summary
    /// stored there is nothing to rebuild, and inventing one here would be a
    /// second implementation of `refresh` on a different trigger.
    #[tokio::test]
    async fn a_session_with_no_summary_yet_is_not_a_rebuild_opportunity() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed_bulky(&storage, "s", 200, 500).await;
        let provider = Arc::new(StubProvider {
            reply: "unused".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());
        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ResummariseOutcome::Skipped(SkipReason::NoSummaryYet)
        );
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    /// The guard that keeps a bad model response from destroying the record —
    /// and, by the same branch, keeps the gate from latching open and
    /// re-firing on every pass forever.
    #[tokio::test]
    async fn a_rebuild_no_longer_than_what_it_replaces_is_not_persisted() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed_bulky(&storage, "s", 200, 500).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks were discussed at length.", "m199")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "Summary.".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            matches!(outcome, ResummariseOutcome::NotBetter { .. }),
            "a one-word rebuild was accepted: {outcome:?}"
        );
        let (summary, through) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(
            summary.as_deref(),
            Some("Earlier: ducks were discussed at length."),
            "a worse summary replaced a better one"
        );
        assert_eq!(through.as_deref(), Some("m199"));
    }

    /// A covered span too big for the prompt budget still improves on the chain
    /// rather than losing to it: the newest affordable suffix is re-read from
    /// source and the summary being replaced is carried in to cover the rest.
    #[tokio::test]
    async fn an_oversized_span_keeps_its_newest_end_and_carries_the_old_summary() {
        let storage = Arc::new(InMemorySessionStorage::new());
        // ~150 x 2,000 chars = 300,000 chars = ~75,000 tokens, past the large
        // tier floor's ~62,000-token source budget.
        seed_bulky(&storage, "s", 150, 2_000).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks were discussed.", "m149")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "A long, faithful and detailed account of the whole conversation, \
                    including the ducks and the geese and what remains undecided."
                .to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());

        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        let ResummariseOutcome::Rebuilt {
            source_messages, ..
        } = outcome
        else {
            panic!("expected a bounded rebuild, got {outcome:?}");
        };
        assert!(
            source_messages > 0 && source_messages < 150,
            "the span was supposed to overflow the source budget; {source_messages} of \
             150 messages were read"
        );
        let sent = provider.calls.lock().unwrap();
        assert!(
            sent[0].contains("Earlier: ducks were discussed."),
            "the truncated head was dropped without the old summary standing in for it, \
             so a bounded rebuild LOST record relative to the chain it improves on"
        );
        assert!(
            sent[0].contains("answer 149:"),
            "the newest end of the span was not the end that survived"
        );
        assert!(
            !sent[0].contains("question 0:"),
            "the oldest message survived a span that was supposed to be truncated"
        );
    }

    #[tokio::test]
    async fn a_cancelled_rebuild_persists_nothing() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed_bulky(&storage, "s", 200, 500).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks.", "m199")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "should never land".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());
        let cancel = CancellationToken::new();
        cancel.cancel();

        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(outcome, ResummariseOutcome::Cancelled);
        assert!(provider.calls.lock().unwrap().is_empty());
        let (summary, _) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(summary.as_deref(), Some("Earlier: ducks."));
    }

    /// A through-pointer naming a message that is not in the session degrades to
    /// "leave it alone" rather than re-summarising an empty span into the column.
    #[tokio::test]
    async fn a_dangling_through_pointer_leaves_the_summary_alone() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed_bulky(&storage, "s", 200, 500).await;
        storage
            .set_rolling_summary("s", "Earlier: ducks.", "not-a-message-id")
            .await
            .unwrap();
        let provider = Arc::new(StubProvider {
            reply: "unused".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider.clone(), storage.clone());
        let outcome = svc
            .resummarise(
                "s",
                ModelClass::Large,
                &large_profile(),
                &HeuristicTokenCounter,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            ResummariseOutcome::Skipped(SkipReason::NothingCovered)
        );
        assert!(provider.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_refresh_persists_nothing() {
        let storage = Arc::new(InMemorySessionStorage::new());
        seed(&storage, "s", 12).await;
        let provider = Arc::new(StubProvider {
            reply: "should never land".to_string(),
            calls: Mutex::new(Vec::new()),
        });
        let svc = SessionSummaryService::new(provider, storage.clone());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = svc.refresh("s", &cancel).await.unwrap();
        assert_eq!(outcome, RefreshOutcome::Cancelled);
        let (summary, _) = storage.get_rolling_summary("s").await.unwrap();
        assert_eq!(summary, None);
    }
}
