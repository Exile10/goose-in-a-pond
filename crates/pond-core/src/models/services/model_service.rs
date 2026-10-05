//! Model lifecycle: catalog, downloads, role assignment. HTTP, SQLite and file paths live in
//! the port implementations wired in `pond-server`.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::models::domain::model_record::{
    BinaryRecord, ModelCategory, ModelRecord, ModelRoleAssignment,
};
use crate::models::ports::model_catalog_provider::ModelCatalogProvider;
use crate::models::ports::model_downloader::ModelDownloader;
use crate::models::ports::model_repository::ModelRepository;
use crate::models::ports::model_storage::ModelStorage;

/// Why a seed takes a row out of the catalogue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pruned {
    /// A drafter, encoder or `draft`-role row: never something to choose.
    Companion,
    /// Added by hand or found on disk, then lost its file, with no source to fetch it again.
    Ghost,
    /// Once bundled, bundled no more, and never downloaded.
    Retired,
    /// An Ollama model a running Ollama server no longer lists.
    Unlisted,
}

/// What a seed does to the rows already in the catalogue.
#[derive(Debug, Default, PartialEq)]
pub struct PrunePlan {
    pub delete: Vec<(String, Pruned)>,
    /// Assigned Ollama rows a running server no longer lists: kept for the assignment,
    /// unavailable.
    pub unavailable: Vec<String>,
}

/// The rows `catalogue` (this seed's fetch) leaves stale. An assigned row is never deleted, and
/// nothing on disk is touched: a downloaded row stays unless it is a companion, whose file the
/// scan's companion filter keeps from coming back as a row. A fetch with no Ollama rows says
/// nothing about Ollama (the composite skips a server that does not answer), so it changes none.
pub fn prune_plan(
    records: &[ModelRecord],
    catalogue: &[ModelRecord],
    assignments: &[ModelRoleAssignment],
) -> PrunePlan {
    use crate::models::domain::taxonomy::is_companion_file;
    let assigned: std::collections::HashSet<&str> =
        assignments.iter().map(|a| a.model_id.as_str()).collect();
    let listed: std::collections::HashSet<&str> = catalogue.iter().map(|m| m.id.as_str()).collect();
    let ollama_answered = catalogue
        .iter()
        .any(|m| m.category == ModelCategory::Ollama);
    let mut plan = PrunePlan::default();
    for r in records {
        let in_listing = listed.contains(r.id.as_str());
        let unlisted_ollama = r.category == ModelCategory::Ollama && ollama_answered && !in_listing;
        if assigned.contains(r.id.as_str()) {
            if unlisted_ollama && r.downloaded {
                plan.unavailable.push(r.id.clone());
            }
            continue;
        }
        let companion = is_companion_file(&r.name)
            || r.filename.as_deref().is_some_and(is_companion_file)
            || r.recommended_role.as_deref() == Some("draft");
        let why = if companion {
            Some(Pruned::Companion)
        } else if r.category == ModelCategory::Ollama {
            unlisted_ollama.then_some(Pruned::Unlisted)
        } else if r.is_custom {
            (!r.downloaded && r.url.is_none()).then_some(Pruned::Ghost)
        } else {
            (!in_listing && !r.downloaded).then_some(Pruned::Retired)
        };
        if let Some(why) = why {
            plan.delete.push((r.id.clone(), why));
        }
    }
    plan
}

/// What [`apply_catalog`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CatalogApplied {
    pub upserted: usize,
    pub pruned: usize,
}

/// Upsert a fetched catalogue and prune what it leaves stale. `present` says whether a row's
/// file is on disk, `None` for a row with no file of its own, whose fetched flag is kept.
pub async fn apply_catalog(
    repo: &dyn ModelRepository,
    models: Vec<ModelRecord>,
    present: &(dyn Fn(&ModelRecord) -> Option<bool> + Send + Sync),
) -> Result<CatalogApplied> {
    let mut applied = CatalogApplied::default();
    for mut record in models.iter().cloned() {
        if let Some(on_disk) = present(&record) {
            record.downloaded = on_disk;
        }
        repo.upsert(&record).await?;
        applied.upserted += 1;
    }
    let records = repo.list_all().await?;
    let assignments = repo.list_assignments().await?;
    let plan = prune_plan(&records, &models, &assignments);
    for (id, why) in &plan.delete {
        if repo.delete(id).await? {
            tracing::info!(model = %id, reason = ?why, "pruned a stale catalogue row");
            applied.pruned += 1;
        }
    }
    for id in &plan.unavailable {
        repo.set_downloaded(id, false).await?;
    }
    Ok(applied)
}

