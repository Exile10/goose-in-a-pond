//! One pipeline acquires a model with its add-ons. It says what it will download before anything
//! starts, runs every file through the download tracker, and when a part arrives marks the row
//! downloaded and hands the model to the agent to settle (register, verify, attach the add-on).
//! Nothing here sends a request: the transfers are the tracked downloads in `routes.rs`.

use std::path::Path;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use pond_core::models::domain::curated;
use pond_core::models::domain::device_budget::{self, VisionFit};
use pond_core::models::domain::model_layout;
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::domain::taxonomy;
use pond_core::models::domain::vision_encoder::{self, EncoderSpec, OnDisk};
use pond_core::models::domain::vision_pairing::{encoder_for_model, encoder_for_source};
use pond_core::models::services::model_service;

use crate::routes::{spawn_tracked_download, TrackedFile};
use crate::AppState;

pub(crate) const PART_MODEL: &str = "model";
pub(crate) const PART_PICTURES: &str = "pictures";

type Refusal = (StatusCode, Json<Value>);

fn refusal(status: StatusCode, code: &str, error: impl Into<String>) -> Refusal {
    (status, Json(json!({"error": error.into(), "code": code})))
}

/// The picture add-on as far as one model is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pictures {
    /// No add-on GIAP knows of: the model reads text only.
    TextOnly,
    /// It has one and this device will not carry it.
    NotOnThisDevice(EncoderSpec),
    /// On disk already.
    Installed(EncoderSpec),
    /// Not on disk: fetched when the household includes it.
    Available(EncoderSpec),
}

impl Pictures {
    fn wire(&self, included: bool) -> &'static str {
        match self {
            Self::TextOnly => "text_only",
            Self::NotOnThisDevice(_) => "not_on_this_device",
            Self::Installed(_) => "installed",
            Self::Available(_) if included => "included",
            Self::Available(_) => "left_out",
        }
    }
}

/// The add-on for `record`: by the source it came from, then its file name and header, and
/// only where this device carries it.
pub(crate) fn pictures_for(data_dir: &Path, record: &ModelRecord) -> Pictures {
    if record.category != ModelCategory::Gguf {
        return Pictures::TextOnly;
    }
    let model_path = record
        .filename
        .as_deref()
        .and_then(|f| model_layout::path_for(data_dir, &record.category, f));
    let from_source = record
        .url
        .as_deref()
        .and_then(pond_hf_cache::parse_hf_url)
        .and_then(|(repo, _, file)| encoder_for_source(&repo, &file));
    let Some(spec) = from_source.or_else(|| encoder_for_model(&record.name, model_path.as_deref()))
    else {
        return Pictures::TextOnly;
    };
    let declared = device_budget::declare(
        Some(spec),
        device_budget::budgeted_device(),
        device_budget::DEVICE_MEASURED_VISION,
        |s| match model_path.as_deref().filter(|p| p.exists()) {
            Some(p) => device_budget::vision_fit_on_device(p, Some(s)),
            // Weights not on disk yet never prove a fit.
            None => VisionFit::CostsWindow {
                window_without: device_budget::MIN_CTX,
                window_with: device_budget::MIN_CTX,
            },
        },
    );
    if !declared.is_declared() {
        return Pictures::NotOnThisDevice(spec);
    }
    match vision_encoder::encoder_on_disk(&vision_encoder::encoder_file(data_dir, &spec), &spec) {
        OnDisk::Verified { .. } | OnDisk::Unverified { .. } => Pictures::Installed(spec),
        OnDisk::Missing | OnDisk::Invalid(_) => Pictures::Available(spec),
    }
}

