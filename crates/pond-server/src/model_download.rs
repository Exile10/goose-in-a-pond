//! Whisper GGML model downloader.
//!
//! Downloads quantized Whisper model files from the ggerganov/whisper.cpp
//! HuggingFace repository into GIAP's data directory.
//!
//! These files are loaded by the **whisper.cpp server binary** (a separate
//! process that `WhisperInput` speaks to over HTTP).  This crate only
//! handles the one-time download; it does not run inference itself.

use anyhow::{anyhow, Context, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt as _;

// ── Model registry ────────────────────────────────────────────────────────────

pub struct WhisperModelInfo {
    pub name: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    /// Approximate size shown during download.
    pub size_mb: u64,
}

/// English-only GGML models ordered by size (smallest → largest).
pub const WHISPER_MODELS: &[WhisperModelInfo] = &[
    WhisperModelInfo {
        name: "tiny",
        filename: "ggml-tiny.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        size_mb: 39,
    },
    WhisperModelInfo {
        name: "base",
        filename: "ggml-base.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        size_mb: 141,
    },
    WhisperModelInfo {
        name: "small",
        filename: "ggml-small.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        size_mb: 244,
    },
];

/// Default model used when none is specified.
pub const DEFAULT_WHISPER_MODEL: &str = "base";

// ── Path helpers ──────────────────────────────────────────────────────────────

pub fn models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models")
}

/// Returns the on-disk path for a named model (does not check existence).
pub fn model_path(data_dir: &Path, model_name: &str) -> Result<PathBuf> {
    let info = find_model(model_name)?;
    Ok(models_dir(data_dir).join(info.filename))
}

fn find_model(name: &str) -> Result<&'static WhisperModelInfo> {
    WHISPER_MODELS
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow!("Unknown model '{}'. Available: tiny, base, small", name))
}

// ── Download ──────────────────────────────────────────────────────────────────

/// Download `model_name` into `<data_dir>/models/` with a live progress line.
///
/// Returns the path to the downloaded file.
/// If the file already exists it is returned immediately (no re-download).
pub async fn download_whisper_model(model_name: &str, data_dir: &Path) -> Result<PathBuf> {
    let info = find_model(model_name)?;

    let dir = models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let out_path = dir.join(info.filename);

    if out_path.exists() {
        println!("  ✅ Already downloaded: {}", out_path.display());
        return Ok(out_path);
    }

    println!("  ⬇  {} (~{} MB)", info.filename, info.size_mb);

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| anyhow!("Failed to build HTTP client: {}", e))?;

    let resp = client
        .get(info.url)
        .send()
        .await
        .map_err(|e| anyhow!("Download request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!("Server returned {}", resp.status()));
    }

    // Use Content-Length if available, else fall back to known approximate size.
    let total_bytes = resp
        .content_length()
        .unwrap_or(info.size_mb * 1_048_576);

    // Write to a temp file first so we never leave a partial .bin on disk.
    let tmp_path = out_path.with_extension("bin.part");
    let mut file = tokio::fs::File::create(&tmp_path).await?;
    let mut downloaded: u64 = 0;
    let mut resp = resp;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow!("Download interrupted: {}", e))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| anyhow!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;

        let pct = (downloaded * 100) / total_bytes.max(1);
        print!(
            "\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            total_bytes / 1_048_576,
            pct
        );
        std::io::stdout().flush().ok();
    }

    println!(); // newline after progress line

    // Atomic rename: only visible if fully written
    tokio::fs::rename(&tmp_path, &out_path).await?;
    println!("  ✅ Saved: {}", out_path.display());

    Ok(out_path)
}

// ── whisper-server binary download ────────────────────────────────────────────

/// Pinned stable release — repo moved from ggerganov → ggml-org at v1.8.x.
const WHISPER_RELEASE_TAG: &str = "v1.8.4";
const WHISPER_REPO: &str = "https://github.com/ggml-org/whisper.cpp";

/// Info about the platform-specific pre-built binary asset.
pub struct WhisperBinaryAsset {
    pub zip_url: &'static str,
    /// Name of the server executable inside the zip.
    pub server_exe: &'static str,
}

