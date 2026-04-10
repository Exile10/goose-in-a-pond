//! Integration tests for `auto_download_assigned_models`.
//!
//! Run: `cargo test -p pond-server --test auto_download_test`
//!
//! Each test uses a real SQLite database in a temp directory, the shared
//! `MockModelDownloader` (spy), and configurable `MockModelStorage`.

use std::sync::Arc;

use pond_core::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::ports::model_repository::ModelRepository;
use pond_core::services::mock_model_downloader::MockModelDownloader;
use pond_core::services::mock_model_storage::MockModelStorage;
use pond_infra::db::Database;
use pond_infra::sqlite_model_repository::SqliteModelRepository;
use pond_server::startup::auto_download_assigned_models;

// ── Test helpers ──────────────────────────────────────────────────────────────

fn gguf_record(name: &str, downloaded: bool) -> ModelRecord {
    ModelRecord {
        id:               format!("gguf/{name}"),
        category:         ModelCategory::Gguf,
        name:             name.to_string(),
        filename:         Some(format!("{name}.gguf")),
        description:      format!("{name} test model"),
        size_mb:          100,
        url:              Some(format!("https://example.com/{name}.gguf")),
        hf_id:            None,
        ram_estimate_mb:  None,
        recommended_role: Some("chat".to_string()),
        context_length:   None,
        quantization:     None,
        asr_language:     None,
        asr_size:         None,
        tts_engine:       None,
        tts_voice_name:   None,
        config_filename:  None,
        config_url:       None,
        tts_url:          None,
        sample_rate:      None,
        downloaded,
        is_custom:        false,
    }
}

fn whisper_record(downloaded: bool) -> ModelRecord {
    ModelRecord {
        id:               "whisper/base".to_string(),
        category:         ModelCategory::Whisper,
        name:             "base".to_string(),
        filename:         Some("ggml-base.en.bin".to_string()),
        description:      "Whisper base English".to_string(),
        size_mb:          74,
        url:              Some("https://example.com/ggml-base.en.bin".to_string()),
        hf_id:            None,
        ram_estimate_mb:  None,
        recommended_role: None,
        context_length:   None,
        quantization:     None,
        asr_language:     Some("en".to_string()),
        asr_size:         Some("base".to_string()),
        tts_engine:       None,
        tts_voice_name:   None,
        config_filename:  None,
        config_url:       None,
        tts_url:          None,
        sample_rate:      None,
        downloaded,
        is_custom:        false,
    }
}

// ── Test 1: Assigned model not on disk → triggers download, flips DB flag ─────

#[tokio::test]
async fn assigned_model_not_on_disk_triggers_download() {
    let db_tmp   = tempfile::tempdir().unwrap();
    let fs_tmp   = tempfile::tempdir().unwrap(); // real fs for storage paths
    let db       = Database::init(db_tmp.path()).await.unwrap();
    let repo     = Arc::new(SqliteModelRepository::new(db.system.clone()));

    // Insert model with downloaded=false, assign to "chat"
    let model = gguf_record("llama-3b", false);
    repo.upsert(&model).await.unwrap();
    repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();

    // FileSystemBacked: no files pre-created → is_present() returns false
    let storage    = Arc::new(MockModelStorage::FileSystemBacked { base: fs_tmp.path().to_path_buf() });
    let downloader = Arc::new(MockModelDownloader::new(true)); // write placeholder

    let triggered = auto_download_assigned_models(
        repo.clone() as Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
        storage,
        downloader.clone(),
    ).await;

    assert_eq!(triggered, 1, "one download should have been triggered");
    assert!(
        downloader.was_downloaded("https://example.com/llama-3b.gguf").await,
        "downloader should have been called with the model URL"
    );

    // DB flag should be flipped
    let updated = repo.get_by_id("gguf/llama-3b").await.unwrap().unwrap();
    assert!(updated.downloaded, "is_downloaded should be true after successful download");
}

// ── Test 2: Model already on disk → no download ───────────────────────────────

#[tokio::test]
async fn model_already_on_disk_skips_download() {
    let tmp = tempfile::tempdir().unwrap();
    let db   = Database::init(tmp.path()).await.unwrap();
    let repo = Arc::new(SqliteModelRepository::new(db.system.clone()));

    let model = gguf_record("llama-3b", true); // downloaded=true in DB
    repo.upsert(&model).await.unwrap();
    repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();

    let storage    = Arc::new(MockModelStorage::AlwaysPresent); // file exists
    let downloader = Arc::new(MockModelDownloader::new(false));

    let triggered = auto_download_assigned_models(
        repo.clone() as Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
        storage,
        downloader.clone(),
    ).await;

    assert_eq!(triggered, 0, "no download should occur when file is already present");
    assert!(
        !downloader.was_downloaded_any().await,
        "downloader should NOT have been called"
    );
}

// ── Test 3: DB drift (flag=false, file present) → flag fixed, no download ─────

