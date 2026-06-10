//! HF-compatible cache layout for model downloads.
//!
//! Mirrors the upstream `huggingface/hf-hub` crate's on-disk layout so that
//! files written here are interoperable with the Python `huggingface_hub`
//! cache. Path encoding + token discovery are pure; `HfFetch::download_to_blob`
//! performs the network + filesystem work for one file.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Env vars consulted (in order) for an HF access token.
const HF_TOKEN_ENV_VARS: &[&str] = &["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HUGGINGFACE_TOKEN"];

/// Root of the HF-compatible cache.
pub struct HfCache {
    root: PathBuf,
    token: Option<String>,
}

/// A handle to one HF repo within the cache.
pub struct HfRepo<'a> {
    cache: &'a HfCache,
    repo_id: String,
    revision: String,
}

/// A handle to one file within a repo.
pub struct HfFetch<'a> {
    repo: &'a HfRepo<'a>,
    filename: String,
}

impl HfCache {
    /// Construct a cache rooted at `$HF_HOME` if set, else `{data_dir}/hf_cache`.
    pub fn new(data_dir: &Path) -> Self {
        let root = match std::env::var_os("HF_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let mut p = data_dir.to_path_buf();
                p.push("hf_cache");
                p
            }
        };
        let token = discover_token(&root);
        Self { root, token }
    }

    /// Cache root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `{root}/hub` — parent of all `models--*` repo folders.
    pub fn hub_dir(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.push("hub");
        p
    }

    /// `{root}/token` — Python-compatible token file location.
    pub fn token_path(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.push("token");
        p
    }

    /// Cached HF access token, if any was discovered at construction.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Build a repo handle (default revision = `"main"`).
    pub fn repo(&self, repo_id: impl Into<String>) -> HfRepo<'_> {
        HfRepo {
            cache: self,
            repo_id: repo_id.into(),
            revision: "main".to_string(),
        }
    }
}

/// Resolve the token via env vars first, then the `{root}/token` file.
fn discover_token(root: &Path) -> Option<String> {
    for var in HF_TOKEN_ENV_VARS {
        if let Ok(v) = std::env::var(var) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    let mut token_file = root.to_path_buf();
    token_file.push("token");
    match std::fs::read_to_string(&token_file) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => None,
    }
}

impl<'a> HfRepo<'a> {
    /// Override the revision (branch / tag / commit) — default is `"main"`.
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = revision.into();
        self
    }

    /// `models--{org}--{repo}` (slashes in `repo_id` become `--`).
    pub fn folder_name(&self) -> String {
        format!("models--{}", self.repo_id).replace('/', "--")
    }

    /// `{hub_dir}/models--{org}--{repo}`.
    pub fn folder_path(&self) -> PathBuf {
        let mut p = self.cache.hub_dir();
        p.push(self.folder_name());
        p
    }

    /// `{folder_path}/refs/{branch}` — text file holding a commit hash.
    pub fn refs_path(&self, branch: &str) -> PathBuf {
        let mut p = self.folder_path();
        p.push("refs");
        p.push(branch);
        p
    }

    /// `{folder_path}/snapshots/{commit}` — directory of symlinks to blobs.
    pub fn snapshot_dir(&self, commit: &str) -> PathBuf {
        let mut p = self.folder_path();
        p.push("snapshots");
        p.push(commit);
        p
    }

    /// `{folder_path}/blobs/{etag}` — content-addressed real file.
    pub fn blob_path(&self, etag: &str) -> PathBuf {
        let mut p = self.folder_path();
        p.push("blobs");
        p.push(etag);
        p
    }

    /// Build a file handle within this repo.
    pub fn file(&self, filename: impl Into<String>) -> HfFetch<'_> {
        HfFetch {
            repo: self,
            filename: filename.into(),
        }
    }
}

impl<'a> HfFetch<'a> {
    /// `{snapshot_dir}/{filename}` — where the symlink to the blob will live.
    pub fn pointer_path(&self, commit: &str) -> PathBuf {
        let mut p = self.repo.snapshot_dir(commit);
        p.push(&self.filename);
        p
    }

    /// `{blobs}/{etag}` — same as `HfRepo::blob_path`.
    pub fn blob_path(&self, etag: &str) -> PathBuf {
        self.repo.blob_path(etag)
    }

