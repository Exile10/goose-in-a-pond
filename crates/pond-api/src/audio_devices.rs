//! `GET /api/v1/audio/devices`: the microphone and speaker in use and why. `POST
//! /api/v1/audio/devices/test`: test every device again and use what works.

use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::http::StatusCode;
use axum::Json;
use pond_core::models::domain::audio_device::AudioDeviceStatus;
use pond_core::models::ports::audio_devices::AudioDeviceControl;
use serde::Deserialize;
use serde_json::{json, Value};

type Refusal = (StatusCode, Json<Value>);

static CONTROL: RwLock<Option<Arc<dyn AudioDeviceControl>>> = RwLock::new(None);

/// Install the device control. Call once at startup, in the process that serves the API.
pub fn install(control: Arc<dyn AudioDeviceControl>) {
    *CONTROL.write().unwrap_or_else(|e| e.into_inner()) = Some(control);
}

fn control() -> Result<Arc<dyn AudioDeviceControl>, Refusal> {
    CONTROL
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "error": "this pond is not managing its audio devices" })),
            )
        })
}

/// Probes open real devices and must not hold a runtime worker while they do.
async fn blocking(
    f: impl FnOnce(Arc<dyn AudioDeviceControl>) -> AudioDeviceStatus + Send + 'static,
) -> Result<Json<AudioDeviceStatus>, Refusal> {
    let control = control()?;
    tokio::task::spawn_blocking(move || f(control))
        .await
        .map(Json)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("the device test failed: {e}") })),
            )
        })
}

/// Choose again without waiting, after a settings save changed what the choice depends on.
pub fn reselect_in_background() {
    if let Ok(control) = control() {
        tokio::task::spawn_blocking(move || control.retest(false));
    }
}

pub async fn status() -> Result<Json<AudioDeviceStatus>, Refusal> {
    blocking(|c| c.status()).await
}

#[derive(Debug, Default, Deserialize)]
struct TestRequest {
    /// Play a short chime on the chosen speaker: the household asked to hear it.
    #[serde(default)]
    chime: bool,
}

/// An empty body is a plain test, so a bare `POST` from a script works.
pub async fn test(body: Bytes) -> Result<Json<AudioDeviceStatus>, Refusal> {
    let request: TestRequest = if body.iter().all(u8::is_ascii_whitespace) {
        TestRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": format!("invalid test request: {e}") })),
            )
        })?
    };
    blocking(move |c| c.retest(request.chime)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::audio_device::{
        AudioChoice, AudioDevice, AudioDirection, DeviceKind,
    };
    use std::sync::Mutex;

    struct Fake {
        chimes: Mutex<Vec<bool>>,
    }

    fn status() -> AudioDeviceStatus {
        AudioDeviceStatus {
            input: AudioChoice::not_tested(
                AudioDirection::Input,
                vec![AudioDevice::new("Built-in", true, DeviceKind::Hardware)],
                None,
            ),
            output: AudioChoice::not_tested(AudioDirection::Output, Vec::new(), None),
            updated_at_ms: 1,
            source: "serve".into(),
            pid: 1,
        }
    }

    impl AudioDeviceControl for Fake {
        fn status(&self) -> AudioDeviceStatus {
            status()
        }
        fn retest(&self, chime: bool) -> AudioDeviceStatus {
            self.chimes.lock().unwrap().push(chime);
            status()
        }
    }

    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn without_a_control_the_routes_say_so_instead_of_inventing_devices() {
        let _g = ONE_AT_A_TIME.lock().await;
        *CONTROL.write().unwrap() = None;
        let (code, _) = super::status().await.unwrap_err();
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_test_is_silent_unless_the_chime_is_asked_for() {
        let _g = ONE_AT_A_TIME.lock().await;
        let fake = Arc::new(Fake {
            chimes: Mutex::new(Vec::new()),
        });
        install(fake.clone());
        let _ = test(Bytes::new()).await.unwrap();
        let _ = test(Bytes::from_static(br#"{"chime":true}"#))
            .await
            .unwrap();
        assert_eq!(*fake.chimes.lock().unwrap(), [false, true]);

        let (code, _) = test(Bytes::from_static(b"{nope")).await.unwrap_err();
        assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);

        let Json(got) = super::status().await.unwrap();
        assert_eq!(got.input.device_id(), Some("Built-in"));
        *CONTROL.write().unwrap() = None;
    }
}