/// Returns the pre-built download asset for this platform, or `None` if none exists.
///
/// - Windows x64  → `whisper-bin-x64.zip` from ggml-org releases
/// - Linux x64    → no upstream pre-built; returns `None` (build from source)
/// - Linux ARM64  → no upstream pre-built; returns `None` (build from source)
pub fn whisper_binary_asset() -> Option<WhisperBinaryAsset> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(WhisperBinaryAsset {
        zip_url: "https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.4/whisper-bin-x64.zip",
        server_exe: "whisper-server.exe",
    });

    #[allow(unreachable_code)]
    None
}

/// Returns the on-disk path where the whisper-server binary should live.
pub fn whisper_binary_path(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return data_dir.join("bin").join("whisper-server.exe");
    #[cfg(not(windows))]
    return data_dir.join("bin").join("whisper-server");
}

// ── Obtain whisper-server binary (download or build) ─────────────────────────

/// Get the whisper-server binary into `<data_dir>/bin/`, using whichever method
/// is appropriate for this platform:
///
/// - **Windows x64**: downloads `whisper-bin-x64.zip` from the pinned release.
/// - **Linux ARM64 / x64**: builds from source (cmake + make), with NEON and
///   CUDA optimizations enabled when the toolchain is available.
/// - **Already present**: no-op.
pub async fn download_whisper_binary(data_dir: &Path) -> Result<PathBuf> {
    let dest = whisper_binary_path(data_dir);

    if dest.exists() {
        println!("  ✅ whisper-server already present: {}", dest.display());
        return Ok(dest);
    }

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;

    match whisper_binary_asset() {
        Some(asset) => fetch_whisper_zip(asset, data_dir, &dest).await,
        None => build_whisper_from_source(data_dir, &dest).await,
    }
}

/// Download the release zip and extract it into `<data_dir>/bin/`.
async fn fetch_whisper_zip(
    asset: WhisperBinaryAsset,
    data_dir: &Path,
    dest: &Path,
) -> Result<PathBuf> {
    println!("  ⬇  whisper-server ({}, pre-built)", WHISPER_RELEASE_TAG);

    let client = reqwest::Client::builder().build()?;
    let resp = client
        .get(asset.zip_url)
        .send()
        .await
        .context("Failed to fetch whisper binary zip")?;

    if !resp.status().is_success() {
        return Err(anyhow!("Download returned {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = if total > 0 { Vec::with_capacity(total as usize) } else { Vec::new() };

    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.context("Download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if total > 0 {
            let pct = (downloaded * 100) / total;
            print!("\r  ⬇  {} / {} MB  ({}%)",
                downloaded / 1_048_576, total / 1_048_576, pct);
            std::io::stdout().flush().ok();
        }
    }
    println!();

    let bin_dir = data_dir.join("bin");
    let server_exe = asset.server_exe.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let cursor = std::io::Cursor::new(buf);
        let mut archive = zip::ZipArchive::new(cursor)
            .context("Failed to open zip archive")?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_string();
            let file_name = std::path::Path::new(&name)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            if file_name.is_empty() {
                continue;
            }

            let out_path = bin_dir.join(&file_name);
            let mut out_file = std::fs::File::create(&out_path)
                .with_context(|| format!("Cannot write {}", out_path.display()))?;
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            out_file.write_all(&content)?;

            #[cfg(unix)]
            if file_name == server_exe {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(0o755))?;
            }
        }

        // Suppress unused warning on non-Unix platforms.
        let _ = &server_exe;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Zip extraction task panicked")??;

    println!("  ✅ whisper-server installed: {}", dest.display());
    Ok(dest.to_path_buf())
}