#[tokio::test]
async fn db_drift_fixed_when_file_present_but_flag_false() {
    let tmp = tempfile::tempdir().unwrap();
    let db   = Database::init(tmp.path()).await.unwrap();
    let repo = Arc::new(SqliteModelRepository::new(db.system.clone()));

    let model = gguf_record("llama-3b", false); // DB says not downloaded
    repo.upsert(&model).await.unwrap();
    repo.set_assignment("chat", "gguf/llama-3b").await.unwrap();

    let storage    = Arc::new(MockModelStorage::AlwaysPresent); // but file IS on disk
    let downloader = Arc::new(MockModelDownloader::new(false));

    auto_download_assigned_models(
        repo.clone() as Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
        storage,
        downloader.clone(),
    ).await;

    // DB should now reflect the true state
    let updated = repo.get_by_id("gguf/llama-3b").await.unwrap().unwrap();
    assert!(updated.downloaded, "DB flag should be corrected to true");
    assert!(
        !downloader.was_downloaded_any().await,
        "no download should occur when file already exists on disk"
    );
}

// ── Test 4: Ollama model → never triggers a local download ────────────────────

#[tokio::test]
async fn ollama_model_skips_local_download() {
    let tmp = tempfile::tempdir().unwrap();
    let db   = Database::init(tmp.path()).await.unwrap();
    let repo = Arc::new(SqliteModelRepository::new(db.system.clone()));

    let model = ModelRecord {
        id:               "ollama/llama3.2".to_string(),
        category:         ModelCategory::Ollama,
        name:             "llama3.2".to_string(),
        filename:         None, // no local file for Ollama
        description:      String::new(),
        size_mb:          0,
        url:              None,
        hf_id:            None, ram_estimate_mb: None, recommended_role: None,
        context_length:   None, quantization: None, asr_language: None,
        asr_size:         None, tts_engine: None, tts_voice_name: None,
        config_filename:  None, config_url: None, tts_url: None,
        sample_rate:      None, downloaded: false, is_custom: false,
    };
    repo.upsert(&model).await.unwrap();
    repo.set_assignment("chat", "ollama/llama3.2").await.unwrap();

    let storage    = Arc::new(MockModelStorage::NeverPresent);
    let downloader = Arc::new(MockModelDownloader::new(false));

    let triggered = auto_download_assigned_models(
        repo.clone() as Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
        storage,
        downloader.clone(),
    ).await;

    assert_eq!(triggered, 0, "Ollama models must never trigger a local download");
    assert!(!downloader.was_downloaded_any().await);
}

// ── Test 5: Multiple roles, partial downloads ─────────────────────────────────

#[tokio::test]
async fn multiple_roles_partial_download() {
    let db_tmp = tempfile::tempdir().unwrap();
    let fs_tmp = tempfile::tempdir().unwrap();
    let db     = Database::init(db_tmp.path()).await.unwrap();
    let repo   = Arc::new(SqliteModelRepository::new(db.system.clone()));

    // Chat model: absent from disk (should download)
    let chat_model = gguf_record("chat-model", false);
    repo.upsert(&chat_model).await.unwrap();
    repo.set_assignment("chat", "gguf/chat-model").await.unwrap();

    // ASR model: absent from disk (should download)
    let asr_model = whisper_record(false);
    repo.upsert(&asr_model).await.unwrap();
    repo.set_assignment("asr", "whisper/base").await.unwrap();

    // FileSystemBacked with empty dir → is_present() returns false for both
    let storage    = Arc::new(MockModelStorage::FileSystemBacked { base: fs_tmp.path().to_path_buf() });
    let downloader = Arc::new(MockModelDownloader::new(true)); // write placeholders

    let triggered = auto_download_assigned_models(
        repo.clone() as Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
        storage,
        downloader.clone(),
    ).await;

    assert_eq!(triggered, 2, "both chat and ASR models should be downloaded");
    assert!(downloader.was_downloaded("https://example.com/chat-model.gguf").await);
    assert!(downloader.was_downloaded("https://example.com/ggml-base.en.bin").await);

    // Both DB flags should be flipped
    let chat = repo.get_by_id("gguf/chat-model").await.unwrap().unwrap();
    let asr  = repo.get_by_id("whisper/base").await.unwrap().unwrap();
    assert!(chat.downloaded);
    assert!(asr.downloaded);
}

// ── Test 6: Model with no URL → skipped gracefully ───────────────────────────

#[tokio::test]
async fn model_with_no_url_is_skipped_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let db   = Database::init(tmp.path()).await.unwrap();
    let repo = Arc::new(SqliteModelRepository::new(db.system.clone()));

    let mut model = gguf_record("no-url-model", false);
    model.url = None; // no download URL
    repo.upsert(&model).await.unwrap();
    repo.set_assignment("chat", "gguf/no-url-model").await.unwrap();

    let storage    = Arc::new(MockModelStorage::NeverPresent);
    let downloader = Arc::new(MockModelDownloader::new(false));

    let triggered = auto_download_assigned_models(
        repo.clone() as Arc<dyn pond_core::ports::model_repository::ModelRepository + Send + Sync>,
        storage,
        downloader.clone(),
    ).await;

    assert_eq!(triggered, 0, "model with no URL should be skipped, not panic");
    assert!(!downloader.was_downloaded_any().await);
}