    /// `https://huggingface.co/{repo_id}/resolve/{revision}/{filename}`.
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo.repo_id,
            url_escape_revision(&self.repo.revision),
            self.filename
        )
    }
}

/// Percent-encode `/` in a revision so branches like `feature/x` survive the URL.
fn url_escape_revision(rev: &str) -> String {
    rev.replace('/', "%2F")
}

// ── URL parsing ──────────────────────────────────────────────────────────────

/// Parse `https://huggingface.co/{org}/{repo}/resolve/{revision}/{path}` into
/// `(repo_id, revision, filename)`. Returns `None` for any URL that does not
/// match this exact shape.
pub fn parse_hf_url(url: &str) -> Option<(String, String, String)> {
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, rest) = after_scheme.split_once('/')?;
    if host != "huggingface.co" {
        return None;
    }
    // Need at least: {org}/{repo}/resolve/{rev}/{file...}
    let parts: Vec<&str> = rest.splitn(5, '/').collect();
    if parts.len() < 5 {
        return None;
    }
    if parts[2] != "resolve" {
        return None;
    }
    let org = parts[0];
    let repo = parts[1];
    let revision = parts[3];
    let filename = parts[4];
    if org.is_empty() || repo.is_empty() || revision.is_empty() || filename.is_empty() {
        return None;
    }
    Some((
        format!("{org}/{repo}"),
        urldecode_simple(revision),
        filename.to_string(),
    ))
}

/// Decode `%2F` → `/` (just enough for revisions). Leaves other escapes alone.
fn urldecode_simple(s: &str) -> String {
    s.replace("%2F", "/").replace("%2f", "/")
}

// ── Host policy ──────────────────────────────────────────────────────────────

/// Hosts that may receive a forwarded HF bearer token across a redirect.
/// Covers `huggingface.co` itself and its CDN domains (CloudFront-fronted).
pub(crate) fn should_send_auth_on_redirect(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "huggingface.co"
        || host.ends_with(".huggingface.co")
        || host.ends_with(".cloudfront.net")
}

// ── Redirect-aware reqwest client ────────────────────────────────────────────

/// Build a `reqwest::Client` with redirects disabled — the hf_cache module
/// drives redirect chains manually so the bearer token can be re-attached only
/// on hops to HF / CloudFront hosts (and stripped on any other host).
pub fn build_redirect_aware_client(_token: Option<&str>) -> Result<reqwest::Client> {
    use reqwest::redirect::Policy;

    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|e| anyhow!("failed to build redirect-aware client: {e}"))
}