/// The model file itself, from its pin when it is one of GIAP's picks.
fn model_file(data_dir: &Path, record: &ModelRecord) -> Result<TrackedFile, Refusal> {
    if matches!(
        record.category,
        ModelCategory::Ollama | ModelCategory::TtsHttp
    ) {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            "external",
            "This model is managed by another program, not downloaded by the pond.",
        ));
    }
    let filename = record
        .filename
        .as_deref()
        .and_then(model_layout::file_name)
        .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "no_file", "model has no filename"))?;
    let dest = model_layout::path_for(data_dir, &record.category, filename).ok_or_else(|| {
        refusal(
            StatusCode::BAD_REQUEST,
            "no_file",
            "this model has no file to download",
        )
    })?;
    let url = curated::for_record(record)
        .map(|pick| pick.url())
        .or_else(|| record.url.clone())
        .ok_or_else(|| {
            refusal(
                StatusCode::BAD_REQUEST,
                "no_url",
                "model has no download URL",
            )
        })?;
    Ok(TrackedFile {
        url,
        dest,
        key: filename.to_string(),
        category: record.category.as_str().to_string(),
        model_id: Some(record.id.clone()),
        part: Some(PART_MODEL),
        size_bytes: curated::for_record(record)
            .map(|pick| pick.size_bytes)
            .or((record.size_mb > 0).then(|| record.size_mb * 1_048_576)),
    })
}

/// The add-on's encoder, pinned, into its own directory.
fn picture_file(data_dir: &Path, model_id: &str, spec: &EncoderSpec) -> TrackedFile {
    TrackedFile {
        url: spec.url(),
        dest: vision_encoder::encoder_file(data_dir, spec),
        key: format!("mmproj/{}/{}", spec.dir, spec.filename),
        category: "mmproj".to_string(),
        model_id: Some(model_id.to_string()),
        part: Some(PART_PICTURES),
        size_bytes: Some(spec.size_bytes),
    }
}

/// Whole decimal gigabytes from 1 GB up, as the catalogue writes them; megabytes below, as the
/// picture-support copy does.
pub(crate) fn size_label(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", vision_encoder::mb(bytes))
    }
}

/// What the household reads when the download starts.
fn announcement(
    title: &str,
    model: Option<&TrackedFile>,
    pictures: Option<&TrackedFile>,
) -> String {
    let sized = |f: &TrackedFile| f.size_bytes.map(|b| format!(" ({})", size_label(b)));
    match (model, pictures) {
        (Some(m), Some(p)) => format!(
            "Downloading {title}{} and picture support{}",
            sized(m).unwrap_or_default(),
            sized(p).unwrap_or_default()
        ),
        (Some(m), None) => format!("Downloading {title}{}", sized(m).unwrap_or_default()),
        (None, Some(p)) => format!(
            "Downloading picture support for {title}{}",
            sized(p).unwrap_or_default()
        ),
        (None, None) => format!("Nothing to download for {title}"),
    }
}

fn parts_json(files: &[TrackedFile]) -> Vec<Value> {
    files
        .iter()
        .map(|f| {
            json!({
                "part": f.part,
                "filename": f.key,
                "size_bytes": f.size_bytes,
            })
        })
        .collect()
}

/// Refuse up front what the network mode would refuse mid-transfer.
fn check_egress(files: &[TrackedFile]) -> Result<(), Refusal> {
    for f in files {
        pond_core::shared::services::egress::check_egress(&f.url)
            .map_err(|denied| refusal(StatusCode::BAD_GATEWAY, "blocked", denied.to_string()))?;
    }
    Ok(())
}

/// Register every part with the tracker at once, so progress lists them all, then fetch.
async fn start(state: &Arc<AppState>, files: Vec<TrackedFile>) {
    for f in &files {
        crate::routes::begin_tracking(&state.download_tracker, f).await;
    }
    for f in files {
        let state = state.clone();
        tokio::spawn(async move { run(state, f).await });
    }
}

async fn run(state: Arc<AppState>, file: TrackedFile) {
    let Some(data_dir) = state.data_dir.clone() else {
        return;
    };
    let on_done = arrival(state.clone(), file.model_id.clone(), file.part);
    spawn_tracked_download(
        file,
        state.download_tracker.clone(),
        state.http_client.clone(),
        data_dir,
        on_done,
    )
    .await;
}

