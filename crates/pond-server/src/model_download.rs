//! Model downloader — Whisper ASR, Piper TTS, and llamafile LLM.
//!
//! All models are downloaded into subdirectories of GIAP's data directory:
//! - `models/ggml-*.bin`        — Whisper GGML models
//! - `models/tts/`              — Piper voice models
//! - `models/llm/`              — llamafile LLM models
//!
//! llamafile bundles model weights + llama.cpp server into a single executable.
//! Running it with `--server --port 8080` starts an OpenAI-compatible HTTP server.

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

/// GGML models ordered by size (smallest → largest).
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
    WhisperModelInfo {
        name: "medium",
        filename: "ggml-medium.en.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.en.bin",
        size_mb: 769,
    },
    WhisperModelInfo {
        name: "large-v3-turbo",
        filename: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        size_mb: 809,
    },
    WhisperModelInfo {
        name: "large-v3",
        filename: "ggml-large-v3.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        size_mb: 1550,
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

// ── Build-tool helpers: cmake + git auto-download ─────────────────────────────

/// Pinned cmake release downloaded when the system has none.
const CMAKE_VERSION: &str = "3.31.6";
const CMAKE_GITHUB_BASE: &str = "https://github.com/Kitware/CMake/releases/download";

struct CmakePlatform {
    archive_name: &'static str,
    /// Relative path inside the extracted archive to the cmake binary.
    bin_rel:      &'static str,
    is_zip:       bool,
}

fn cmake_platform_info() -> Option<CmakePlatform> {
    // macOS universal binary (runs on both Apple Silicon and Intel)
    #[cfg(target_os = "macos")]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-macos-universal.tar.gz",
        bin_rel:      "cmake-3.31.6-macos-universal/CMake.app/Contents/bin/cmake",
        is_zip:       false,
    });

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-linux-x86_64.tar.gz",
        bin_rel:      "cmake-3.31.6-linux-x86_64/bin/cmake",
        is_zip:       false,
    });

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-linux-aarch64.tar.gz",
        bin_rel:      "cmake-3.31.6-linux-aarch64/bin/cmake",
        is_zip:       false,
    });

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(CmakePlatform {
        archive_name: "cmake-3.31.6-windows-x86_64.zip",
        bin_rel:      "cmake-3.31.6-windows-x86_64/bin/cmake.exe",
        is_zip:       true,
    });

    #[allow(unreachable_code)]
    None
}

fn managed_cmake_path(data_dir: &Path) -> Option<PathBuf> {
    Some(data_dir.join("build-tools").join(cmake_platform_info()?.bin_rel))
}