/// Strip a leading and trailing `"` (HTTP etag quoting).
fn unquote_etag(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

impl<'a> HfFetch<'a> {
    /// Resumable, etag-aware download to the content-addressed blob path.
    ///
    /// 1. `HEAD` → capture final URL, etag (or `x-linked-etag`), content-length,
    ///    and commit hash from `x-repo-commit` (else `"main"`).
    /// 2. If `blobs/{etag}` already exists with matching size, return it.
    /// 3. Else stream `GET` (with `Range: bytes={n}-` if `.incomplete` exists)
    ///    into `{blob}.incomplete`. Call `progress(downloaded, total)` per chunk.
    /// 4. Atomic rename to `blobs/{etag}`.
    /// 5. Write `refs/main` and create the snapshot symlink.
    pub async fn download_to_blob<F>(
        &self,
        client: &reqwest::Client,
        token: Option<&str>,
        mut progress: F,
    ) -> Result<PathBuf>
    where
        F: FnMut(u64, u64),
    {
        use tokio::io::AsyncWriteExt as _;

        let url = self.url();

        // Manual redirect follow so we can strip auth on cross-host hops.
        let head_resp = head_with_redirects(client, &url, token).await?;
        let headers = head_resp.headers();

        let etag = headers
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(unquote_etag)
            .or_else(|| {
                headers
                    .get("x-linked-etag")
                    .and_then(|v| v.to_str().ok())
                    .map(unquote_etag)
            })
            .ok_or_else(|| anyhow!("HEAD {url}: no etag / x-linked-etag header"))?;
        if etag.is_empty() {
            return Err(anyhow!("HEAD {url}: empty etag"));
        }

        let total: u64 = headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let commit = headers
            .get("x-repo-commit")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main".to_string());

        let blob_path = self.blob_path(&etag);
        let blobs_dir = blob_path
            .parent()
            .ok_or_else(|| anyhow!("blob path has no parent: {}", blob_path.display()))?;
        tokio::fs::create_dir_all(blobs_dir)
            .await
            .with_context(|| format!("create blobs dir {}", blobs_dir.display()))?;

        // ── Fast path: blob already complete on disk ────────────────────────
        if let Ok(meta) = tokio::fs::metadata(&blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), meta.len());
                finalize_pointers(self, &etag, &commit, &blob_path).await?;
                return Ok(blob_path);
            }
        }

        // ── Resumable download into {blob}.incomplete ───────────────────────
        let final_url = head_resp.final_url.clone();
        let mut incomplete_path = blob_path.clone();
        let incomplete_name = format!(
            "{}.incomplete",
            blob_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&etag)
        );
        incomplete_path.set_file_name(incomplete_name);

        // ── Per-blob advisory lock — at most one process does the GET ───────
        // Lock file sits beside the blob: `blobs/{etag}.lock`. The lock is
        // released when `_lock_guard` drops, or when the process exits.
        let lock_path = {
            let mut p = blob_path.clone();
            p.set_file_name(format!("{etag}.lock"));
            p
        };
        let lock_outcome = acquire_blob_lock(
            &lock_path,
            &blob_path,
            &incomplete_path,
            total,
            &mut progress,
        )
        .await?;
        let _lock_guard = match lock_outcome {
            LockOutcome::Acquired(guard) => guard,
            LockOutcome::AnotherFinished => {
                // Winner finished while we waited. Finalise pointers and return.
                finalize_pointers(self, &etag, &commit, &blob_path).await?;
                return Ok(blob_path);
            }
        };

        // After acquiring the lock, the previous holder may have finalised the
        // blob just before releasing — re-check the fast path so we don't
        // re-download bytes that are already on disk.
        if let Ok(meta) = tokio::fs::metadata(&blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), meta.len());
                finalize_pointers(self, &etag, &commit, &blob_path).await?;
                return Ok(blob_path);
            }
        }

        let existing_size = match tokio::fs::metadata(&incomplete_path).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };

        let range_header: Option<String> = if existing_size > 0 {
            Some(format!("bytes={existing_size}-"))
        } else {
            None
        };
        let resp = get_with_redirects(client, &final_url, token, range_header.as_deref()).await?;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(existing_size > 0)
            .write(true)
            .truncate(existing_size == 0)
            .open(&incomplete_path)
            .await
            .with_context(|| format!("open {}", incomplete_path.display()))?;

        let mut downloaded: u64 = existing_size;
        progress(downloaded, total);

        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .with_context(|| format!("read chunk from {final_url}"))?
        {
            file.write_all(&chunk)
                .await
                .with_context(|| format!("write {}", incomplete_path.display()))?;
            downloaded += chunk.len() as u64;
            progress(downloaded, total);
        }
        file.flush().await.ok();
        drop(file);

        tokio::fs::rename(&incomplete_path, &blob_path)
            .await
            .with_context(|| {
                format!(
                    "rename {} -> {}",
                    incomplete_path.display(),
                    blob_path.display()
                )
            })?;

        finalize_pointers(self, &etag, &commit, &blob_path).await?;
        Ok(blob_path)
    }
}

// ── Per-blob advisory locking ────────────────────────────────────────────────

/// Maximum number of poll iterations while waiting for another process to
/// finish downloading the same blob. Combined with `BLOB_LOCK_POLL_INTERVAL`
/// this bounds the wait at 10 minutes.
const BLOB_LOCK_MAX_POLLS: u32 = 600;
const BLOB_LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Holds an acquired advisory lock for the lifetime of one download attempt.
/// Dropping the inner `File` releases the OS-level lock.
struct BlobLockGuard {
    _file: std::fs::File,
}

enum LockOutcome {
    Acquired(BlobLockGuard),
    /// While waiting, the blob appeared on disk — another process finished.
    AnotherFinished,
}