/// What happens when one part has arrived: the model part marks its row downloaded (with its
/// real size), and any part hands an in-process model to the agent to settle.
pub(crate) async fn arrival(
    state: Arc<AppState>,
    model_id: Option<String>,
    part: Option<&'static str>,
) {
    let (Some(model_id), Some(repo)) = (model_id, state.model_repo.clone()) else {
        return;
    };
    let Ok(Some(mut record)) = repo.get_by_id(&model_id).await else {
        return;
    };
    if part == Some(PART_MODEL) {
        crate::routes::fetch_tts_config(&state, &record).await;
        let on_disk = state.data_dir.as_deref().and_then(|dd| {
            let f = record.filename.as_deref()?;
            let path = model_layout::path_for(dd, &record.category, f)?;
            std::fs::metadata(path).ok().map(|m| m.len() / 1_048_576)
        });
        record.downloaded = true;
        if let Some(mb) = on_disk.filter(|mb| *mb > 0 && record.size_mb == 0) {
            record.size_mb = mb;
            let _ = repo.upsert(&record).await;
        }
        let _ = repo.set_downloaded(&model_id, true).await;
    }
    if matches!(record.category, ModelCategory::Gguf | ModelCategory::Litert) {
        state.agent.prepare_model(&record.name);
    }
}

/// What a download of one model will fetch, decided before anything starts.
#[derive(Debug)]
pub(crate) struct Plan {
    pub files: Vec<TrackedFile>,
    pub pictures: Pictures,
    pub include_pictures: bool,
    pub title: String,
}

impl Plan {
    /// What the household reads when the download starts, from the parts as they now stand.
    pub fn message(&self) -> String {
        let part = |name| self.files.iter().find(|f| f.part == Some(name));
        announcement(&self.title, part(PART_MODEL), part(PART_PICTURES))
    }
}

/// `record`'s file, and the picture add-on when the model has one, this device carries it and
/// the household left it included.
pub(crate) fn plan(
    data_dir: &Path,
    record: &ModelRecord,
    include_pictures: bool,
) -> Result<Plan, Refusal> {
    let model = model_file(data_dir, record)?;
    let pictures = pictures_for(data_dir, record);
    let picture = match pictures {
        Pictures::Available(spec) if include_pictures => {
            Some(picture_file(data_dir, &record.id, &spec))
        }
        _ => None,
    };
    Ok(Plan {
        files: std::iter::once(model).chain(picture).collect(),
        pictures,
        include_pictures,
        title: taxonomy::title(record, curated::for_record(record).map(|p| p.title)),
    })
}

/// A model file whose size nothing has stated is asked for it before anything is fetched, so
/// the announcement says the number first; the row keeps what was learned.
async fn learn_model_size(
    state: &AppState,
    data_dir: &Path,
    record: &ModelRecord,
    plan: &mut Plan,
) {
    let Some(model) = plan
        .files
        .iter_mut()
        .find(|f| f.part == Some(PART_MODEL) && f.size_bytes.is_none())
    else {
        return;
    };
    let Some(bytes) = crate::routes::remote_size(data_dir, &model.url).await else {
        return;
    };
    model.size_bytes = Some(bytes);
    if let (0, Some(repo)) = (record.size_mb, state.model_repo.as_ref()) {
        let mut sized = record.clone();
        sized.size_mb = bytes / 1_048_576;
        if let Err(e) = repo.upsert(&sized).await {
            tracing::warn!(model = %record.id, error = %e, "could not record the size learned");
        }
    }
}