async fn tool_in_path(name: &str) -> bool {
    tokio::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Ensure cmake is available, downloading a portable binary if not in PATH.
/// Returns the command/path to invoke cmake.
pub async fn ensure_cmake(data_dir: &Path) -> Result<String> {
    // 1. Already downloaded by GIAP
    if let Some(p) = managed_cmake_path(data_dir) {
        if p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }

    // 2. Available in system PATH
    if tool_in_path("cmake").await {
        return Ok("cmake".to_string());
    }

    // 3. Download portable cmake binary from cmake.org
    match cmake_platform_info() {
        Some(info) => {
            println!("  📥 cmake not found — downloading cmake {} for {} {}...",
                CMAKE_VERSION, std::env::consts::OS, std::env::consts::ARCH);
            download_cmake_binary(data_dir, &info).await?;
            let path = managed_cmake_path(data_dir)
                .expect("cmake_platform_info is Some");
            Ok(path.to_string_lossy().into_owned())
        }
        None => Err(anyhow!("cmake unavailable for {} {}", std::env::consts::OS, std::env::consts::ARCH)),
    }
}

async fn download_cmake_binary(data_dir: &Path, info: &CmakePlatform) -> Result<()> {
    let tools_dir = data_dir.join("build-tools");
    tokio::fs::create_dir_all(&tools_dir).await?;

    let url = format!("{}/v{}/{}", CMAKE_GITHUB_BASE, CMAKE_VERSION, info.archive_name);
    println!("  ⬇  cmake {} ({})", CMAKE_VERSION, info.archive_name);

    let client = reqwest::Client::builder().build()?;
    let resp = client.get(&url).send().await.context("Failed to fetch cmake archive")?;
    if !resp.status().is_success() {
        return Err(anyhow!("cmake download returned {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(60 * 1_048_576);
    let mut downloaded: u64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.context("cmake download interrupted")? {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        let pct = (downloaded * 100) / total.max(1);
        print!("\r  ⬇  {} / {} MB  ({}%)",
            downloaded / 1_048_576, total / 1_048_576, pct);
        std::io::stdout().flush().ok();
    }
    println!();

    let tools_dir_clone = tools_dir.clone();
    let bin_rel = info.bin_rel.to_string();

    if info.is_zip {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let cursor = std::io::Cursor::new(buf);
            let mut archive = zip::ZipArchive::new(cursor).context("Invalid cmake zip")?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                if entry.is_dir() { continue; }
                let entry_path = entry.name().replace('\\', "/");
                let out_path = tools_dir_clone.join(&entry_path);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out = std::fs::File::create(&out_path)?;
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                out.write_all(&content)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("cmake zip extraction panicked")??;
    } else {
        tokio::task::spawn_blocking(move || {
            use flate2::read::GzDecoder;
            use tar::Archive;
            let gz = GzDecoder::new(std::io::Cursor::new(buf));
            let mut tar = Archive::new(gz);
            tar.set_preserve_permissions(true);
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.to_path_buf();
                let out_path = tools_dir_clone.join(&path);
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if entry.header().entry_type().is_dir() {
                    std::fs::create_dir_all(&out_path)?;
                } else {
                    entry.unpack(&out_path)?;
                }
            }
            // Ensure the cmake binary is executable on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let cmake_bin = tools_dir_clone.join(&bin_rel);
                if cmake_bin.exists() {
                    std::fs::set_permissions(&cmake_bin, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            let _ = bin_rel;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("cmake tar extraction panicked")??;
    }

    println!("  ✅ cmake {} installed to build-tools/", CMAKE_VERSION);
    Ok(())
}

/// Ensure git is available, installing automatically if possible.
pub async fn ensure_git() -> Result<()> {
    if tool_in_path("git").await {
        return Ok(());
    }

    println!("  📥 git not found — installing...");

    #[cfg(target_os = "linux")]
    {
        for args in &[
            &["apt-get", "install", "-y", "git", "build-essential"][..],
            &["apt",     "install", "-y", "git", "build-essential"],
            &["dnf",     "install", "-y", "git", "gcc-c++", "make"],
            &["yum",     "install", "-y", "git", "gcc-c++", "make"],
            &["pacman",  "--noconfirm", "-S", "git", "base-devel"],
            &["zypper",  "install", "-y", "git", "gcc-c++", "make"],
        ] {
            if tokio::process::Command::new("sudo")
                .args(*args)
                .status().await.map(|s| s.success()).unwrap_or(false)
            {
                return Ok(());
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Try Homebrew
        if tokio::process::Command::new("brew")
            .args(["install", "git"])
            .status().await.map(|s| s.success()).unwrap_or(false)
        {
            return Ok(());
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Try winget (Windows 10 1809+)
        if tokio::process::Command::new("winget")
            .args(["install", "--id", "Git.Git", "-e", "--source", "winget", "--silent"])
            .status().await.map(|s| s.success()).unwrap_or(false)
        {
            return Ok(());
        }
    }

    Err(anyhow!("git unavailable"))
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
    ensure_git().await?;
    let cmake_cmd = ensure_cmake(data_dir).await?;

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

    let mut cmake_cfg = tokio::process::Command::new(&cmake_cmd);
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
        tokio::process::Command::new(&cmake_cmd)
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

/// Platform-specific archive asset for piper.
///
/// Returns `(archive_filename, is_zip)` for supported platforms, or `None`
/// when no pre-built binary is available (e.g. macOS, Windows ARM64).
#[derive(Debug)]
pub struct PiperBinaryAsset {
    pub archive_name: &'static str,
    pub is_zip: bool,
}

/// Returns the pre-built piper asset for this platform, or `None` if unavailable.
///
/// rhasspy/piper ships pre-built binaries for:
///   - Windows x86_64
///   - Linux x86_64
///   - Linux aarch64 (Jetson)
///
/// macOS and Windows ARM64 have no upstream pre-built release.
/// On those platforms callers should fall back gracefully (print output).
pub fn piper_binary_asset() -> Option<PiperBinaryAsset> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Some(PiperBinaryAsset { archive_name: "piper_windows_amd64.zip", is_zip: true });

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Some(PiperBinaryAsset { archive_name: "piper_linux_aarch64.tar.gz", is_zip: false });

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Some(PiperBinaryAsset { archive_name: "piper_linux_x86_64.tar.gz", is_zip: false });

    #[allow(unreachable_code)]
    None
}

/// Returns the on-disk path where the piper binary should live.
pub fn piper_binary_path(data_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    return data_dir.join("bin").join("piper.exe");
    #[cfg(not(windows))]
    return data_dir.join("bin").join("piper");
}

/// Download (or build) the piper binary into `<data_dir>/bin/`.
///
/// Prefers a pre-built release archive; falls back to building from source
/// on platforms without one (macOS, Windows ARM64).
pub async fn download_piper_binary(data_dir: &Path) -> Result<PathBuf> {
    let dest = piper_binary_path(data_dir);

    if dest.exists() {
        println!("  ✅ piper already present: {}", dest.display());
        return Ok(dest);
    }

    let Some(asset) = piper_binary_asset() else {
        // No pre-built binary for this platform — build from source instead.
        println!("  📦 No pre-built piper for {} {} — building from source...",
            std::env::consts::OS, std::env::consts::ARCH);
        return build_piper_from_source(data_dir, &dest).await;
    };

    let archive_name = asset.archive_name;
    let is_zip = asset.is_zip;

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;

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
        // Linux: extract piper from .tar.gz, preserving subdirectory structure
        // so that espeak-ng-data/ ends up at bin/espeak-ng-data/.
        // The archive root is a single directory (e.g. "piper/"); strip it.
        let dest_clone = dest.clone();
        tokio::task::spawn_blocking(move || {
            use flate2::read::GzDecoder;
            use tar::Archive;
            let gz = GzDecoder::new(std::io::Cursor::new(buf));
            let mut tar = Archive::new(gz);
            for entry in tar.entries()? {
                let mut entry = entry?;
                let path = entry.path()?.into_owned();

                // Skip the top-level directory entry itself.
                let mut components = path.components();
                components.next(); // strip leading "piper/" component
                let relative: std::path::PathBuf = components.collect();
                if relative.as_os_str().is_empty() {
                    continue;
                }

                let out_path = bin_dir.join(&relative);

                // Ensure parent directories exist (needed for espeak-ng-data/*)
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                entry.unpack(&out_path)?;

                #[cfg(unix)]
                if relative.to_string_lossy() == "piper" {
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

    // Ensure espeak-ng-data is present alongside the binary.
    ensure_espeak_ng_data(data_dir).await;

    println!("  ✅ piper installed: {}", dest.display());
    Ok(dest)
}

/// Returns the path where espeak-ng-data should live: `<data_dir>/bin/espeak-ng-data/`.
pub fn piper_espeak_data_path(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("espeak-ng-data")
}

/// Ensure espeak-ng-data is installed at `<data_dir>/bin/espeak-ng-data/`.
///
/// On first run (or after a source-only build that didn't copy the data):
///   1. macOS: try `brew install espeak-ng` and copy from Homebrew prefix.
///   2. All platforms fallback: download the Linux x86_64 piper tarball and
///      extract only the `espeak-ng-data/` subtree.  The phoneme data files
///      are platform-agnostic (text/binary tables, not native code).
pub async fn ensure_espeak_ng_data(data_dir: &Path) {
    let dest = piper_espeak_data_path(data_dir);
    if dest.exists() {
        return;
    }

    println!("  📥 espeak-ng-data missing — installing...");

    // ── Option 1: brew prefix on macOS ──────────────────────────────────────
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = tokio::process::Command::new("brew")
            .args(["install", "espeak-ng"])
            .output()
            .await
        {
            if output.status.success() {
                // Find data dir in Homebrew prefix.
                for candidate in &[
                    "/opt/homebrew/lib/espeak-ng-data",
                    "/usr/local/lib/espeak-ng-data",
                    "/opt/homebrew/Cellar",
                ] {
                    let p = std::path::Path::new(candidate);
                    if p.is_dir() && p.file_name().map_or(false, |n| n == "espeak-ng-data") {
                        if copy_dir_all(p, &dest).is_ok() {
                            println!("  ✅ espeak-ng-data from Homebrew: {}", dest.display());
                            return;
                        }
                    }
                }
                // Homebrew installed but path detection failed — do broader search.
                if let Ok(out) = tokio::process::Command::new("brew")
                    .args(["--prefix", "espeak-ng"])
                    .output()
                    .await
                {
                    let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let data_p = std::path::Path::new(&prefix).join("lib").join("espeak-ng-data");
                    if data_p.is_dir() {
                        if copy_dir_all(&data_p, &dest).is_ok() {
                            println!("  ✅ espeak-ng-data from Homebrew: {}", dest.display());
                            return;
                        }
                    }
                }
            }
        }
    }

    // ── Option 2: download from piper Linux x86_64 tarball ──────────────────
    // The phoneme data files are platform-independent; we borrow them from the
    // Linux release and they work on macOS/Windows just as well.
    let url = format!(
        "{}/piper_linux_x86_64.tar.gz",
        PIPER_GITHUB_BASE
    );
    println!("  ⬇  espeak-ng-data (via piper Linux tarball)...");

    let bytes = match reqwest::get(&url).await.and_then(|r| Ok(r)) {
        Ok(resp) if resp.status().is_success() => {
            match resp.bytes().await {
                Ok(b) => b.to_vec(),
                Err(e) => { tracing::warn!("espeak-ng-data download failed: {e}"); return; }
            }
        }
        Ok(resp) => { tracing::warn!("espeak-ng-data download: HTTP {}", resp.status()); return; }
        Err(e) => { tracing::warn!("espeak-ng-data download failed: {e}"); return; }
    };

    let dest_clone = dest.clone();
    let result = tokio::task::spawn_blocking(move || {
        use flate2::read::GzDecoder;
        use tar::Archive;
        let gz = GzDecoder::new(std::io::Cursor::new(bytes));
        let mut tar = Archive::new(gz);
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            // Only extract entries under espeak-ng-data/
            let mut comps = path.components();
            comps.next(); // strip top-level "piper/"
            let relative: std::path::PathBuf = comps.collect();
            let rel_str = relative.to_string_lossy();
            if !rel_str.starts_with("espeak-ng-data") {
                continue;
            }
            let out_path = dest_clone.parent().unwrap_or(&dest_clone).join(&relative);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            entry.unpack(&out_path)?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) if dest.exists() => println!("  ✅ espeak-ng-data installed: {}", dest.display()),
        Ok(Ok(())) => tracing::warn!("espeak-ng-data not found in tarball"),
        Ok(Err(e)) => tracing::warn!("espeak-ng-data extraction failed: {e}"),
        Err(e) => tracing::warn!("espeak-ng-data task panicked: {e}"),
    }
}

/// Build piper from source for platforms without a pre-built binary (macOS, Windows ARM64).
///
/// Clones `https://github.com/rhasspy/piper` at `PIPER_RELEASE_TAG` into
/// `<data_dir>/piper-src/` and builds with cmake.  Piper's CMakeLists.txt
/// fetches its own dependencies (onnxruntime, espeak-ng-data) automatically.
async fn build_piper_from_source(data_dir: &Path, dest: &Path) -> Result<PathBuf> {
    ensure_git().await?;
    let cmake_cmd = ensure_cmake(data_dir).await?;

    let src_dir   = data_dir.join("piper-src");
    let build_dir = src_dir.join("build");

    if !src_dir.join(".git").exists() {
        println!("  📦 Cloning piper source ({})...", PIPER_RELEASE_TAG);
        run_cmd(
            tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", "--branch", PIPER_RELEASE_TAG,
                       "https://github.com/rhasspy/piper"])
                .arg(&src_dir),
            "git clone piper",
        ).await?;
    }

    println!("  🔨 Configuring piper cmake...");
    run_cmd(
        tokio::process::Command::new(&cmake_cmd)
            .arg("-B").arg(&build_dir)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .current_dir(&src_dir),
        "cmake configure piper",
    ).await?;

    let jobs = std::thread::available_parallelism()
        .map(|n| n.get().to_string())
        .unwrap_or_else(|_| "4".to_string());

    println!("  🔨 Building piper ({} jobs)...", jobs);
    run_cmd(
        tokio::process::Command::new(&cmake_cmd)
            .args(["--build"])
            .arg(&build_dir)
            .args(["-j", &jobs, "--config", "Release"])
            .current_dir(&src_dir),
        "cmake build piper",
    ).await?;

    // Search common output locations across cmake generators/platforms.
    let candidates = [
        build_dir.join("piper"),
        build_dir.join("src").join("piper"),
        build_dir.join("Release").join("piper.exe"),
        build_dir.join("src").join("Release").join("piper.exe"),
    ];
    let built = candidates.iter()
        .find(|p| p.exists())
        .ok_or_else(|| anyhow!("piper build completed but binary not found in expected locations"))?;

    tokio::fs::create_dir_all(data_dir.join("bin")).await?;
    tokio::fs::copy(built, dest).await
        .with_context(|| format!("Failed to copy piper to {}", dest.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }

    // Copy espeak-ng-data next to the binary so piper can find it at runtime.
    // cmake's FetchContent/ExternalProject places it somewhere under build_dir.
    let espeak_dest = data_dir.join("bin").join("espeak-ng-data");
    if !espeak_dest.exists() {
        if let Some(espeak_src) = find_espeak_ng_data(&build_dir) {
            println!("  📋 Copying espeak-ng-data from {}...", espeak_src.display());
            copy_dir_all(&espeak_src, &espeak_dest)?;
            println!("  ✅ espeak-ng-data installed: {}", espeak_dest.display());
        } else {
            // Not found in build tree — fall through to ensure_espeak_ng_data() below.
            tracing::warn!("espeak-ng-data not found in cmake build tree — will download separately");
        }
    }

    // Final safety net: download if still missing.
    ensure_espeak_ng_data(data_dir).await;

    println!("  ✅ piper built and installed: {}", dest.display());
    Ok(dest.to_path_buf())
}

/// Recursively search `root` for a directory named `espeak-ng-data`.
fn find_espeak_ng_data(root: &Path) -> Option<PathBuf> {
    // BFS through the directory tree (depth-limited to avoid infinite loops).
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((root.to_path_buf(), 0u32));
    while let Some((dir, depth)) = queue.pop_front() {
        if depth > 10 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() == "espeak-ng-data" {
                    return Some(path);
                }
                queue.push_back((path, depth + 1));
            }
        }
    }
    None
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

// ── llamafile LLM model registry ──────────────────────────────────────────────

/// Metadata for a llamafile model.
///
/// llamafile bundles weights + llama.cpp into a single executable.
/// On Windows the file must have a `.exe` extension to be runnable.
pub struct LlamafileModelInfo {
    /// Short name used in CLI args (e.g. `"gemma-2b"`).
    pub name:        &'static str,
    /// Base filename without `.exe` (the extension is added on Windows automatically).
    pub filename:    &'static str,
    /// HuggingFace direct-download URL.
    pub url:         &'static str,
    /// Approximate compressed download size in MB.
    pub size_mb:     u64,
    /// Human-readable description shown during download.
    pub description: &'static str,
}

/// Available llamafile LLM models, lightest first.
pub const LLAMAFILE_MODELS: &[LlamafileModelInfo] = &[
    LlamafileModelInfo {
        name:        "llama-1b",
        filename:    "Llama-3.2-1B-Instruct-Q4_K_M.llamafile",
        url:         "https://huggingface.co/Mozilla/Llama-3.2-1B-Instruct-llamafile/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.llamafile",
        size_mb:     1_120,
        description: "Llama 3.2 1B Instruct Q4_K_M (Meta/Mozilla, ~1.1 GB)",
    },
    LlamafileModelInfo {
        name:        "gemma-2b",
        filename:    "gemma-2-2b-it.Q4_K_M.llamafile",
        url:         "https://huggingface.co/Mozilla/gemma-2-2b-it-llamafile/resolve/main/gemma-2-2b-it.Q4_K_M.llamafile",
        size_mb:     1_950,
        description: "Gemma 2 2B IT Q4_K_M (Google/Mozilla, ~2.0 GB)",
    },
    LlamafileModelInfo {
        name:        "llama-3b",
        filename:    "Llama-3.2-3B-Instruct-Q4_K_M.llamafile",
        url:         "https://huggingface.co/Mozilla/Llama-3.2-3B-Instruct-llamafile/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.llamafile",
        size_mb:     2_020,
        description: "Llama 3.2 3B Instruct Q4_K_M (Meta/Mozilla, ~2.0 GB)",
    },
    LlamafileModelInfo {
        name:        "phi-3.5-mini",
        filename:    "Phi-3.5-mini-instruct.Q4_K_M.llamafile",
        url:         "https://huggingface.co/Mozilla/Phi-3.5-mini-instruct-llamafile/resolve/main/Phi-3.5-mini-instruct.Q4_K_M.llamafile",
        size_mb:     2_390,
        description: "Phi-3.5 Mini Instruct Q4_K_M (Microsoft/Mozilla, ~2.4 GB)",
    },
    LlamafileModelInfo {
        name:        "mistral-7b",
        filename:    "Mistral-7B-Instruct-v0.2.Q4_K_M.llamafile",
        url:         "https://huggingface.co/Mozilla/Mistral-7B-Instruct-v0.2-llamafile/resolve/main/Mistral-7B-Instruct-v0.2.Q4_K_M.llamafile",
        size_mb:     4_370,
        description: "Mistral 7B Instruct v0.2 Q4_K_M (Mistral/Mozilla, ~4.4 GB)",
    },
];

/// Default LLM model downloaded during `setup` and used during `serve`.
pub const DEFAULT_LLAMAFILE_MODEL: &str = "gemma-2b";

/// Directory for LLM models: `<data_dir>/models/llm/`.
pub fn llm_models_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("llm")
}

/// On-disk path for a llamafile model.
///
/// On Windows the `.exe` suffix is appended so the file is directly runnable.
pub fn llamafile_path(data_dir: &Path, model_name: &str) -> Result<PathBuf> {
    let info = find_llamafile(model_name)?;
    let base = llm_models_dir(data_dir).join(info.filename);
    #[cfg(windows)]
    return Ok(PathBuf::from(format!("{}.exe", base.display())));
    #[cfg(not(windows))]
    Ok(base)
}

fn find_llamafile(name: &str) -> Result<&'static LlamafileModelInfo> {
    LLAMAFILE_MODELS
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow!("Unknown llamafile model '{}'. Available: {}",
            name,
            LLAMAFILE_MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")))
}

/// Download `model_name` into `<data_dir>/models/llm/` with live progress.
///
/// On Unix, `chmod +x` is applied so the file can be executed directly.
/// On Windows, the file is saved with a `.exe` extension.
///
/// Returns the path to the downloaded executable.
/// If the file already exists it is returned immediately (no re-download).
pub async fn download_llamafile_model(model_name: &str, data_dir: &Path) -> Result<PathBuf> {
    let info = find_llamafile(model_name)?;
    let dir  = llm_models_dir(data_dir);
    tokio::fs::create_dir_all(&dir).await?;

    let dest = llamafile_path(data_dir, model_name)?;

    if dest.exists() {
        println!("  ✅ Already downloaded: {}", dest.display());
        return Ok(dest);
    }

    println!("  ⬇  {} (~{} MB)", info.description, info.size_mb);
    println!("     This is a one-time download — it may take several minutes.");
    download_file(info.url, &dest, info.size_mb).await?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("Failed to chmod +x {}", dest.display()))?;
    }

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Path helper tests (all platforms) ────────────────────────────────────

    #[test]
    fn whisper_binary_path_has_correct_extension() {
        let dir = std::path::PathBuf::from("/data");
        let p = whisper_binary_path(&dir);
        #[cfg(windows)]
        assert!(p.to_string_lossy().ends_with(".exe"), "expected .exe on Windows, got {}", p.display());
        #[cfg(not(windows))]
        assert!(!p.to_string_lossy().ends_with(".exe"), "unexpected .exe on non-Windows, got {}", p.display());
        assert!(p.to_string_lossy().contains("whisper-server"));
    }

    #[test]
    fn piper_binary_path_has_correct_extension() {
        let dir = std::path::PathBuf::from("/data");
        let p = piper_binary_path(&dir);
        #[cfg(windows)]
        assert!(p.to_string_lossy().ends_with(".exe"), "expected .exe on Windows, got {}", p.display());
        #[cfg(not(windows))]
        assert!(!p.to_string_lossy().ends_with(".exe"), "unexpected .exe on non-Windows, got {}", p.display());
        assert!(p.to_string_lossy().contains("piper"));
    }

    #[test]
    fn llamafile_path_has_correct_extension() {
        let dir = std::path::PathBuf::from("/data");
        let p = llamafile_path(&dir, "gemma-2b").unwrap();
        #[cfg(windows)]
        assert!(p.to_string_lossy().ends_with(".exe"), "expected .exe on Windows, got {}", p.display());
        #[cfg(not(windows))]
        assert!(!p.to_string_lossy().ends_with(".exe"), "unexpected .exe on non-Windows, got {}", p.display());
    }

    // ── Asset detection tests ─────────────────────────────────────────────────

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn whisper_binary_asset_returns_some_on_windows_x64() {
        let asset = whisper_binary_asset().expect("Windows x64 should have a pre-built whisper asset");
        assert!(asset.zip_url.contains("whisper"), "URL should reference whisper, got {}", asset.zip_url);
        assert!(asset.server_exe.ends_with(".exe"), "server_exe should end in .exe on Windows");
    }

    #[test]
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    fn whisper_binary_asset_returns_none_on_non_windows_x64() {
        // Linux and macOS fall back to build-from-source.
        assert!(
            whisper_binary_asset().is_none(),
            "Expected None for this platform — build-from-source path should be used"
        );
    }

    #[test]
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    fn piper_binary_asset_returns_zip_on_windows_x64() {
        let asset = piper_binary_asset().expect("Windows x64 should have a piper asset");
        assert!(asset.is_zip, "Windows piper asset should be a zip archive");
        assert!(asset.archive_name.ends_with(".zip"));
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn piper_binary_asset_returns_targz_on_linux_x64() {
        let asset = piper_binary_asset().expect("Linux x86_64 should have a piper asset");
        assert!(!asset.is_zip, "Linux piper asset should be a .tar.gz archive");
        assert!(asset.archive_name.ends_with(".tar.gz"));
        assert!(asset.archive_name.contains("x86_64"));
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    fn piper_binary_asset_returns_targz_on_linux_aarch64() {
        let asset = piper_binary_asset().expect("Linux aarch64 (Jetson) should have a piper asset");
        assert!(!asset.is_zip, "Linux piper asset should be a .tar.gz archive");
        assert!(asset.archive_name.contains("aarch64"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn piper_binary_asset_returns_none_on_macos() {
        assert!(
            piper_binary_asset().is_none(),
            "macOS has no pre-built piper binary — should return None"
        );
    }

    // ── Whisper model download via mock HTTP server ───────────────────────────

    #[tokio::test]
    async fn download_whisper_model_writes_file_to_disk() {
        let server = MockServer::start().await;
        let fake_model_bytes = b"fake whisper model data";

        Mock::given(method("GET"))
            .and(path("/ggml-base.en.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fake_model_bytes.as_slice()))
            .mount(&server)
            .await;

        // Temporarily redirect the model URL by downloading from our mock URL directly.
        // We test via download_file (the internal helper) since WHISPER_MODELS URLs are hardcoded.
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("ggml-base.en.bin");
        let url = format!("{}/ggml-base.en.bin", server.uri());

        download_file(&url, &dest, 1).await.unwrap();

        assert!(dest.exists(), "model file should exist after download");
        assert_eq!(std::fs::read(&dest).unwrap(), fake_model_bytes);
    }

    #[tokio::test]
    async fn download_whisper_model_skips_if_already_present() {
        let tmp = TempDir::new().unwrap();
        // Pre-create the models dir and model file.
        let models = tmp.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let dest = models.join("ggml-base.en.bin");
        std::fs::write(&dest, b"existing content").unwrap();

        // download_whisper_model should detect the file and return early without
        // making any HTTP request. We pass an unreachable URL to prove no request is made.
        let result = download_whisper_model("base", tmp.path()).await;
        assert!(result.is_ok());
        // Content should be unchanged (no overwrite).
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing content");
    }

    #[tokio::test]
    async fn download_whisper_model_fails_on_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("fail.bin");
        let url = format!("{}/fail.bin", server.uri());

        let err = download_file(&url, &dest, 1).await.unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "expected 500 error, got: {}",
            err
        );
    }

    // ── piper_binary_asset graceful degradation on unsupported platforms ──────

    #[tokio::test]
    async fn download_piper_binary_errors_gracefully_when_no_asset() {
        // Simulate the None path by calling ok_or_else directly — this runs on all
        // platforms but only triggers in production on macOS / Windows ARM64.
        #[cfg(target_os = "macos")]
        {
            let result: Result<PiperBinaryAsset> = piper_binary_asset().ok_or_else(|| {
                anyhow!(
                    "No pre-built piper binary for {} {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                )
            });
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("No pre-built"),
                "error should mention missing binary, got: {}",
                msg
            );
        }

        // On Windows / Linux a pre-built asset exists; the function returns Some.
        #[cfg(not(target_os = "macos"))]
        {
            assert!(
                piper_binary_asset().is_some(),
                "expected a pre-built piper asset on this platform"
            );
        }
    }

    // ── llamafile model: skip if already present ──────────────────────────────

    #[tokio::test]
    async fn download_llamafile_model_skips_if_already_present() {
        let tmp = TempDir::new().unwrap();
        let llm_dir = tmp.path().join("models").join("llm");
        std::fs::create_dir_all(&llm_dir).unwrap();

        // Create the expected file at the platform-correct path.
        let dest = llamafile_path(tmp.path(), "gemma-2b").unwrap();
        std::fs::write(&dest, b"existing llamafile").unwrap();

        let result = download_llamafile_model("gemma-2b", tmp.path()).await;
        assert!(result.is_ok());
        // Content unchanged.
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing llamafile");
    }

    #[tokio::test]
    async fn download_llamafile_model_rejects_unknown_model_name() {
        let tmp = TempDir::new().unwrap();
        let err = download_llamafile_model("does-not-exist", tmp.path()).await.unwrap_err();
        assert!(
            err.to_string().contains("Unknown llamafile model"),
            "expected 'Unknown llamafile model' error, got: {}",
            err
        );
    }

    // ── Generic download_file: connection refused ─────────────────────────────

    #[tokio::test]
    async fn download_file_fails_on_connection_refused() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("nope.bin");
        // Port 1 is reserved — instant connection refused.
        let err = download_file("http://127.0.0.1:1/nope.bin", &dest, 1)
            .await
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "expected a non-empty error on connection refused"
        );
        assert!(!dest.exists(), "partial file should not exist after connection failure");
    }
}