/// Try to take an exclusive advisory lock on `lock_path`. If another process
/// holds it, poll until either (a) the blob appears (winner finalised), or
/// (b) the lock becomes available (winner died). While polling, surface the
/// current `.incomplete` size through `progress` so a UI can show download
/// activity even when this caller isn't the one doing the bytes.
async fn acquire_blob_lock<F>(
    lock_path: &Path,
    blob_path: &Path,
    incomplete_path: &Path,
    total: u64,
    progress: &mut F,
) -> Result<LockOutcome>
where
    F: FnMut(u64, u64),
{
    use fs2::FileExt as _;
    use std::fs::OpenOptions;

    // Ensure the parent (blobs/) dir exists — the caller already created it
    // for blob_path, but be defensive in case lock_path's parent differs.
    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    // Open or create the lock file. The handle is what fs2 locks.
    let open_lock = || -> Result<std::fs::File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .with_context(|| format!("open lock file {}", lock_path.display()))
    };

    let file = open_lock()?;
    match file.try_lock_exclusive() {
        Ok(()) => return Ok(LockOutcome::Acquired(BlobLockGuard { _file: file })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(anyhow!("lock {}: {e}", lock_path.display())),
    }
    // Drop the un-locked handle before polling — keeping it open is harmless
    // but cleaner to reopen on each poll attempt.
    drop(file);

    for _ in 0..BLOB_LOCK_MAX_POLLS {
        // Winner finished?
        if let Ok(meta) = tokio::fs::metadata(blob_path).await {
            if total == 0 || meta.len() == total {
                progress(meta.len(), if total == 0 { meta.len() } else { total });
                return Ok(LockOutcome::AnotherFinished);
            }
        }
        // Mirror the in-flight download's progress for UI smoothness.
        if let Ok(meta) = tokio::fs::metadata(incomplete_path).await {
            progress(meta.len(), total);
        }
        tokio::time::sleep(BLOB_LOCK_POLL_INTERVAL).await;

        // Did the holder die?
        let file = open_lock()?;
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(LockOutcome::Acquired(BlobLockGuard { _file: file })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                drop(file);
                continue;
            }
            Err(e) => return Err(anyhow!("lock {}: {e}", lock_path.display())),
        }
    }
    Err(anyhow!(
        "timed out waiting for blob lock {} after {}s",
        lock_path.display(),
        BLOB_LOCK_MAX_POLLS as u64 * BLOB_LOCK_POLL_INTERVAL.as_secs()
    ))
}

/// Result of a manually-followed HEAD chain.
struct HeadResult {
    final_url: String,
    headers: reqwest::header::HeaderMap,
}

impl HeadResult {
    fn headers(&self) -> &reqwest::header::HeaderMap {
        &self.headers
    }
}

/// HEAD the URL, following redirects manually. Re-attach the bearer token only
/// on hops to HF/CloudFront hosts. Returns the final URL + response headers.
async fn head_with_redirects(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<HeadResult> {
    let mut current = url.to_string();
    for _ in 0..10 {
        let host = url::Url::parse(&current)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));

        let mut req = client.head(&current);
        if let Some(t) = token {
            if let Some(h) = host.as_deref() {
                if should_send_auth_on_redirect(h) {
                    req = req.bearer_auth(t);
                }
            }
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("HEAD {current}"))?;

        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect from {current} without Location"))?;
            current = absolute_url(&current, loc)?;
            continue;
        }
        if !status.is_success() {
            return Err(anyhow!("HEAD {current} returned {status}"));
        }
        return Ok(HeadResult {
            final_url: current,
            headers: resp.headers().clone(),
        });
    }
    Err(anyhow!("too many redirects following HEAD {url}"))
}

/// GET the URL, following redirects manually so the bearer token is only
/// re-attached on hops to HF/CloudFront hosts. Returns the response stream on
/// the first non-redirect hop. Honours an optional `Range` header — re-sent on
/// every hop until the body is reached.
async fn get_with_redirects(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    range: Option<&str>,
) -> Result<reqwest::Response> {
    let mut current = url.to_string();
    for _ in 0..10 {
        let host = url::Url::parse(&current)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));

        let mut req = client.get(&current);
        if let Some(t) = token {
            if let Some(h) = host.as_deref() {
                if should_send_auth_on_redirect(h) {
                    req = req.bearer_auth(t);
                }
            }
        }
        if let Some(r) = range {
            req = req.header(reqwest::header::RANGE, r);
        }
        let resp = req.send().await.with_context(|| format!("GET {current}"))?;
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect from {current} without Location"))?;
            current = absolute_url(&current, loc)?;
            continue;
        }
        if !status.is_success() && status.as_u16() != 206 {
            return Err(anyhow!("GET {current} returned {status}"));
        }
        return Ok(resp);
    }
    Err(anyhow!("too many redirects following GET {url}"))
}

