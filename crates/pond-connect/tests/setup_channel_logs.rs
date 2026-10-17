//! No secret of a setup channel session reaches a log line or the audit trail. Its own binary,
//! because the capturing subscriber must be the process's only one.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::json;

/// Formats every event at every level into one buffer, fields included.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn no_secret_reaches_a_log_line() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let pond = pond().await;
    let (mut phone, paired) = claim_and_join(&pond, 23).await;
    let wrong = phone
        .ask(
            "wifi_join",
            json!({"ssid": "Home", "passphrase": "not-the-passphrase-77"}),
        )
        .await;
    assert_eq!(wrong["error"], "wrong_password");
    phone.handle.writer.phone_disconnected().await;
    phone.ended().await;
    wait_until_free(&pond.channel).await;

    let mut second = Phone::connect(&pond, 185, "install-heard").await;
    second.ask("spoken_code", json!({})).await;
    let spoken = wait_for_announcement(&pond.announcer).await;
    let second_paired = second.pake("pairing", &spoken).await.unwrap();
    second.handle.writer.phone_disconnected().await;
    second.ended().await;

    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("setup_channel_request") && log.contains("wifi_join"),
        "the capture saw the session's lines: {}",
        log.len()
    );
    let events = format!("{:?}", pond.events.all());
    for (name, secret) in [
        ("the passphrase", HOME_PASSPHRASE.to_string()),
        ("a wrong passphrase", "not-the-passphrase-77".to_string()),
        ("the setup code", pond.code.clone()),
        ("the spoken code", spoken.clone()),
        ("a session token", paired.session_token.clone()),
        ("a refresh token", paired.refresh_token.clone()),
        (
            "a second session token",
            second_paired.session_token.clone(),
        ),
        ("k_p2c", paired.keys_hex[0].clone()),
        ("k_c2p", paired.keys_hex[1].clone()),
        ("a second k_p2c", second_paired.keys_hex[0].clone()),
    ] {
        assert!(!log.contains(&secret), "{name} reached a log line");
        assert!(!events.contains(&secret), "{name} reached the audit trail");
    }
}
