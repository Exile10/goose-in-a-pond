//! What the pond does with the local engines on its way out.

use std::time::Duration;

use goose::providers::local_inference::{evict_model, loaded_model_ids};

/// Free every model the local engines hold, before the process exits.
///
/// goose keeps its model runtime in a process-wide static that is never dropped, so a loaded model
/// is still alive at exit. LiteRT-LM's own exit-time destructors then destroy its GPU accelerator
/// while the engine's threads still use it, so the pond would die with SIGSEGV on every stop.
/// Evicting first deletes each engine in order, the same path a model switch takes.
/// Bounded by `within`: a model still busy then is left to the exit as before, rather than holding
/// the stop up.
pub async fn release_local_models(within: Duration) {
    let release = async {
        let ids = match loaded_model_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "could not list the loaded local models to release them");
                return;
            }
        };
        for id in ids {
            match evict_model(&id).await {
                Ok(true) => tracing::info!(model = %id, "released the local model before exit"),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(model = %id, error = %e, "could not release a local model")
                }
            }
        }
    };
    if tokio::time::timeout(within, release).await.is_err() {
        tracing::warn!(
            within_secs = within.as_secs(),
            "a local model was still busy; exiting without releasing it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing loaded (the runtime may not even exist): returns at once, touching nothing.
    #[tokio::test]
    async fn with_nothing_loaded_it_returns_at_once() {
        let started = std::time::Instant::now();
        release_local_models(Duration::from_secs(5)).await;
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