/// Resolve a `Location` header value (which may be relative) against the
/// current request URL.
fn absolute_url(current: &str, location: &str) -> Result<String> {
    let base = url::Url::parse(current).map_err(|e| anyhow!("parse {current}: {e}"))?;
    let joined = base
        .join(location)
        .map_err(|e| anyhow!("join {location} onto {current}: {e}"))?;
    Ok(joined.into())
}

/// Write `refs/{branch}` and create the snapshot symlink pointing at the blob.
async fn finalize_pointers(
    fetch: &HfFetch<'_>,
    _etag: &str,
    commit: &str,
    blob_path: &Path,
) -> Result<()> {
    let refs_path = fetch.repo.refs_path("main");
    if let Some(parent) = refs_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&refs_path, commit.as_bytes())
        .await
        .with_context(|| format!("write {}", refs_path.display()))?;

    let pointer = fetch.pointer_path(commit);
    if let Some(parent) = pointer.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    // Best-effort cleanup of stale pointer (symlink or file) before recreating.
    let _ = tokio::fs::remove_file(&pointer).await;

    let blob = blob_path.to_path_buf();
    let ptr = pointer.clone();
    tokio::task::spawn_blocking(move || create_pointer(&blob, &ptr))
        .await
        .map_err(|e| anyhow!("pointer task panicked: {e}"))??;
    Ok(())
}

#[cfg(unix)]
fn create_pointer(blob: &Path, pointer: &Path) -> Result<()> {
    std::os::unix::fs::symlink(blob, pointer)
        .with_context(|| format!("symlink {} -> {}", pointer.display(), blob.display()))
}

