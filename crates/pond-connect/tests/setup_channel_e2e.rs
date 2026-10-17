//! A phone drives the setup channel end to end over an in-memory transport: the real SQLite
//! handshake and setup record behind it, pond-infra's CPace responder in front, at the smallest
//! ATT_MTU and at a typical negotiated one.

mod common;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use common::*;
use pond_connect::setup_channel::SessionEnd;
use pond_core::network::ports::setup_code_store::SetupCodeStore;
use pond_core::security::domain::event::AttributeValue;
use pond_core::security::ports::handshake::Handshake;
use serde_json::{json, Value};

#[tokio::test]
async fn a_phone_claims_a_pond_with_its_card_and_puts_it_on_wifi() {
    for att_mtu in MTUS {
        let pond = pond().await;
        let (phone, _) = claim_and_join(&pond, att_mtu).await;
        let paired = pond
            .events
            .all()
            .into_iter()
            .find(|event| event.action == "auth.device_paired")
            .expect("the pairing is audited");
        assert_eq!(
            paired.attributes.get("method"),
            Some(&AttributeValue::from("cpace-setup"))
        );
        assert_eq!(
            paired.attributes.get("transport"),
            Some(&AttributeValue::from("setup_channel"))
        );
        phone.handle.writer.phone_disconnected().await;
        assert_eq!(phone.ended().await, Some(SessionEnd::PhoneDisconnected));
    }
}

#[tokio::test]
async fn a_phone_without_the_card_pairs_with_a_code_it_hears() {
    for att_mtu in MTUS {
        let pond = pond().await;
        let mut phone = Phone::connect(&pond, att_mtu, "install-heard").await;
        let spoken = phone.ask("spoken_code", json!({})).await;
        assert_eq!(spoken["ok"], true);
        assert_eq!(spoken["spoken"], true);
        let heard = wait_for_announcement(&pond.announcer).await;
        assert!(
            !String::from_utf8_lossy(&phone.heard).contains(&heard),
            "the code is never sent, only spoken"
        );

        let again = phone.ask("spoken_code", json!({})).await;
        assert_eq!(again["error"], "spoken_code_limited");
        assert_eq!(again["retry_after"], 30);

        let paired = phone
            .pake("pairing", &heard)
            .await
            .expect("the heard code pairs");
        assert!(pond
            .handshake
            .validate_token(&paired.session_token)
            .await
            .unwrap());
        let status = phone.ask("status", json!({})).await;
        assert_eq!(status["claimed"], true, "any first pairing claims the pond");
    }
}

#[tokio::test]
async fn a_claimed_pond_refuses_its_card_over_the_channel() {
    let pond = pond().await;
    let (first, _) = claim_and_join(&pond, 185).await;
    first.handle.writer.phone_disconnected().await;
    first.ended().await;
    wait_until_free(&pond.channel).await;

    let mut phone = Phone::connect(&pond, 23, "install-late").await;
    let refusal = phone.pake("setup", &pond.code.clone()).await.err().unwrap();
    assert_eq!(
        refusal,
        json!({"id": refusal["id"], "type": "pake_init", "ok": false,
               "error": "setup_code_unavailable"})
    );
    assert!(phone.cipher.is_none());
    let hello = phone.ask("hello", json!({})).await;
    assert_eq!(hello["claimed"], true);
}

#[tokio::test]
async fn a_wrong_card_is_refused_without_keys_or_tokens() {
    let pond = pond().await;
    let mut phone = Phone::connect(&pond, 23, "install-typo").await;
    let refusal = phone.pake("setup", "34970112330").await.err().unwrap();
    assert_eq!(
        refusal,
        json!({"id": refusal["id"], "type": "pake_verify", "ok": false, "accepted": false,
               "error": "invalid_confirmation", "rejection_reason": "invalid_confirmation"})
    );
    assert!(!pond
        .store
        .load()
        .await
        .unwrap()
        .unwrap()
        .claim_state()
        .is_claimed());
    let status = phone.ask("status", json!({})).await;
    assert_eq!(status["error"], "wrong_phase", "still in the clear");
}

#[tokio::test]
async fn a_tampered_ciphertext_ends_the_session() {
    let pond = pond().await;
    let (mut phone, _) = claim_and_join(&pond, 23).await;
    let cipher = phone.cipher.as_mut().unwrap();
    let envelope: Value = serde_json::from_slice(
        &cipher
            .seal_envelope(br#"{"id":90,"type":"status"}"#)
            .unwrap(),
    )
    .unwrap();
    let mut sealed = B64.decode(envelope["sealed"].as_str().unwrap()).unwrap();
    let last = sealed.len() - 1;
    sealed[last] ^= 0x01;
    let tampered = serde_json::to_vec(&json!({"sealed": B64.encode(sealed)})).unwrap();
    assert!(phone.send(&tampered).await);
    assert_eq!(phone.receive().await, None);
    assert_eq!(phone.ended().await, Some(SessionEnd::SealingFailure));
    wait_until_free(&pond.channel).await;
}

#[tokio::test]
async fn a_replayed_message_ends_the_session() {
    let pond = pond().await;
    let (mut phone, _) = claim_and_join(&pond, 185).await;
    let replay = phone
        .cipher
        .as_mut()
        .unwrap()
        .seal_envelope(br#"{"id":91,"type":"status"}"#)
        .unwrap();
    assert!(phone.send(&replay).await);
    let answer = phone.receive().await.expect("the first copy is answered");
    let answer: Value = serde_json::from_slice(
        &phone
            .cipher
            .as_mut()
            .unwrap()
            .open_envelope(&answer)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(answer["ok"], true);
    assert!(phone.send(&replay).await);
    assert_eq!(
        phone.receive().await,
        None,
        "the same counter twice ends the session"
    );
    assert_eq!(phone.ended().await, Some(SessionEnd::SealingFailure));
}

#[tokio::test]
async fn sealed_requests_before_the_exchange_are_refused() {
    let pond = pond().await;
    let mut phone = Phone::connect(&pond, 23, "install-early").await;
    for kind in ["status", "wifi_scan"] {
        let answer = phone.ask(kind, json!({})).await;
        assert_eq!(answer["error"], "wrong_phase", "{kind}");
    }
    let join = phone
        .ask(
            "wifi_join",
            json!({"ssid": "Home", "passphrase": HOME_PASSPHRASE}),
        )
        .await;
    assert_eq!(join["error"], "wrong_phase");
    assert!(
        pond.station.joins().is_empty(),
        "nothing reached the station"
    );
    assert_eq!(
        phone.ask("hello", json!({})).await["ok"],
        true,
        "and the session goes on"
    );

    // An envelope before any keys exist is unreadable, and that ends the session.
    assert!(
        phone
            .send(br#"{"sealed":"AAAAAAAAAAAAAAAAAAAAAA=="}"#)
            .await
    );
    assert_eq!(phone.receive().await, None);
    assert_eq!(phone.ended().await, Some(SessionEnd::UnreadableFrame));
}

#[tokio::test]
async fn a_second_phone_is_refused_while_a_session_is_open() {
    let pond = pond().await;
    let first = Phone::connect(&pond, 23, "install-a").await;
    assert!(pond.channel.open().is_err());
    first.handle.writer.phone_disconnected().await;
    first.ended().await;
    wait_until_free(&pond.channel).await;
    assert!(pond.channel.open().is_ok());
}