/// Build whisper-server from source using cmake.
///
/// Enables platform-appropriate optimizations:
/// - `-DGGML_NATIVE=ON`  — native CPU (NEON on ARM64, AVX2 on x86)
/// - `-DGGML_CUDA=ON`    — GPU acceleration when `nvcc` is on PATH (Jetson)
/// - `-DGGML_OPENMP=ON`  — multi-core inference
///
/// Clones into `<data_dir>/whisper-src/`, builds in `<data_dir>/whisper-src/build/`.
async fn build_whisper_from_source(data_dir: &Path, dest: &Path) -> Result<PathBuf> {
    // Check that git and cmake are available.
    for tool in &["git", "cmake"] {
        let ok = tokio::process::Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err(anyhow!(
                "`{}` not found — install it and re-run `pond-server setup`",
                tool
            ));
        }
    }

    let src_dir = data_dir.join("whisper-src");
    let build_dir = src_dir.join("build");

    // Clone (skip if already present).
    if !src_dir.join(".git").exists() {
        println!("  📦 Cloning whisper.cpp source ({})...", WHISPER_RELEASE_TAG);
        run_cmd(
            tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", "--branch", WHISPER_RELEASE_TAG, WHISPER_REPO])
                .arg(&src_dir),
            "git clone",
        ).await?;
    } else {
        println!("  📦 whisper.cpp source already cloned, skipping.");
    }

    // Detect CUDA (nvcc in PATH → Jetson / CUDA workstation).
    let has_cuda = tokio::process::Command::new("nvcc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    println!("  🔨 Configuring cmake (CUDA: {})...", if has_cuda { "enabled" } else { "disabled" });

    let mut cmake_cfg = tokio::process::Command::new("cmake");
    cmake_cfg
        .arg("-B").arg(&build_dir)
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DGGML_NATIVE=ON")
        .arg("-DGGML_OPENMP=ON")
        .current_dir(&src_dir);
    if has_cuda {
        cmake_cfg.arg("-DGGML_CUDA=ON");
    }
    run_cmd(&mut cmake_cfg, "cmake configure").await?;

    // Determine parallelism: use all cores.
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "4".to_string());

    println!("  🔨 Building whisper-server ({} jobs)...", jobs);
    run_cmd(
        tokio::process::Command::new("cmake")
            .args(["--build"])
            .arg(&build_dir)
            .args(["-j", &jobs, "--config", "Release", "--target", "whisper-server"])
            .current_dir(&src_dir),
        "cmake build",
    ).await?;

    // Copy binary to data_dir/bin/.
    let built = build_dir.join("bin").join("whisper-server");
    if !built.exists() {
        return Err(anyhow!(
            "Build succeeded but whisper-server not found at {}",
            built.display()
        ));
    }
    tokio::fs::copy(&built, dest).await
        .with_context(|| format!("Failed to copy binary to {}", dest.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }

    println!("  ✅ whisper-server installed: {}", dest.display());
    Ok(dest.to_path_buf())
}

// ── Piper TTS model download ───────────────────────────────────────────────────

/// HuggingFace base URL for rhasspy/piper-voices.
const PIPER_VOICES_BASE: &str =
    "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium";

pub const PIPER_MODEL_FILENAME: &str = "en_US-lessac-medium.onnx";
const PIPER_MODEL_JSON_FILENAME: &str = "en_US-lessac-medium.onnx.json";
const PIPER_MODEL_SIZE_MB: u64 = 65;

/// Directory for TTS voice models: `<data_dir>/models/tts/`.
pub fn tts_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("tts")
}

/// Download `en_US-lessac-medium.onnx` + `.onnx.json` into `<data_dir>/models/tts/`.
///
/// Both files are required — piper reads the JSON config alongside the ONNX weights.
/// Returns the path to the `.onnx` file.
pub async fn download_piper_model(data_dir: &Path) -> Result<PathBuf> {
    let dir = tts_models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;

    let onnx_path = dir.join(PIPER_MODEL_FILENAME);
    let json_path = dir.join(PIPER_MODEL_JSON_FILENAME);

    // Download .onnx weights
    if onnx_path.exists() {
        println!("  ✅ Already downloaded: {}", onnx_path.display());
    } else {
        let url = format!("{}/{}", PIPER_VOICES_BASE, PIPER_MODEL_FILENAME);
        download_file(&url, &onnx_path, PIPER_MODEL_SIZE_MB).await?;
    }

    // Download .onnx.json config (tiny, but required)
    if json_path.exists() {
        println!("  ✅ Already downloaded: {}", json_path.display());
    } else {
        let url = format!("{}/{}", PIPER_VOICES_BASE, PIPER_MODEL_JSON_FILENAME);
        download_file(&url, &json_path, 1).await?;
    }

    Ok(onnx_path)
}

// ── Piper binary download ──────────────────────────────────────────────────────

/// Pinned Piper release.
const PIPER_RELEASE_TAG: &str = "2023.11.14-2";
const PIPER_GITHUB_BASE: &str =
    "https://github.com/rhasspy/piper/releases/download/2023.11.14-2";

/// Returns the on-disk path where the piper binary should live.
pub fn piper_binary_path(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return data_dir.join("bin").join("piper.exe");
    #[cfg(not(windows))]
    return data_dir.join("bin").join("piper");
}

