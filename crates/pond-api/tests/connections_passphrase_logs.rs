//! No Wi-Fi passphrase reaches a log line, however a join over HTTPS goes. Its own binary,
//! because the capturing subscriber must be the process's only one.
#[path = "support/connections_pond.rs"]
mod connections_pond;

use std::io::Write;
use std::sync::{Arc, Mutex};

use connections_pond::*;
use pond_core::network::domain::wifi::WifiJoinError;
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

/// The current-thread runtime keeps every task of the request on this thread, so the
/// thread-local subscriber sees all of them.
#[tokio::test(flavor = "current_thread")]
async fn no_passphrase_reaches_a_log_line_however_the_join_goes() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let pond = pond().await;
    let phones = household(&pond).await;
    let too_long = format!("{PASSPHRASE}{}", "z".repeat(48));
    for (token, passphrase) in [
        (&phones.claimant, PASSPHRASE),
        (&phones.claimant, "otter-lantern-wrong-17"),
        (&phones.claimant, too_long.as_str()),
        (&phones.guest, PASSPHRASE),
    ] {
        let body = json!({"ssid": "Home", "passphrase": passphrase}).to_string();
        join(&pond.lan, token, &body).await;
    }
    pond.station.fail_joins(Some(WifiJoinError::Timeout));
    let body = json!({"ssid": "Home", "passphrase": PASSPHRASE}).to_string();
    join(&pond.lan, &phones.claimant, &body).await;

    let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("joining Wi-Fi for the household's connections settings"),
        "vacuity: the joins were logged at all"
    );
    for secret in ["otter-lantern", "quietly-91", "wrong-17"] {
        assert!(!logged.contains(secret), "{secret} reached a log line");
    }
}
