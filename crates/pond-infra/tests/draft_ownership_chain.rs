//! The draft-ownership chain, end to end, against real SQLite.
//!
//! This test exists because of a recorded failure in this programme, not
//! because the unit tests looked thin. `ProfileScope::Owner` was a no-op in
//! production for a whole phase while every test of it passed, because every
//! fixture set the owner column by hand and no production code path ever did.
//! `RepoDraftAuthority` is the only thing that can stamp a draft's owner in
//! production, and it is stubbed out in every `pond-mcp-server` unit test. If
//! the chain it walks is broken at any link, `save_draft` stamps NULL forever,
//! `is_draft_decision_permitted` never reaches its `Some(owner)` arm, and every
//! deny test in `policy.rs` still passes.
//!
//! So walk the whole chain through the writers production actually uses:
//! `ProfileRepository::create`, `SessionStorage::create_session`,
//! `set_session_identity_if_stronger` (what `PUT /sessions/:id/user` calls),
//! and `set_engine_session_id` (what `GooseAdapter::remember_goose_session`
//! calls on every turn). Nothing here writes a column directly.

use std::sync::Arc;

use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::security::ports::policy::{
    is_draft_decision_permitted, PolicyMode, REASON_FOREIGN_DRAFT, REASON_UNRESOLVED_ACTOR,
};
use pond_core::security::services::draft_authority::RepoDraftAuthority;
use pond_core::user_data::domain::profile::{CreateProfileRequest, ProfileScope};
use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_infra::db::Database;
use pond_infra::sqlite_profile::SqliteProfileRepository;
use pond_infra::sqlite_session_storage::SqliteSessionStorage;
use pond_infra::sqlite_settings::SqliteSettingsRepository;

struct Fixture {
    authority: RepoDraftAuthority,
    liz: String,
    jerry: String,
    _tmp: tempfile::TempDir,
}

/// Two household members, a session bound to one of them the way the REST route
/// binds it, and that session paired to an engine session the way every turn
/// pairs it.
async fn two_member_pond(engine_session_id: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::init(tmp.path()).await.unwrap();

    let profiles = Arc::new(SqliteProfileRepository::new(db.system.clone()));
    let sessions = Arc::new(SqliteSessionStorage::new(db.system.clone()));
    let settings = Arc::new(SqliteSettingsRepository::new(db.system.clone()));

    let liz = profiles
        .create(CreateProfileRequest {
            display_name: "Liz".to_string(),
            avatar_emoji: "L".to_string(),
        })
        .await
        .unwrap()
        .id;
    let jerry = profiles
        .create(CreateProfileRequest {
            display_name: "Jerry".to_string(),
            avatar_emoji: "J".to_string(),
        })
        .await
        .unwrap()
        .id;

    sessions
        .create_session("giap-sess-1".to_string())
        .await
        .unwrap();
    let bound = sessions
        .set_session_identity_if_stronger(
            "giap-sess-1",
            &SessionIdentity {
                profile_id: Some(liz.clone()),
                source: IdentificationSource::Explicit,
                confidence: None,
            },
        )
        .await
        .unwrap();
    assert!(bound, "the identity write must actually land");

    sessions
        .set_engine_session_id("giap-sess-1", engine_session_id)
        .await
        .unwrap();

    Fixture {
        authority: RepoDraftAuthority::new(settings, sessions, profiles, None),
        liz,
        jerry,
        _tmp: tmp,
    }
}

/// The link that decides whether any of this is real: an engine session id, of
/// the shape the MCP request `_meta` carries, resolving to a NAMED member.
///
/// If this returns `Household` or `None`, `save_draft` writes `profile_id`
/// NULL on every pond and the ownership rule is decoration.
#[tokio::test]
async fn an_engine_session_resolves_to_the_member_who_owns_the_giap_session() {
    let f = two_member_pond("20260805_9").await;

    let actor = f.authority.actor_for_engine_session("20260805_9").await;
    let (scope, source) = actor.expect("the engine session must resolve to a speaker");

    assert_eq!(
        scope,
        ProfileScope::Owner(f.liz.clone()),
        "a two-member pond with an explicitly bound session must name the member"
    );
    assert_eq!(
        source,
        IdentificationSource::Explicit,
        "provenance must survive the whole chain: PAI-1 invariant 3"
    );

    // ...and that is a scope the rule's Some(owner) arm actually discriminates
    // on, in both directions.
    assert_eq!(
        is_draft_decision_permitted(
            Some(&scope),
            "20260805_9",
            Some(f.jerry.as_str()),
            "20260805_9"
        ),
        Err(REASON_FOREIGN_DRAFT),
        "the reported hole, with a scope resolved by production code"
    );
    assert_eq!(
        is_draft_decision_permitted(
            Some(&scope),
            "20260805_9",
            Some(f.liz.as_str()),
            "20260805_9"
        ),
        Ok(())
    );
}

/// Every way the chain can fail must produce a refusal, never a permission.
/// An engine session nobody mapped is the common case on an upgraded pond,
/// where `engine_session_map` has rows only for sessions seen since the map
/// landed in 0032.
#[tokio::test]
async fn an_unmapped_or_blank_engine_session_refuses_rather_than_widens() {
    let f = two_member_pond("20260805_9").await;

    for engine in ["20260805_nope", "", "   "] {
        let actor = f.authority.actor_for_engine_session(engine).await;
        assert!(
            actor.is_none(),
            "engine session {engine:?} must be unresolvable, got {actor:?}"
        );
        assert_eq!(
            is_draft_decision_permitted(None, engine, Some(f.liz.as_str()), "giap-sess-1"),
            Err(REASON_UNRESOLVED_ACTOR)
        );
    }
}

/// The mode is read from settings on every decision. Default is audit, and a
/// pond that has never been configured must not silently be in `off`.
#[tokio::test]
async fn the_mode_comes_from_settings_and_defaults_to_audit() {
    let f = two_member_pond("20260805_9").await;
    assert_eq!(f.authority.policy_mode().await, PolicyMode::Audit);
}