/// Plan and start `record`'s download; see [`plan`].
pub(crate) async fn acquire(
    state: &Arc<AppState>,
    record: &ModelRecord,
    include_pictures: bool,
) -> Result<Value, Refusal> {
    let data_dir = state.data_dir.clone().ok_or_else(|| {
        refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_data_dir",
            "data_dir not configured",
        )
    })?;
    let mut plan = plan(&data_dir, record, include_pictures)?;
    check_egress(&plan.files)?;
    learn_model_size(state, &data_dir, record, &mut plan).await;
    let parts = parts_json(&plan.files);
    let pictures = plan.pictures.wire(plan.include_pictures);
    let message = plan.message();
    start(state, plan.files).await;
    Ok(json!({
        "status": "download_started",
        "name": record.name,
        "category": record.category.as_str(),
        "model_id": record.id,
        "parts": parts,
        "pictures": pictures,
        "message": message,
    }))
}

/// The row a file downloaded by URL becomes: its own, or an existing row of the same name.
pub(crate) async fn row_for_url(
    state: &AppState,
    url: &str,
    category: &ModelCategory,
    filename: &str,
) -> Result<ModelRecord, Refusal> {
    let name = taxonomy::catalogue_name(category, filename);
    let id = ModelRecord::id_for(category, &name);
    let repo = state.model_repo.as_ref().ok_or_else(|| {
        refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_registry",
            "registry not available",
        )
    })?;
    if let Ok(Some(existing)) = repo.get_by_id(&id).await {
        return Ok(existing);
    }
    let record = ModelRecord {
        id,
        category: category.clone(),
        name: name.clone(),
        filename: Some(filename.to_string()),
        description: String::new(),
        size_mb: 0,
        url: Some(url.to_string()),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: category.is_llm().then(|| "chat".to_string()),
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
        is_custom: true,
    };
    repo.upsert(&record)
        .await
        .map_err(|e| refusal(StatusCode::INTERNAL_SERVER_ERROR, "store", e.to_string()))?;
    Ok(record)
}

/// `POST /api/v1/models/{category}/{name}/companions/pictures` — add picture support to a model.
pub(crate) async fn add_pictures(
    State(state): State<Arc<AppState>>,
    UrlPath((category, name)): UrlPath<(String, String)>,
) -> Result<Json<Value>, Refusal> {
    let (Some(repo), Some(data_dir)) = (state.model_repo.clone(), state.data_dir.clone()) else {
        return Err(refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_registry",
            "registry not available",
        ));
    };
    let category = ModelCategory::from_str(&category).ok_or_else(|| {
        refusal(
            StatusCode::BAD_REQUEST,
            "unknown_category",
            format!("Unknown category '{category}'"),
        )
    })?;
    let record = repo
        .get_by_id(&ModelRecord::id_for(&category, &name))
        .await
        .map_err(|e| refusal(StatusCode::INTERNAL_SERVER_ERROR, "store", e.to_string()))?
        .ok_or_else(|| {
            refusal(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("Model '{name}' not found in '{}'", category.as_str()),
            )
        })?;
    let title = taxonomy::title(&record, curated::for_record(&record).map(|p| p.title));
    match pictures_for(&data_dir, &record) {
        Pictures::TextOnly => Err(refusal(
            StatusCode::CONFLICT,
            "text_only",
            format!("{title} reads text only; there is no picture support to add."),
        )),
        Pictures::NotOnThisDevice(spec) => Err(refusal(
            StatusCode::CONFLICT,
            "not_on_this_device",
            vision_encoder::not_on_this_device_message(
                Some(spec.label),
                device_budget::DEVICE_MEASURED_VISION,
            ),
        )),
        Pictures::Installed(spec) => {
            state.agent.prepare_model(&record.name);
            Ok(Json(json!({
                "status": "already_installed",
                "model_id": record.id,
                "size_bytes": spec.size_bytes,
            })))
        }
        Pictures::Available(spec) => {
            let file = picture_file(&data_dir, &record.id, &spec);
            let message = announcement(&title, None, Some(&file));
            let files = vec![file];
            check_egress(&files)?;
            let parts = parts_json(&files);
            start(&state, files).await;
            Ok(Json(json!({
                "status": "download_started",
                "model_id": record.id,
                "parts": parts,
                "message": message,
            })))
        }
    }
}