/// Download and install the platform-appropriate piper binary into `<data_dir>/bin/`.
pub async fn download_piper_binary(data_dir: &Path) -> Result<PathBuf> {
    let dest = piper_binary_path(data_dir);

    if dest.exists() {
        println!("  ✅ piper already present: {}", dest.display());
        return Ok(dest);
    }

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let (archive_name, is_zip) = ("piper_windows_amd64.zip", true);

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    let (archive_name, is_zip) = ("piper_linux_aarch64.tar.gz", false);

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let (archive_name, is_zip) = ("piper_linux_x86_64.tar.gz", false);

    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    return Err(anyhow!(
        "No pre-built piper binary for this platform — build from source: https://github.com/rhasspy/piper"
    ));

    println!(
        "  ⬇  piper TTS binary ({}, {})",
        PIPER_RELEASE_TAG, archive_name
    );

    let url = format!("{}/{}", PIPER_GITHUB_BASE, archive_name);
    let client = reqwest::Client::builder().build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch piper binary archive")?;

    if !resp.status().is_success() {
        return Err(anyhow!("Download returned {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = if total > 0 {
        Vec::with_capacity(total as usize)
    } else {
        Vec::new()
    };

    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.context("Download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if total > 0 {
            let pct = (downloaded * 100) / total;
            print!(
                "\r  ⬇  {} / {} MB  ({}%)",
                downloaded / 1_048_576,
                total / 1_048_576,
                pct
            );
            std::io::stdout().flush().ok();
        }
    }
    println!();

    let bin_dir = data_dir.join("bin");

    if is_zip {
        // Windows: extract piper.exe from zip
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let cursor = std::io::Cursor::new(buf);
            let mut archive = zip::ZipArchive::new(cursor)
                .context("Failed to open piper zip archive")?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                let name = entry.name().to_string();
                let file_name = std::path::Path::new(&name)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                if file_name.is_empty() {
                    continue;
                }
                let out_path = bin_dir.join(&file_name);
                let mut out_file = std::fs::File::create(&out_path)
                    .with_context(|| format!("Cannot write {}", out_path.display()))?;
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                out_file.write_all(&content)?;
            }
            let _ = &dest_clone; // suppress unused warning
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("Zip extraction task panicked")??;
    } else {
        // Linux: extract piper binary from .tar.gz
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || {
            use flate2::read::GzDecoder;
            use tar::Archive;
            let gz = GzDecoder::new(std::io::Cursor::new(buf));
            let mut tar = Archive::new(gz);
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?;
                let file_name = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                if file_name.is_empty() {
                    continue;
                }
                let out_path = bin_dir.join(&file_name);
                entry.unpack(&out_path)?;

                #[cfg(unix)]
                if file_name == "piper" {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            let _ = &dest_clone;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("Tar extraction task panicked")??;
    }

    println!("  ✅ piper installed: {}", dest.display());
    Ok(dest)
}

// ── Generic file download helper ──────────────────────────────────────────────

/// Download `url` to `dest`, showing a live progress line.  Skips if `dest` exists.
async fn download_file(url: &str, dest: &Path, approx_size_mb: u64) -> Result<()> {
    println!("  ⬇  {} (~{} MB)", dest.file_name().unwrap_or_default().to_string_lossy(), approx_size_mb);

    let client = reqwest::Client::builder().build()?;
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch {url}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!("Server returned {} for {url}", resp.status()));
    }

    let total = resp
        .content_length()
        .unwrap_or(approx_size_mb * 1_048_576);

    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut downloaded: u64 = 0;
    let mut resp = resp;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow!("Download interrupted: {}", e))?
    {
        use tokio::io::AsyncWriteExt as _;
        file.write_all(&chunk)
            .await
            .map_err(|e| anyhow!("Write error: {}", e))?;
        downloaded += chunk.len() as u64;
        let pct = (downloaded * 100) / total.max(1);
        print!(
            "\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576,
            total / 1_048_576,
            pct
        );
        std::io::stdout().flush().ok();
    }

    println!();
    tokio::fs::rename(&tmp, dest).await?;
    println!("  ✅ Saved: {}", dest.display());
    Ok(())
}

/// Run a `tokio::process::Command`, streaming its output, and return an error on non-zero exit.
async fn run_cmd(cmd: &mut tokio::process::Command, label: &str) -> Result<()> {
    let status = cmd
        .status()
        .await
        .with_context(|| format!("Failed to run {}", label))?;
    if !status.success() {
        return Err(anyhow!("{} failed (exit {})", label, status));
    }
    Ok(())
}
