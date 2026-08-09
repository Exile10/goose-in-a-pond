//! What a live turn is authorised to delegate — PAI-6 P3.
//!
//! PAI-6 P1 made a [`DelegationAuthority`] impossible to forge: it has no
//! `Default`, no `Deserialize` and no field-wise constructor, so the only way to
//! hold one is to be handed one. That is only worth anything if the real one —
//! the parent turn's — is reachable from the place a `delegate` call arrives.
//! It is not: an MCP tool handler runs in `pond-mcp-server`, is given the
//! caller's ENGINE session id in `_meta`, and has no access to the adapter's
//! per-turn locals.
//!
//! This is the seam between the two. The adapter publishes the turn's authority
//! here as it starts the turn, keyed by the engine session id; a tool handler
//! looks it up by the id the engine stamped.
//!
//! # Why the engine session id is the right key
//!
//! It is the only session identity a tool call carries that the model cannot
//! choose. `inject_session_context_into_extensions` in the engine `retain`s away
//! any caller-supplied `agent-session-id` (case-insensitively) and re-inserts
//! the value from its own task-local before the request leaves. So a model that
//! emits a session id in its arguments changes nothing, and a subagent's calls
//! carry the CHILD's id rather than the parent's — which is what makes "is this
//! caller allowed to delegate" answerable at all.
//!
//! # Why entries are leased rather than overwritten
//!
//! An authority outliving its turn is a stale answer to an authorisation
//! question, and this programme's rule is that access narrows on failure.
//! [`TurnAuthorityRegistry::publish`] returns a [`TurnAuthorityLease`] whose
//! `Drop` revokes the entry, so a turn that ends — normally, by cancellation, or
//! by the client hanging up and the stream being dropped — takes its delegation
//! authority with it. The lookup then answers `None`, and P1's contract is that
//! `None` means refuse.
//!
//! # What is deliberately NOT here
//!
//! Persistence. An authority is derived from a turn's resolved scope and its
//! post-selection tool set, both of which are recomputed on every turn from
//! rows that ARE persisted. Writing one down would create a second source of
//! truth for an authorisation input, which is how one gets widened.

use crate::shared::domain::orchestration::DelegationAuthority;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock, Weak};
use tokio_util::sync::CancellationToken;

/// How many live turns keep a published authority.
///
/// Concurrent chat streams are bounded far below this by the SSE semaphore, so
/// in practice the map holds a handful of entries and the lease keeps it that
/// way. The cap exists so a leaked lease cannot grow the map for the lifetime of
/// a home server that is never restarted.
///
/// Eviction only ever removes the ABILITY to delegate, never widens one, so
/// evicting the wrong entry costs a refused delegation rather than an
/// unauthorised one.
pub const MAX_TRACKED_TURNS: usize = 64;

/// A live turn's delegation authority, plus the token that ends it.
#[derive(Clone)]
struct TurnEntry {
    authority: DelegationAuthority,
    cancel: CancellationToken,
}

#[derive(Default)]
struct Entries {
    by_engine_session: HashMap<String, TurnEntry>,
    order: VecDeque<String>,
}

/// Where a live turn's [`DelegationAuthority`] can be found by the engine
/// session id its tool calls carry.
#[derive(Default)]
pub struct TurnAuthorityRegistry {
    entries: RwLock<Entries>,
}