/// What a boot restore does: correct the flag of an assigned file that is on disk, and fetch an
/// assigned model whose file is missing. Only the household's own assignments; nothing else.
#[derive(Debug, Default)]
pub struct RestorePlan {
    pub mark_downloaded: Vec<String>,
    pub fetch: Vec<ModelRecord>,
}

/// The restore for `assignments`. `present` says whether a row's file is on disk, `None` for a
/// row with no file of its own (Ollama, HTTP voices, self-fetching embeddings).
pub fn restore_plan(
    assignments: &[ModelRoleAssignment],
    records: &[ModelRecord],
    present: impl Fn(&ModelRecord) -> Option<bool>,
) -> RestorePlan {
    let mut plan = RestorePlan::default();
    let mut seen = std::collections::HashSet::new();
    for a in assignments {
        if !seen.insert(a.model_id.as_str()) {
            continue;
        }
        let Some(record) = records.iter().find(|r| r.id == a.model_id) else {
            continue;
        };
        match present(record) {
            None => {}
            Some(true) if !record.downloaded => plan.mark_downloaded.push(record.id.clone()),
            Some(true) => {}
            Some(false) => {
                let fetchable = record.url.is_some()
                    || crate::models::domain::curated::for_record(record).is_some();
                if fetchable {
                    plan.fetch.push(record.clone());
                }
            }
        }
    }
    plan
}

pub struct ModelService {
    repo: Arc<dyn ModelRepository>,
    catalog: Arc<dyn ModelCatalogProvider>,
    downloader: Arc<dyn ModelDownloader>,
    storage: Arc<dyn ModelStorage>,
}

impl ModelService {
    pub fn new(
        repo: Arc<dyn ModelRepository>,
        catalog: Arc<dyn ModelCatalogProvider>,
        downloader: Arc<dyn ModelDownloader>,
        storage: Arc<dyn ModelStorage>,
    ) -> Self {
        Self {
            repo,
            catalog,
            downloader,
            storage,
        }
    }

    // ── Catalog ───────────────────────────────────────────────────────────────

    /// Upsert the fetched catalog and prune what it leaves stale; `upsert` keeps `is_custom` set.
    pub async fn seed_catalog(&self) -> Result<usize> {
        let (models, _binaries) = self.catalog.fetch().await?;
        let storage = self.storage.clone();
        let present =
            move |r: &ModelRecord| storage.path_for(r).is_some().then(|| storage.is_present(r));
        Ok(apply_catalog(&*self.repo, models, &present).await?.upserted)
    }

    /// Re-run `seed_catalog`; safe to repeat.
    pub async fn refresh_catalog(&self) -> Result<usize> {
        self.seed_catalog().await
    }

    /// Fetch tool binaries without touching the model catalog.
    pub async fn fetch_binaries(&self) -> Result<Vec<BinaryRecord>> {
        let (_models, binaries) = self.catalog.fetch().await?;
        Ok(binaries)
    }

    // ── Disk sync ─────────────────────────────────────────────────────────────

    /// Refresh `downloaded` flags from disk on every later startup; returns how many changed.
    /// A row with no file of its own (Ollama, HTTP voices) keeps the flag its source gave it.
    pub async fn sync_disk_flags(&self) -> Result<usize> {
        let records = self.repo.list_all().await?;
        let mut changed = 0usize;
        for record in &records {
            if self.storage.path_for(record).is_none() {
                continue;
            }
            let on_disk = self.storage.is_present(record);
            if on_disk != record.downloaded {
                self.repo.set_downloaded(&record.id, on_disk).await?;
                changed += 1;
            }
        }
        Ok(changed)
    }

    // ── Role assignments ──────────────────────────────────────────────────────

    pub async fn model_for_role(&self, role: &str) -> Result<Option<ModelRecord>> {
        let assignment = self.repo.get_assignment(role).await?;
        match assignment {
            None => Ok(None),
            Some(a) => self.repo.get_by_id(&a.model_id).await,
        }
    }