/// Brings back the household's own assigned models whose files are missing, through the
/// tracker so the download shows. An add-on comes back only if the model had one: something
/// is still at its encoder's place. Nothing unassigned is ever fetched.
pub async fn restore_assigned_models(state: Arc<AppState>) -> usize {
    let (Some(repo), Some(data_dir)) = (state.model_repo.clone(), state.data_dir.clone()) else {
        return 0;
    };
    let (Ok(records), Ok(assignments)) = (repo.list_all().await, repo.list_assignments().await)
    else {
        return 0;
    };
    let plan = model_service::restore_plan(&assignments, &records, |r| {
        let path = model_layout::path_for(&data_dir, &r.category, r.filename.as_deref()?)?;
        Some(path.exists())
    });
    for id in &plan.mark_downloaded {
        if repo.set_downloaded(id, true).await.is_ok() {
            tracing::info!(model = %id, "restore: its file is on disk; marked downloaded");
        }
    }
    let mut started = 0usize;
    for record in &plan.fetch {
        let target = record
            .filename
            .as_deref()
            .and_then(|f| model_layout::path_for(&data_dir, &record.category, f));
        if record.category == ModelCategory::Gguf
            && target.as_deref().is_some_and(other_quant_on_disk)
        {
            tracing::info!(model = %record.id, "restore: another quant of this model is on disk; not fetching");
            continue;
        }
        let model = match model_file(&data_dir, record) {
            Ok(f) => f,
            Err((_, Json(why))) => {
                tracing::warn!(model = %record.id, reason = %why["error"], "restore: cannot fetch an assigned model");
                continue;
            }
        };
        let picture = match pictures_for(&data_dir, record) {
            Pictures::Available(spec) if had_pictures(&data_dir, &spec) => {
                Some(picture_file(&data_dir, &record.id, &spec))
            }
            _ => None,
        };
        let files: Vec<TrackedFile> = std::iter::once(model).chain(picture).collect();
        if let Err((_, Json(why))) = check_egress(&files) {
            tracing::warn!(model = %record.id, reason = %why["error"], "restore: the network mode refuses it");
            continue;
        }
        tracing::info!(model = %record.id, parts = files.len(), "restore: fetching an assigned model whose file is missing");
        start(&state, files).await;
        started += 1;
    }
    started
}

/// Whether another quant of the model `target` names is on disk: a `.gguf` sharing the stem before
/// its last `-Q`, then `-Q`. A restore leaves such a household's copy alone rather than fetch one.
fn other_quant_on_disk(target: &Path) -> bool {
    let (Some(dir), Some(stem)) = (target.parent(), target.file_stem().and_then(|s| s.to_str()))
    else {
        return false;
    };
    let Some(prefix) = stem
        .rfind("-Q")
        .map(|i| &stem[..i])
        .filter(|p| !p.is_empty())
    else {
        return false;
    };
    let lead = format!("{prefix}-Q");
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            e.path() != target && name.starts_with(&lead) && name.ends_with(".gguf")
        })
    })
}