#[cfg(not(unix))]
fn create_pointer(blob: &Path, pointer: &Path) -> Result<()> {
    std::fs::copy(blob, pointer)
        .map(|_| ())
        .with_context(|| format!("copy {} -> {}", blob.display(), pointer.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    // Env var manipulation must be serialised — Rust tests run in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot + clear the env vars this module reads, restoring on drop.
    struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let vars = [
                "HF_HOME",
                "HF_TOKEN",
                "HUGGING_FACE_HUB_TOKEN",
                "HUGGINGFACE_TOKEN",
            ];
            let saved: Vec<_> = vars.iter().map(|v| (*v, std::env::var_os(v))).collect();
            for (v, _) in &saved {
                std::env::remove_var(v);
            }
            Self { saved, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (v, old) in &self.saved {
                match old {
                    Some(val) => std::env::set_var(v, val),
                    None => std::env::remove_var(v),
                }
            }
        }
    }

    #[test]
    fn folder_name_encodes_slashes() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/gemma-4-E2B-it-GGUF");
        assert_eq!(repo.folder_name(), "models--bartowski--gemma-4-E2B-it-GGUF");
    }

    #[test]
    fn folder_name_for_simple_repo() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("gpt2");
        assert_eq!(repo.folder_name(), "models--gpt2");
    }

    #[test]
    fn default_root_under_data_dir() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.root(), tmp.path().join("hf_cache"));
        assert_eq!(cache.hub_dir(), tmp.path().join("hf_cache").join("hub"));
    }

    #[test]
    fn hf_home_env_overrides_default() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let override_root = tmp.path().join("custom_hf");
        std::env::set_var("HF_HOME", &override_root);
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.root(), override_root.as_path());
        assert_eq!(cache.hub_dir(), override_root.join("hub"));
    }

    #[test]
    fn token_file_is_read() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("hf_cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("token"), "  hf_abc123\n").unwrap();
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.token(), Some("hf_abc123"));
    }

    #[test]
    fn empty_token_file_returns_none() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("hf_cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("token"), "   \n  \t  ").unwrap();
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.token(), None);
    }

    #[test]
    fn env_var_takes_precedence_over_token_file() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("hf_cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("token"), "from_file").unwrap();
        std::env::set_var("HF_TOKEN", "from_env");
        let cache = HfCache::new(tmp.path());
        assert_eq!(cache.token(), Some("from_env"));
    }

    #[test]
    fn url_with_default_revision() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/foo");
        let fetch = repo.file("bar.gguf");
        assert_eq!(
            fetch.url(),
            "https://huggingface.co/bartowski/foo/resolve/main/bar.gguf"
        );
    }

    #[test]
    fn url_with_branch_slashes_url_escapes() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/foo").with_revision("feature/x");
        let fetch = repo.file("bar.gguf");
        assert_eq!(
            fetch.url(),
            "https://huggingface.co/bartowski/foo/resolve/feature%2Fx/bar.gguf"
        );
    }

    #[test]
    fn parse_hf_url_simple() {
        let (repo, rev, file) =
            parse_hf_url("https://huggingface.co/bartowski/foo/resolve/main/bar.gguf").unwrap();
        assert_eq!(repo, "bartowski/foo");
        assert_eq!(rev, "main");
        assert_eq!(file, "bar.gguf");
    }

    #[test]
    fn parse_hf_url_subfolder_file() {
        let (repo, rev, file) = parse_hf_url(
            "https://huggingface.co/immich-app/antelopev2/resolve/main/recognition/model.onnx",
        )
        .unwrap();
        assert_eq!(repo, "immich-app/antelopev2");
        assert_eq!(rev, "main");
        assert_eq!(file, "recognition/model.onnx");
    }

    #[test]
    fn parse_hf_url_revision_with_encoded_slash() {
        let (repo, rev, file) =
            parse_hf_url("https://huggingface.co/foo/bar/resolve/refs%2Fpr%2F123/model.gguf")
                .unwrap();
        assert_eq!(repo, "foo/bar");
        assert_eq!(rev, "refs/pr/123");
        assert_eq!(file, "model.gguf");
    }

    #[test]
    fn parse_hf_url_non_hf_returns_none() {
        assert!(parse_hf_url("https://example.com/foo/bar/resolve/main/x.bin").is_none());
        assert!(parse_hf_url("https://github.com/owner/repo/releases/download/v1/x").is_none());
    }

    #[test]
    fn parse_hf_url_missing_segments_returns_none() {
        assert!(parse_hf_url("https://huggingface.co/foo/bar").is_none());
        assert!(parse_hf_url("https://huggingface.co/foo/bar/resolve/main/").is_none());
        assert!(parse_hf_url("https://huggingface.co/foo/bar/blob/main/x").is_none());
    }

    #[test]
    fn auth_redirect_policy_allows_hf_and_cdn_hosts() {
        assert!(should_send_auth_on_redirect("huggingface.co"));
        assert!(should_send_auth_on_redirect("cdn-lfs.huggingface.co"));
        assert!(should_send_auth_on_redirect(
            "d2l4uplgqnwxzd.cloudfront.net"
        ));
        assert!(should_send_auth_on_redirect("HUGGINGFACE.CO"));
    }

    #[test]
    fn auth_redirect_policy_blocks_other_hosts() {
        assert!(!should_send_auth_on_redirect("evil.com"));
        assert!(!should_send_auth_on_redirect("example.org"));
        assert!(!should_send_auth_on_redirect("127.0.0.1"));
        assert!(!should_send_auth_on_redirect("localhost"));
        // Substring trickery: "huggingface.co.evil.com" is NOT an HF host.
        assert!(!should_send_auth_on_redirect("huggingface.co.evil.com"));
    }

    #[test]
    fn blob_and_pointer_paths_join_correctly() {
        let _g = EnvGuard::new();
        let tmp = tempdir().unwrap();
        let cache = HfCache::new(tmp.path());
        let repo = cache.repo("bartowski/gemma-4-E2B-it-GGUF");
        let folder = tmp
            .path()
            .join("hf_cache")
            .join("hub")
            .join("models--bartowski--gemma-4-E2B-it-GGUF");

        assert_eq!(repo.folder_path(), folder);
        assert_eq!(repo.refs_path("main"), folder.join("refs").join("main"));
        assert_eq!(
            repo.snapshot_dir("abc123"),
            folder.join("snapshots").join("abc123")
        );
        assert_eq!(
            repo.blob_path("deadbeef"),
            folder.join("blobs").join("deadbeef")
        );

        let fetch = repo.file("gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            fetch.pointer_path("abc123"),
            folder
                .join("snapshots")
                .join("abc123")
                .join("gemma-4-E2B-it-Q4_K_M.gguf")
        );
        assert_eq!(
            fetch.blob_path("deadbeef"),
            folder.join("blobs").join("deadbeef")
        );
    }
}
