//! Auto-starts a downloaded llamafile as the local OpenAI-compatible LLM server.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Child;

// ── Guard ─────────────────────────────────────────────────────────────────────

/// Holds the spawned llamafile child process.  Kills it on drop.
pub struct LlamafileProcess(Child);

impl Drop for LlamafileProcess {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// ── Health check ──────────────────────────────────────────────────────────────

/// Returns `true` if something is already serving on `port`.
pub async fn is_running(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}", port);
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok()
}

// ── Binary lookup ─────────────────────────────────────────────────────────────

/// The file of the chosen llamafile model: its catalogue row's, else its name as a file.
async fn chosen_file(
    data_dir: &Path,
    model_service: &pond_core::models::services::model_service::ModelService,
    name: &str,
) -> Option<PathBuf> {
    use pond_core::models::domain::model_layout::path_for;
    use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
    let recorded = model_service
        .get(&ModelRecord::id_for(&ModelCategory::Llamafile, name))
        .await
        .ok()
        .flatten()
        .and_then(|r| r.filename);
    [
        recorded,
        Some(name.to_string()),
        Some(format!("{name}.llamafile")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|f| path_for(data_dir, &ModelCategory::Llamafile, &f))
    .find(|p| p.is_file())
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Whether the binary takes `--jinja`; builds ≤ v0.9.0 crash if it is passed.
fn supports_jinja(binary: &Path) -> bool {
    std::process::Command::new(binary)
        .arg("--help")
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout).to_string()
                + String::from_utf8_lossy(&o.stderr).as_ref();
            out.contains("--jinja")
        })
        .unwrap_or(false)
}

/// Spawn the server on loopback only (privacy) and wait up to 60 s for it to load.
async fn spawn(binary: &Path, port: u16) -> Result<LlamafileProcess> {
    let jinja = supports_jinja(binary);
    if jinja {
        println!("  ✅ llamafile supports --jinja — Jinja2 chat templates enabled");
    } else {
        println!("  ⚠  llamafile does not support --jinja (old build) — skipping flag; upgrade for better chat template support");
    }

    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--server")
        .args(["--port", &port.to_string()])
        .args(["--host", "127.0.0.1"])
        .arg("--nobrowser")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if jinja {
        cmd.arg("--jinja");
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn {}", binary.display()))?;

    let proc = LlamafileProcess(child);

    for attempt in 1..=60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if is_running(port).await {
            return Ok(proc);
        }
        if attempt == 15 {
            println!("  ⏳ Still loading LLM model — this can take up to 30 s on first launch...");
        }
    }

    // Still loading after 60 s: return the guard; the FallbackProvider covers early requests.
    println!("  ⚠  LLM did not respond within 60 s — it may still be loading in the background.");
    Ok(proc)
}

// ── High-level entry point ────────────────────────────────────────────────────

/// Base URL for llamafile given a port.
pub fn url_for(port: u16) -> String {
    format!("http://127.0.0.1:{}", port)
}

/// Spawn the chosen llamafile on the first free port; never downloads. Never errors: `None` if
/// the base port already serves, none is chosen or on disk, or on any failure, which is printed.
pub async fn try_start(
    data_dir: &Path,
    model_service: std::sync::Arc<pond_core::models::services::model_service::ModelService>,
    model_name: Option<&str>,
) -> Option<(LlamafileProcess, u16)> {
    let base_port = crate::ports::llamafile_port();

    if is_running(base_port).await {
        println!("  🧠 LLM already running at {}", url_for(base_port));
        return None;
    }

    let port = match crate::ports::find_free_port(base_port).await {
        Some(p) => p,
        None => {
            println!("  ⚠  No free port found near {} for llamafile", base_port);
            return None;
        }
    };

    // A chosen llamafile runs. None chosen, none runs, and nothing is fetched on its behalf: an
    // assigned model whose file is missing comes back through the boot restore.
    let chosen: Option<String> = match model_name {
        Some(name) if !name.trim().is_empty() => Some(name.to_string()),
        _ => match model_service.model_for_role("chat").await {
            Ok(Some(record))
                if record.category
                    == pond_core::models::domain::model_record::ModelCategory::Llamafile =>
            {
                Some(record.name)
            }
            _ => None,
        },
    };
    let Some(name) = chosen else {
        println!("  ⏭  LLM: no llamafile model is chosen");
        return None;
    };
    let model = match chosen_file(data_dir, &model_service, &name).await {
        Some(path) => path,
        None => {
            println!("  ⚠  LLM: llamafile model '{name}' is not on disk yet");
            return None;
        }
    };

    println!(
        "  🧠 Starting llamafile  ({})...",
        model.file_name().unwrap_or_default().to_string_lossy()
    );

    match spawn(&model, port).await {
        Ok(proc) => {
            println!("  ✅ LLM ready on port {}", port);
            Some((proc, port))
        }
        Err(e) => {
            println!("  ⚠  Failed to start llamafile: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
    use pond_core::models::mocks::mock_model_catalog_provider::MockModelCatalogProvider;
    use pond_core::models::mocks::mock_model_downloader::MockModelDownloader;
    use pond_core::models::mocks::mock_model_repository::MockModelRepository;
    use pond_core::models::mocks::mock_model_storage::MockModelStorage;
    use pond_core::models::ports::model_repository::ModelRepository;
    use pond_core::models::services::model_service::ModelService;
    use std::sync::Arc;

    fn llamafile_row(name: &str) -> ModelRecord {
        ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::Llamafile, name),
            category: ModelCategory::Llamafile,
            name: name.to_string(),
            filename: Some(format!("{name}.llamafile")),
            description: String::new(),
            size_mb: 1950,
            url: Some(format!("https://example.com/{name}.llamafile")),
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: Some("chat".into()),
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        }
    }

    /// No model chosen, or one not on disk: nothing starts and nothing is downloaded, whatever
    /// the catalogue offers.
    #[tokio::test]
    async fn nothing_is_picked_or_fetched_for_the_household() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        repo.upsert(&llamafile_row("gemma-2b")).await.unwrap();
        let downloader = Arc::new(MockModelDownloader::new(true));
        let service = Arc::new(ModelService::new(
            repo,
            Arc::new(MockModelCatalogProvider::default()),
            downloader.clone(),
            Arc::new(MockModelStorage::file_system_backed(
                tmp.path().to_path_buf(),
            )),
        ));

        for chosen in [None, Some(""), Some("gemma-2b")] {
            assert!(try_start(tmp.path(), service.clone(), chosen)
                .await
                .is_none());
        }
        assert_eq!(downloader.download_count().await, 0, "nothing downloaded");
    }

    #[tokio::test]
    async fn the_chosen_file_is_found_by_its_row_or_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        let llm = tmp.path().join("models/llm");
        std::fs::create_dir_all(&llm).unwrap();
        std::fs::write(llm.join("gemma-2-2b-it.Q4_K_M.llamafile"), b"exe").unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let mut row = llamafile_row("gemma-2b");
        row.filename = Some("gemma-2-2b-it.Q4_K_M.llamafile".into());
        repo.upsert(&row).await.unwrap();
        let service = ModelService::new(
            repo,
            Arc::new(MockModelCatalogProvider::default()),
            Arc::new(MockModelDownloader::new(false)),
            Arc::new(MockModelStorage::file_system_backed(
                tmp.path().to_path_buf(),
            )),
        );
        assert_eq!(
            chosen_file(tmp.path(), &service, "gemma-2b").await,
            Some(llm.join("gemma-2-2b-it.Q4_K_M.llamafile"))
        );
        assert_eq!(chosen_file(tmp.path(), &service, "missing").await, None);
    }
}