/// Whether anything is left of the add-on's install, which is how a restore knows it was wanted.
fn had_pictures(data_dir: &Path, spec: &EncoderSpec) -> bool {
    vision_encoder::encoder_file(data_dir, spec)
        .parent()
        .is_some_and(|dir| std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_the_catalogue_and_the_picture_copy_write_them() {
        assert_eq!(size_label(4_215_695_776), "4.2 GB");
        assert_eq!(size_label(2_620_370_976), "2.6 GB");
        assert_eq!(size_label(991_552_320), "945 MB");
        assert_eq!(size_label(986_833_728), "941 MB");
    }

    fn file(part: &'static str, bytes: Option<u64>) -> TrackedFile {
        TrackedFile {
            url: "https://huggingface.co/a/b/resolve/c/d.gguf".into(),
            dest: "/pond/models/gguf/d.gguf".into(),
            key: "d.gguf".into(),
            category: "gguf".into(),
            model_id: Some("gguf/d".into()),
            part: Some(part),
            size_bytes: bytes,
        }
    }

    #[test]
    fn the_announcement_says_what_comes_down_and_how_big() {
        let m = file(PART_MODEL, Some(4_215_695_776));
        let p = file(PART_PICTURES, Some(991_552_320));
        assert_eq!(
            announcement("Gemma 4 E4B", Some(&m), Some(&p)),
            "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)"
        );
        assert_eq!(
            announcement("Gemma 4 E4B", Some(&m), None),
            "Downloading Gemma 4 E4B (4.2 GB)"
        );
        assert_eq!(
            announcement("Gemma 4 E4B", None, Some(&p)),
            "Downloading picture support for Gemma 4 E4B (945 MB)"
        );
        assert_eq!(
            announcement("My model", Some(&file(PART_MODEL, None)), None),
            "Downloading My model"
        );
    }

    fn row_for_pick(pick: &curated::CuratedModel) -> ModelRecord {
        ModelRecord {
            id: pick.id(),
            category: pick.category(),
            name: pick.name().to_string(),
            filename: Some(pick.filename.to_string()),
            description: pick.summary.to_string(),
            size_mb: pick.size_mb(),
            url: Some(pick.url()),
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: Some("chat".into()),
            context_length: Some(pick.context_length),
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

    /// The two parts a household's E4B download would fetch, each pinned, decided offline.
    #[test]
    fn a_pick_with_an_add_on_plans_two_pinned_parts() {
        if device_budget::budgeted_device() {
            return; // the Orin carries no add-on; see the next test's NotOnThisDevice
        }
        let tmp = tempfile::tempdir().unwrap();
        let pick = curated::find(&ModelCategory::Gguf, "gemma-4-E4B-it-qat-UD-Q4_K_XL").unwrap();
        let row = row_for_pick(pick);

        let plan = plan(tmp.path(), &row, true).unwrap();
        assert_eq!(plan.pictures.wire(true), "included");
        assert_eq!(plan.files.len(), 2);
        let model = &plan.files[0];
        assert_eq!(model.part, Some(PART_MODEL));
        assert_eq!(model.key, pick.filename);
        assert_eq!(model.url, pick.url());
        assert_eq!(model.size_bytes, Some(4_215_695_776));
        assert_eq!(
            model.dest,
            tmp.path().join("models/gguf").join(pick.filename)
        );
        let pictures = &plan.files[1];
        assert_eq!(pictures.part, Some(PART_PICTURES));
        assert_eq!(pictures.model_id.as_deref(), Some(row.id.as_str()));
        assert_eq!(pictures.key, "mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf");
        assert_eq!(pictures.size_bytes, Some(991_552_320));
        assert_eq!(
            pictures.dest,
            tmp.path()
                .join("models/mmproj/gemma-4-e4b-it-qat/mmproj-BF16.gguf")
        );
        let (repo, revision, file) = pond_hf_cache::parse_hf_url(&pictures.url).unwrap();
        assert!(
            curated::file_pin(&repo, &revision, &file).is_some(),
            "the add-on downloads its pinned revision"
        );
        assert_eq!(
            plan.message(),
            "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)"
        );

        let without = super::plan(tmp.path(), &row, false).unwrap();
        assert_eq!(without.files.len(), 1, "unticked: no add-on is fetched");
        assert_eq!(without.pictures.wire(false), "left_out");
    }

    /// A file named by URL is planned like a pick: a listed file brings its add-on, and once its
    /// own size is learned the announcement names both numbers under the table's name for it.
    #[test]
    fn a_file_named_by_url_announces_both_sizes_once_learned() {
        if device_budget::budgeted_device() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let mut row = row_for_pick(&curated::CURATED[0]);
        row.name = "SmolVLM-256M-Instruct-Q8_0".into();
        row.id = ModelRecord::id_for(&ModelCategory::Gguf, &row.name);
        row.filename = Some("SmolVLM-256M-Instruct-Q8_0.gguf".into());
        row.url = Some(
            "https://huggingface.co/ggml-org/SmolVLM-256M-Instruct-GGUF/resolve/main/\
             SmolVLM-256M-Instruct-Q8_0.gguf"
                .into(),
        );
        row.size_mb = 0;
        row.is_custom = true;

        let mut plan = plan(tmp.path(), &row, true).unwrap();
        assert_eq!(plan.files.len(), 2);
        assert_eq!(
            plan.files[0].size_bytes, None,
            "nothing has said its size yet"
        );
        assert_eq!(
            plan.message(),
            "Downloading SmolVLM 256M and picture support (181 MB)"
        );
        plan.files[0].size_bytes = Some(290_000_000);
        assert_eq!(
            plan.message(),
            "Downloading SmolVLM 256M (276 MB) and picture support (181 MB)"
        );
    }

    #[test]
    fn a_litert_pick_is_text_only_and_downloads_its_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let pick = curated::find(&ModelCategory::Litert, "gemma-4-E2B-it.litertlm").unwrap();
        let plan = plan(tmp.path(), &row_for_pick(pick), true).unwrap();
        assert_eq!(plan.pictures, Pictures::TextOnly);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].url, pick.url());
        assert_eq!(
            plan.files[0].dest,
            tmp.path().join("models/litertlm").join(pick.filename)
        );
    }

    #[test]
    fn an_ollama_row_is_not_downloaded_by_the_pond() {
        let tmp = tempfile::tempdir().unwrap();
        let pick = curated::CURATED[0];
        let mut row = row_for_pick(&pick);
        row.category = ModelCategory::Ollama;
        row.filename = None;
        row.url = None;
        let (status, Json(body)) = plan(tmp.path(), &row, true).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "external");
    }

    #[test]
    fn a_name_cannot_steer_the_file_outside_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let pick = curated::CURATED[0];
        let mut row = row_for_pick(&pick);
        row.filename = Some("../../escape.gguf".into());
        row.url = Some("https://example.com/escape.gguf".into());
        row.name = "escape".into();
        row.id = "gguf/escape".into();
        let plan = plan(tmp.path(), &row, false).unwrap();
        assert_eq!(
            plan.files[0].dest,
            tmp.path().join("models/gguf/escape.gguf")
        );
        assert_eq!(plan.files[0].key, "escape.gguf");
    }

    #[test]
    fn the_wire_names_every_add_on_outcome() {
        let spec = pond_core::models::domain::vision_encoder::encoder_by_dir("gemma-4-e4b-it-qat")
            .unwrap();
        assert_eq!(Pictures::TextOnly.wire(true), "text_only");
        assert_eq!(
            Pictures::NotOnThisDevice(spec).wire(true),
            "not_on_this_device"
        );
        assert_eq!(Pictures::Installed(spec).wire(false), "installed");
        assert_eq!(Pictures::Available(spec).wire(true), "included");
        assert_eq!(Pictures::Available(spec).wire(false), "left_out");
    }

    #[test]
    fn another_quant_is_the_same_model_and_a_qat_build_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("gemma-4-E4B-it-Q4_K_M.gguf");
        assert!(!other_quant_on_disk(&target));
        std::fs::write(tmp.path().join("gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"), b"x").unwrap();
        std::fs::write(tmp.path().join("gemma-4-E4B-it-Q4_K_S.gguf.part"), b"x").unwrap();
        assert!(
            !other_quant_on_disk(&target),
            "a qat build or a partial file"
        );
        std::fs::write(tmp.path().join("gemma-4-E4B-it-Q4_K_S.gguf"), b"x").unwrap();
        assert!(other_quant_on_disk(&target));
        assert!(
            !other_quant_on_disk(&tmp.path().join("gemma-4-E4B-it.gguf")),
            "a name with no quant has no other quant"
        );
    }
}