impl TurnAuthorityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish `authority` for the duration of the returned lease.
    ///
    /// `cancel` is the turn's own cancellation token. It is held here for one
    /// reason: PAI-6 invariant 5 says cancelling a parent cancels its children,
    /// and the parent's token is otherwise a stack local inside the adapter's
    /// stream closure, stored in no map and exposed by no accessor. An
    /// orchestrator that derives the child's token from
    /// [`parent_turn_token`](Self::parent_turn_token) gets that half of the
    /// invariant by construction rather than by remembering to wire a callback.
    ///
    /// Takes `&Arc<Self>` because the lease holds a [`Weak`] back: a lease that
    /// outlived the registry must be inert, not a dangling revoke.
    pub fn publish(
        self: &Arc<Self>,
        engine_session_id: &str,
        authority: DelegationAuthority,
        cancel: CancellationToken,
    ) -> TurnAuthorityLease {
        {
            let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
            let entry = TurnEntry { authority, cancel };
            if entries
                .by_engine_session
                .insert(engine_session_id.to_string(), entry)
                .is_none()
            {
                entries.order.push_back(engine_session_id.to_string());
            }
            while entries.order.len() > MAX_TRACKED_TURNS {
                if let Some(oldest) = entries.order.pop_front() {
                    entries.by_engine_session.remove(&oldest);
                }
            }
        }
        TurnAuthorityLease {
            registry: Arc::downgrade(self),
            engine_session_id: engine_session_id.to_string(),
        }
    }

    /// The authority of the live turn running in `engine_session_id`, if there
    /// is one.
    ///
    /// `None` is the answer for a subagent's own session, for a session whose
    /// turn has ended, and for an engine session GIAP never chatted in. All
    /// three must refuse, and returning the same `None` for all three is
    /// deliberate: a caller that could tell them apart would eventually treat
    /// one of them as benign.
    pub fn authority_for_engine_session(
        &self,
        engine_session_id: &str,
    ) -> Option<DelegationAuthority> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_engine_session
            .get(engine_session_id)
            .map(|entry| entry.authority.clone())
    }

    /// The cancellation token of the live turn belonging to a GIAP session.
    ///
    /// Keyed differently from [`authority_for_engine_session`](Self::authority_for_engine_session)
    /// on purpose. A `TaskSpec` carries `parent_session_id`, which is the GIAP
    /// session id, because that is what the adapter resolves an engine session
    /// from; the registry is keyed by the engine id, because that is what a tool
    /// call carries. Rather than store the pairing a third time, this scans —
    /// the map holds at most [`MAX_TRACKED_TURNS`] entries and this runs once
    /// per spawn.
    pub fn parent_turn_token(&self, giap_session_id: &str) -> Option<CancellationToken> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_engine_session
            .values()
            .find(|entry| entry.authority.session_id() == giap_session_id)
            .map(|entry| entry.cancel.clone())
    }

    /// Live turns currently published. Diagnostics and tests only.
    pub fn tracked_turns(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_engine_session
            .len()
    }

    fn revoke(&self, engine_session_id: &str) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if entries
            .by_engine_session
            .remove(engine_session_id)
            .is_some()
        {
            entries.order.retain(|id| id != engine_session_id);
        }
    }
}

/// Holds one published authority alive. Dropping it revokes.
///
/// Deliberately has no method to extend, refresh or detach. The turn owns it;
/// when the turn's value goes out of scope — including when the stream is
/// dropped because the client hung up — the authority goes with it.
pub struct TurnAuthorityLease {
    registry: Weak<TurnAuthorityRegistry>,
    engine_session_id: String,
}

impl Drop for TurnAuthorityLease {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.revoke(&self.engine_session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::profile::ProfileScope;

    fn authority(session_id: &str, scope: ProfileScope) -> DelegationAuthority {
        DelegationAuthority::for_turn(
            session_id,
            scope,
            ["giap-weather__get_forecast", "giap-knowledge__lookup"],
        )
    }

    #[test]
    fn a_published_authority_is_found_by_the_engine_session_id() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let _lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );

        let found = registry
            .authority_for_engine_session("goose-1")
            .expect("the live turn's authority must be findable");
        assert_eq!(found.session_id(), "giap-1");
        assert_eq!(found.profile_scope(), &ProfileScope::Household);
        assert!(found.tool_groups().contains("giap-weather"));
    }

    /// The three cases that must all refuse, and must refuse identically: a
    /// session that never chatted, a subagent's own session, and a turn that
    /// has ended.
    #[test]
    fn an_unknown_or_finished_turn_has_no_authority() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        assert!(registry
            .authority_for_engine_session("never-seen")
            .is_none());

        let lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );
        // A child's session id is not the parent's, so a subagent asking under
        // its own id gets nothing even while the parent's turn is live.
        assert!(registry
            .authority_for_engine_session("goose-1-child")
            .is_none());

        drop(lease);
        assert!(
            registry.authority_for_engine_session("goose-1").is_none(),
            "an authority outlived its turn - a stale answer to an authorisation question"
        );
        assert_eq!(registry.tracked_turns(), 0);
    }

    /// Vacuity control for the test above: revocation must be about the lease
    /// ending, not about the lookup never working. Proven by asserting the
    /// positive case in the same test.
    #[test]
    fn revocation_removes_only_the_turn_that_ended() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let lease_a = registry.publish(
            "goose-a",
            authority("giap-a", ProfileScope::Household),
            CancellationToken::new(),
        );
        let _lease_b = registry.publish(
            "goose-b",
            authority("giap-b", ProfileScope::Guest),
            CancellationToken::new(),
        );
        assert_eq!(registry.tracked_turns(), 2);

        drop(lease_a);
        assert!(registry.authority_for_engine_session("goose-a").is_none());
        assert!(
            registry.authority_for_engine_session("goose-b").is_some(),
            "revoking one turn took another turn's authority with it"
        );
        assert_eq!(registry.tracked_turns(), 1);
    }

    /// Invariant 5's other end: the orchestrator finds the PARENT's token by
    /// the GIAP session id a `TaskSpec` carries, so a child token derived from
    /// it dies when the parent's turn does.
    #[test]
    fn the_parents_turn_token_is_reachable_by_its_giap_session_id() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let parent = CancellationToken::new();
        let _lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            parent.clone(),
        );

        let found = registry
            .parent_turn_token("giap-1")
            .expect("a live parent turn must expose its token");
        let child = found.child_token();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(
            child.is_cancelled(),
            "cancelling the parent did not cancel a token derived from it"
        );
        assert!(registry.parent_turn_token("giap-other").is_none());
    }

    /// Re-publishing the same session must replace rather than accumulate, or
    /// a long conversation grows one entry per turn and evicts everything else.
    #[test]
    fn republishing_a_session_replaces_its_entry() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let first = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );
        let _second = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Guest),
            CancellationToken::new(),
        );
        assert_eq!(registry.tracked_turns(), 1);
        assert_eq!(
            registry
                .authority_for_engine_session("goose-1")
                .unwrap()
                .profile_scope(),
            &ProfileScope::Guest,
            "the newer turn's scope must win"
        );
        // Dropping the superseded lease still revokes, which is the narrowing
        // direction: the worst case is a refused delegation, never a stale one.
        drop(first);
        assert!(registry.authority_for_engine_session("goose-1").is_none());
    }

    #[test]
    fn the_map_is_bounded_and_eviction_only_ever_removes() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let mut leases = Vec::new();
        for i in 0..(MAX_TRACKED_TURNS + 10) {
            leases.push(registry.publish(
                &format!("goose-{i}"),
                authority(&format!("giap-{i}"), ProfileScope::Household),
                CancellationToken::new(),
            ));
        }
        assert_eq!(registry.tracked_turns(), MAX_TRACKED_TURNS);
        assert!(
            registry.authority_for_engine_session("goose-0").is_none(),
            "the oldest entry should have been evicted"
        );
        assert!(registry
            .authority_for_engine_session(&format!("goose-{}", MAX_TRACKED_TURNS + 9))
            .is_some());
    }

    /// A lease that outlives its registry must be inert rather than a dangling
    /// revoke. `Weak` makes it so; this pins that it is `Weak` and not `Arc`,
    /// because an `Arc` here would keep the registry alive forever.
    #[test]
    fn a_lease_outliving_its_registry_is_inert() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );
        let weak = Arc::downgrade(&registry);
        drop(registry);
        assert!(
            weak.upgrade().is_none(),
            "the lease is holding a strong reference and the registry can never be freed"
        );
        drop(lease);
    }
}