    pub async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
        self.repo.list_assignments().await
    }

    /// Assign `model_id` to `role` if its category suits the role.
    pub async fn assign_role(&self, role: &str, model_id: &str) -> Result<()> {
        let record = self
            .repo
            .get_by_id(model_id)
            .await?
            .ok_or_else(|| anyhow!("Model '{}' not found in catalog", model_id))?;

        if !ModelRoleAssignment::category_matches_role(&record.category, role) {
            return Err(anyhow!(
                "Model category '{}' is not valid for role '{}'",
                record.category.as_str(),
                role
            ));
        }

        self.repo.set_assignment(role, model_id).await
    }

    /// Remove the assignment for `role` (no-op if unset).
    pub async fn clear_role(&self, role: &str) -> Result<()> {
        self.repo.clear_assignment(role).await
    }

    // ── Download ──────────────────────────────────────────────────────────────

    /// Path of `model_id`'s file, downloading it from the record's `url` if missing.
    pub async fn ensure_downloaded(&self, model_id: &str) -> Result<PathBuf> {
        let record = self
            .repo
            .get_by_id(model_id)
            .await?
            .ok_or_else(|| anyhow!("Model '{}' not found in catalog", model_id))?;

        let path = self.storage.path_for(&record).ok_or_else(|| {
            anyhow!(
                "Model '{}' has no local file path (it is server-side only)",
                model_id
            )
        })?;

        if path.exists() {
            return Ok(path);
        }

        let url = record
            .url
            .as_deref()
            .ok_or_else(|| anyhow!("Model '{}' has no download URL in the catalog", model_id))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.downloader.download(url, &path, record.size_mb).await?;
        self.repo.set_downloaded(model_id, true).await?;

        Ok(path)
    }

    /// Ensure a tool binary is on disk. Returns its path.
    pub async fn ensure_binary_downloaded(&self, record: &BinaryRecord) -> Result<PathBuf> {
        let path = self.storage.binary_path(record);
        if path.exists() {
            return Ok(path);
        }

        let url = record.url_for_current_platform().ok_or_else(|| {
            anyhow!(
                "Binary '{}' has no download URL for platform '{}'",
                record.name,
                BinaryRecord::current_platform_key()
            )
        })?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        self.downloader.download(url, &path, 0).await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            std::fs::set_permissions(&path, perms)?;
        }

        Ok(path)
    }

    // ── Listing ───────────────────────────────────────────────────────────────

    pub async fn list_all(&self) -> Result<Vec<ModelRecord>> {
        self.repo.list_all().await
    }

    pub async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>> {
        self.repo.list_by_category(category).await
    }

    /// Look up a model by its stable id (`"{category}/{name}"`).
    pub async fn get(&self, model_id: &str) -> Result<Option<ModelRecord>> {
        self.repo.get_by_id(model_id).await
    }

    /// Register a user-added model (sets `is_custom = true`).
    pub async fn add_custom(&self, mut record: ModelRecord) -> Result<()> {
        record.is_custom = true;
        record.downloaded = self.storage.is_present(&record);
        self.repo.upsert(&record).await
    }

    /// Mark a model as downloaded (or not). Used by download progress handlers.
    pub async fn set_downloaded(&self, model_id: &str, downloaded: bool) -> Result<()> {
        self.repo.set_downloaded(model_id, downloaded).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // ── Mock ModelRepository ──────────────────────────────────────────────

    struct MockModelRepository {
        records: Mutex<HashMap<String, ModelRecord>>,
    }

    impl MockModelRepository {
        fn new() -> Self {
            Self {
                records: Mutex::new(HashMap::new()),
            }
        }
    }

    #[async_trait]
    impl ModelRepository for MockModelRepository {
        async fn list_all(&self) -> Result<Vec<ModelRecord>> {
            let mut v: Vec<_> = self.records.lock().unwrap().values().cloned().collect();
            v.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(v)
        }
        async fn list_by_category(&self, cat: &ModelCategory) -> Result<Vec<ModelRecord>> {
            Ok(self
                .records
                .lock()
                .unwrap()
                .values()
                .filter(|r| &r.category == cat)
                .cloned()
                .collect())
        }
        async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>> {
            Ok(self.records.lock().unwrap().get(id).cloned())
        }
        async fn upsert(&self, model: &ModelRecord) -> Result<()> {
            self.records
                .lock()
                .unwrap()
                .insert(model.id.clone(), model.clone());
            Ok(())
        }
        async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()> {
            if let Some(r) = self.records.lock().unwrap().get_mut(id) {
                r.downloaded = downloaded;
            }
            Ok(())
        }
        async fn delete(&self, id: &str) -> Result<bool> {
            Ok(self.records.lock().unwrap().remove(id).is_some())
        }
        async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
            Ok(vec![])
        }
        async fn get_assignment(&self, _: &str) -> Result<Option<ModelRoleAssignment>> {
            Ok(None)
        }
        async fn set_assignment(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn clear_assignment(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    // ── Mock ModelCatalogProvider ─────────────────────────────────────────

    struct MockCatalog;
    #[async_trait]
    impl ModelCatalogProvider for MockCatalog {
        async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
            Ok((vec![], vec![]))
        }
    }

    // ── Mock ModelDownloader ──────────────────────────────────────────────

    struct MockDownloader {
        called_urls: Mutex<Vec<String>>,
    }
    impl MockDownloader {
        fn new() -> Self {
            Self {
                called_urls: Mutex::new(vec![]),
            }
        }
    }
    #[async_trait]
    impl ModelDownloader for MockDownloader {
        async fn download(&self, url: &str, _dest: &std::path::Path, _size: u64) -> Result<()> {
            self.called_urls.lock().unwrap().push(url.to_string());
            Ok(())
        }
    }

    // ── Mock ModelStorage (uses a real tempdir) ──────────────────────────

    struct MockStorage {
        base: PathBuf,
    }
    impl MockStorage {
        fn new(base: PathBuf) -> Self {
            Self { base }
        }
    }
    impl ModelStorage for MockStorage {
        fn path_for(&self, record: &ModelRecord) -> Option<PathBuf> {
            record.filename.as_ref().map(|f| self.base.join(f))
        }
        fn binary_path(&self, _record: &BinaryRecord) -> PathBuf {
            self.base.join("bin")
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn stub_model(id: &str, filename: &str, downloaded: bool, url: Option<&str>) -> ModelRecord {
        ModelRecord {
            id: id.to_string(),
            category: ModelCategory::Llamafile,
            name: id.to_string(),
            filename: Some(filename.to_string()),
            description: String::new(),
            size_mb: 100,
            url: url.map(|s| s.to_string()),
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
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
            downloaded,
            is_custom: false,
        }
    }

    fn make_service(
        repo: Arc<MockModelRepository>,
        dl: Arc<MockDownloader>,
        base_dir: &std::path::Path,
    ) -> ModelService {
        ModelService::new(
            repo,
            Arc::new(MockCatalog),
            dl,
            Arc::new(MockStorage::new(base_dir.to_path_buf())),
        )
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ensure_downloaded_triggers_download_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl.clone(), tmp.path());

        // Seed a model with a URL, not yet on disk
        let m = stub_model(
            "llamafile/qwen",
            "qwen.llamafile",
            false,
            Some("https://example.com/qwen.llamafile"),
        );
        repo.upsert(&m).await.unwrap();

        let path = svc.ensure_downloaded("llamafile/qwen").await.unwrap();
        assert_eq!(path, tmp.path().join("qwen.llamafile"));

        let urls = dl.called_urls.lock().unwrap();
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://example.com/qwen.llamafile");

        let record = repo.get_by_id("llamafile/qwen").await.unwrap().unwrap();
        assert!(
            record.downloaded,
            "downloaded flag should be true after download"
        );
    }

    #[tokio::test]
    async fn ensure_downloaded_skips_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl.clone(), tmp.path());

        let model_path = tmp.path().join("existing.llamafile");
        std::fs::File::create(&model_path).unwrap();

        let m = stub_model(
            "llamafile/existing",
            "existing.llamafile",
            true,
            Some("https://example.com/existing.llamafile"),
        );
        repo.upsert(&m).await.unwrap();

        let path = svc.ensure_downloaded("llamafile/existing").await.unwrap();
        assert_eq!(path, model_path);

        let urls = dl.called_urls.lock().unwrap();
        assert!(
            urls.is_empty(),
            "downloader should not have been called, but got: {:?}",
            *urls
        );
    }

    #[tokio::test]
    async fn ensure_downloaded_errors_when_model_not_in_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo, dl, tmp.path());

        let result = svc.ensure_downloaded("gguf/nonexistent").await;
        assert!(result.is_err(), "should error when model id is unknown");
    }

    #[tokio::test]
    async fn ensure_downloaded_errors_when_no_url() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl, tmp.path());

        // Model with no URL and file not present
        let m = stub_model("llamafile/no-url", "no-url.llamafile", false, None);
        repo.upsert(&m).await.unwrap();

        let result = svc.ensure_downloaded("llamafile/no-url").await;
        assert!(
            result.is_err(),
            "should error when model has no download URL"
        );
    }

    /// The shared mocks used by integration tests work with `ModelService`.
    mod shared_mocks {
        use crate::models::domain::model_record::{ModelCategory, ModelRecord};
        use crate::models::mocks::mock_model_catalog_provider::MockModelCatalogProvider;
        use crate::models::mocks::mock_model_downloader::MockModelDownloader;
        use crate::models::mocks::mock_model_repository::MockModelRepository as SharedMockRepo;
        use crate::models::mocks::mock_model_storage::MockModelStorage;
        use crate::models::ports::{
            model_repository::ModelRepository, model_storage::ModelStorage,
        };
        use crate::models::services::model_service::ModelService;
        use std::sync::Arc;

        fn gguf(name: &str, downloaded: bool, url: Option<&str>) -> ModelRecord {
            ModelRecord {
                id: format!("gguf/{name}"),
                category: ModelCategory::Gguf,
                name: name.to_string(),
                filename: Some(format!("{name}.gguf")),
                description: String::new(),
                size_mb: 10,
                url: url.map(str::to_string),
                hf_id: None,
                ram_estimate_mb: None,
                recommended_role: None,
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
                downloaded,
                is_custom: false,
            }
        }

        #[tokio::test]
        async fn shared_never_present_forces_download() {
            let tmp = tempfile::tempdir().unwrap();
            let repo = Arc::new(SharedMockRepo::new());
            let dl = Arc::new(MockModelDownloader::new(true)); // write placeholder
            let storage: Arc<dyn ModelStorage> = Arc::new(MockModelStorage::file_system_backed(
                tmp.path().to_path_buf(),
            ));
            let svc = ModelService::new(
                repo.clone() as Arc<dyn ModelRepository>,
                Arc::new(MockModelCatalogProvider::default()),
                dl.clone(),
                storage,
            );

            let m = gguf("llama3", false, Some("https://example.com/llama3.gguf"));
            repo.upsert(&m).await.unwrap();

            svc.ensure_downloaded("gguf/llama3").await.unwrap();
            assert!(dl.was_downloaded_any().await);
            assert!(
                repo.get_by_id("gguf/llama3")
                    .await
                    .unwrap()
                    .unwrap()
                    .downloaded
            );
        }

        #[tokio::test]
        async fn shared_file_present_skips_download() {
            let tmp = tempfile::tempdir().unwrap();
            let repo = Arc::new(SharedMockRepo::new());
            let dl = Arc::new(MockModelDownloader::new(false));
            let storage: Arc<dyn ModelStorage> = Arc::new(MockModelStorage::file_system_backed(
                tmp.path().to_path_buf(),
            ));
            let svc = ModelService::new(
                repo.clone() as Arc<dyn ModelRepository>,
                Arc::new(MockModelCatalogProvider::default()),
                dl.clone(),
                storage,
            );

            // Pre-create the file on disk so path.exists() returns true
            let model_dir = tmp.path().join("models/gguf");
            std::fs::create_dir_all(&model_dir).unwrap();
            std::fs::write(model_dir.join("llama3.gguf"), b"fake weights").unwrap();

            let m = gguf("llama3", true, Some("https://example.com/llama3.gguf"));
            repo.upsert(&m).await.unwrap();

            svc.ensure_downloaded("gguf/llama3").await.unwrap();
            assert!(
                !dl.was_downloaded_any().await,
                "should not download when file already present"
            );
        }
    }

    // ── Prune at seed ─────────────────────────────────────────────────────

    fn row(id: &str, category: ModelCategory, custom: bool, downloaded: bool) -> ModelRecord {
        let name = id.split_once('/').map_or(id, |(_, n)| n).to_string();
        let mut r = stub_model(id, &format!("{name}.gguf"), downloaded, None);
        r.category = category;
        r.name = name;
        r.is_custom = custom;
        r
    }

    #[test]
    fn a_seed_prunes_what_nobody_can_use_and_keeps_what_is_downloaded_or_assigned() {
        use ModelCategory::{Gguf, Ollama, Whisper};
        let mut drafter = row("gguf/mtp-gemma-4-E2B-it", Gguf, true, true);
        drafter.recommended_role = Some("draft".into());
        let assistant = row("gguf/gemma-4-E2B-it-assistant-F16", Gguf, true, false);
        let ghost = row("gguf/lost-by-hand", Gguf, true, false);
        let mut added = row("gguf/added-by-url", Gguf, true, false);
        added.url = Some("https://example.com/a.gguf".into());
        let on_disk = row("gguf/copied-in", Gguf, true, true);
        let retired = row("gguf/llama-3.2-3b", Gguf, false, false);
        let kept_file = row("gguf/qwen2.5-3b", Gguf, false, true);
        let assigned = row("gguf/gemma-2b", Gguf, false, false);
        let assigned_companion = row("gguf/mmproj-BF16", Gguf, true, true);
        let gone_ollama = row("ollama/llama3.2", Ollama, true, true);
        let listed_ollama = row("ollama/qwen3:4b", Ollama, false, true);
        let assigned_ollama = row("ollama/mistral", Ollama, false, true);
        let bundled = row("whisper/base", Whisper, false, false);
        let records = vec![
            drafter,
            assistant,
            ghost,
            added,
            on_disk,
            retired,
            kept_file,
            assigned,
            assigned_companion,
            gone_ollama,
            listed_ollama.clone(),
            assigned_ollama,
            bundled.clone(),
        ];
        let catalogue = vec![bundled, listed_ollama];
        let assignments = vec![
            assigned_one("chat", "gguf/gemma-2b"),
            assigned_one("tool", "gguf/mmproj-BF16"),
            assigned_one("think", "ollama/mistral"),
        ];

        let plan = prune_plan(&records, &catalogue, &assignments);
        let mut deleted: Vec<(&str, Pruned)> = plan
            .delete
            .iter()
            .map(|(id, why)| (id.as_str(), *why))
            .collect();
        deleted.sort_by_key(|(id, _)| *id);
        assert_eq!(
            deleted,
            [
                ("gguf/gemma-4-E2B-it-assistant-F16", Pruned::Companion),
                ("gguf/llama-3.2-3b", Pruned::Retired),
                ("gguf/lost-by-hand", Pruned::Ghost),
                ("gguf/mtp-gemma-4-E2B-it", Pruned::Companion),
                ("ollama/llama3.2", Pruned::Unlisted),
            ]
        );
        assert_eq!(plan.unavailable, ["ollama/mistral"]);
    }

    #[test]
    fn an_ollama_server_that_did_not_answer_changes_none_of_its_rows() {
        use ModelCategory::{Ollama, Whisper};
        let unassigned = row("ollama/llama3.2", Ollama, false, true);
        let assigned = row("ollama/mistral", Ollama, false, true);
        let bundled = row("whisper/base", Whisper, false, false);
        let records = vec![unassigned, assigned, bundled.clone()];
        let plan = prune_plan(
            &records,
            &[bundled],
            &[assigned_one("chat", "ollama/mistral")],
        );
        assert_eq!(plan, PrunePlan::default());
    }

    fn assigned_one(role: &str, id: &str) -> ModelRoleAssignment {
        ModelRoleAssignment {
            role: role.into(),
            model_id: id.into(),
        }
    }

    /// The seed path itself: upsert with the disk's word, keep a file-less row's own, then prune.
    #[tokio::test]
    async fn applying_a_catalogue_upserts_then_prunes() {
        use crate::models::mocks::mock_model_repository::MockModelRepository as Repo;
        let repo = Repo::new();
        let stale = row("gguf/llama-3.2-3b", ModelCategory::Gguf, false, false);
        repo.upsert(&stale).await.unwrap();

        let pick = row("gguf/pick", ModelCategory::Gguf, false, false);
        let mut ollama = row("ollama/qwen3:4b", ModelCategory::Ollama, false, true);
        ollama.filename = None;
        let present = |r: &ModelRecord| r.filename.as_ref().map(|f| f == "pick.gguf");
        let applied = apply_catalog(&repo, vec![pick, ollama], &present)
            .await
            .unwrap();
        assert_eq!(
            applied,
            CatalogApplied {
                upserted: 2,
                pruned: 1
            }
        );
        assert!(repo.get_by_id("gguf/llama-3.2-3b").await.unwrap().is_none());
        assert!(
            repo.get_by_id("gguf/pick")
                .await
                .unwrap()
                .unwrap()
                .downloaded
        );
        assert!(
            repo.get_by_id("ollama/qwen3:4b")
                .await
                .unwrap()
                .unwrap()
                .downloaded,
            "a listed Ollama model is available: it has no file to check"
        );
    }

    // ── Boot restore ──────────────────────────────────────────────────────

    fn assigned(role: &str, id: &str) -> ModelRoleAssignment {
        ModelRoleAssignment {
            role: role.into(),
            model_id: id.into(),
        }
    }

    #[test]
    fn a_restore_fetches_only_assigned_files_that_are_missing() {
        let missing = stub_model(
            "gguf/gone",
            "gone.gguf",
            true,
            Some("https://example.com/g"),
        );
        let flagless = stub_model("gguf/here", "here.gguf", false, None);
        let sourceless = stub_model("gguf/hand", "hand.gguf", false, None);
        let unassigned = stub_model("gguf/other", "other.gguf", false, Some("https://e/o"));
        let mut ollama = stub_model("ollama/llama3.2", "", false, None);
        ollama.filename = None;
        let records = vec![
            missing.clone(),
            flagless.clone(),
            sourceless,
            unassigned,
            ollama,
        ];
        let assignments = vec![
            assigned("chat", "gguf/gone"),
            assigned("think", "gguf/gone"),
            assigned("task", "gguf/here"),
            assigned("tool", "gguf/hand"),
            assigned("embedding", "ollama/llama3.2"),
            assigned("asr", "whisper/none"),
        ];
        let on_disk = |r: &ModelRecord| match r.filename.as_deref() {
            None => None,
            Some("here.gguf") => Some(true),
            Some(_) => Some(false),
        };
        let plan = restore_plan(&assignments, &records, on_disk);
        assert_eq!(plan.mark_downloaded, vec!["gguf/here".to_string()]);
        assert_eq!(
            plan.fetch.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["gguf/gone"],
            "once each, never an unassigned row or one with no source"
        );
    }

    #[test]
    fn a_pick_restores_from_its_pin_even_without_a_url() {
        let pick = crate::models::domain::curated::CURATED[0];
        let mut row = stub_model(&pick.id(), pick.filename, true, None);
        row.category = pick.category();
        row.name = pick.name().to_string();
        let plan = restore_plan(&[assigned("chat", &pick.id())], &[row], |_| Some(false));
        assert_eq!(plan.fetch.len(), 1);
    }

    #[tokio::test]
    async fn sync_disk_flags_corrects_stale_records() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = Arc::new(MockModelRepository::new());
        let dl = Arc::new(MockDownloader::new());
        let svc = make_service(repo.clone(), dl, tmp.path());

        // model_a: file exists on disk, but DB says downloaded=false
        std::fs::File::create(tmp.path().join("a.llamafile")).unwrap();
        repo.upsert(&stub_model("a", "a.llamafile", false, None))
            .await
            .unwrap();

        // model_b: file does NOT exist, but DB says downloaded=true
        repo.upsert(&stub_model("b", "b.llamafile", true, None))
            .await
            .unwrap();

        let changed = svc.sync_disk_flags().await.unwrap();
        assert_eq!(changed, 2, "both records should have been corrected");

        assert!(
            repo.get_by_id("a").await.unwrap().unwrap().downloaded,
            "model_a should now be downloaded=true"
        );
        assert!(
            !repo.get_by_id("b").await.unwrap().unwrap().downloaded,
            "model_b should now be downloaded=false"
        );
    }
}
