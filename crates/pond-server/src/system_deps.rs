//! System dependency checker and auto-installer.
//!
//! Goose In A Pond uses `cpal` for audio I/O (microphone capture and TTS playback).
//! On Linux, `cpal` links against ALSA — `libasound2-dev` and `pkg-config` must be
//! present at **build time** and at **runtime**.  On macOS and Windows these are
//! handled by the OS SDK and are always available.
//!
//! This module runs at `setup` time (and as a soft-check on `serve`) to:
//!   1. Detect missing system libraries.
//!   2. Print clear instructions.
//!   3. Attempt automatic installation when a supported package manager is found.
//!
//! Supported package managers (Linux):
//!   - `apt-get`  — Debian / Ubuntu / Raspberry Pi OS / Linux Mint
//!   - `dnf`      — Fedora / RHEL 8+ / CentOS Stream
//!   - `pacman`   — Arch Linux / Manjaro
//!
//! Supported package managers (macOS):
//!   - `brew`     — Homebrew
//!
//! Windows: no-op — all required DLLs ship with Windows.

#[cfg(not(windows))]
use std::process::Stdio;

// ── Required packages per package manager ────────────────────────────────────

#[cfg(not(windows))]
const APT_PACKAGES: &[&str] = &[
    "libasound2-dev",
    "pkg-config",
    "libssl-dev",
    "build-essential",
];
#[cfg(not(windows))]
const DNF_PACKAGES: &[&str] = &["alsa-lib-devel", "pkg-config", "openssl-devel", "gcc"];
#[cfg(not(windows))]
const PACMAN_PACKAGES: &[&str] = &["alsa-lib", "pkg-config", "openssl", "base-devel"];
#[cfg(not(windows))]
const BREW_PACKAGES: &[&str] = &["pkg-config", "openssl"]; // ALSA is not used on macOS

// ── Package manager detection ─────────────────────────────────────────────────

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy, PartialEq)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Brew,
}

#[cfg(not(windows))]
fn detect_package_manager() -> Option<PackageManager> {
    // Linux managers first, then macOS
    for (bin, pm) in &[
        ("apt-get", PackageManager::Apt),
        ("dnf", PackageManager::Dnf),
        ("pacman", PackageManager::Pacman),
        ("brew", PackageManager::Brew),
    ] {
        if which_bin(bin) {
            return Some(*pm);
        }
    }
    None
}

#[cfg(not(windows))]
fn which_bin(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Dependency checks ─────────────────────────────────────────────────────────

/// True if `pkg-config` tool is present.
#[cfg(not(windows))]
fn has_pkg_config() -> bool {
    which_bin("pkg-config")
}

/// True if the named library is detectable by pkg-config.
#[cfg(not(windows))]
fn pkg_config_exists(lib: &str) -> bool {
    std::process::Command::new("pkg-config")
        .arg("--exists")
        .arg(lib)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True if a C compiler is present (required to link native crates).
#[cfg(not(windows))]
fn has_c_compiler() -> bool {
    which_bin("cc") || which_bin("gcc") || which_bin("clang")
}

/// Collect which system packages appear to be missing.
/// Returns a list of human-readable labels (not package names — those vary by distro).
#[cfg(not(windows))]
fn missing_deps() -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !has_pkg_config() {
        missing.push("pkg-config");
    }
    // ALSA is Linux-only; macOS uses CoreAudio which is always available.
    #[cfg(target_os = "linux")]
    if !pkg_config_exists("alsa") {
        missing.push("ALSA dev headers (libasound2-dev)");
    }
    if !pkg_config_exists("openssl") {
        missing.push("OpenSSL dev headers (libssl-dev)");
    }
    if !has_c_compiler() {
        missing.push("C compiler (gcc / clang)");
    }
    missing
}

// ── Installation ──────────────────────────────────────────────────────────────

#[cfg(not(windows))]
async fn install_packages(pm: PackageManager) -> bool {
    let (cmd, sudo, args, packages): (&str, bool, &[&str], &[&str]) = match pm {
        PackageManager::Apt => ("apt-get", true, &["install", "-y"], APT_PACKAGES),
        PackageManager::Dnf => ("dnf", true, &["install", "-y"], DNF_PACKAGES),
        PackageManager::Pacman => ("pacman", true, &["-Sy", "--noconfirm"], PACMAN_PACKAGES),
        PackageManager::Brew => ("brew", false, &["install"], BREW_PACKAGES),
    };

    let mut full_args: Vec<&str> = args.to_vec();
    full_args.extend_from_slice(packages);

    let pkg_list = packages.join(" ");
    println!("  📦 Installing: {}", pkg_list);

    let status = if sudo {
        tokio::process::Command::new("sudo")
            .arg(cmd)
            .args(&full_args)
            .status()
            .await
    } else {
        tokio::process::Command::new(cmd)
            .args(&full_args)
            .status()
            .await
    };

    match status {
        Ok(s) if s.success() => {
            println!("  ✅ System packages installed.");
            true
        }
        Ok(s) => {
            println!("  ⚠  Package install exited with {}", s);
            false
        }
        Err(e) => {
            println!("  ⚠  Could not run package manager: {}", e);
            false
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Check system dependencies and attempt auto-install if possible.
///
/// Returns `true` when all required dependencies are present (or after a
/// successful auto-install).  Returns `false` when something is missing and
/// could not be installed — the caller should warn but continue.
///
/// No-op on Windows (always returns `true`).
pub async fn ensure_system_deps() -> bool {
    // Windows: nothing to do.
    #[cfg(windows)]
    return true;

    #[cfg(not(windows))]
    {
        let missing = missing_deps();
        if missing.is_empty() {
            return true;
        }

        println!("  ⚠  Missing system dependencies:");
        for dep in &missing {
            println!("       • {}", dep);
        }

        match detect_package_manager() {
            Some(pm) => {
                println!("  🔧 Attempting automatic installation...");
                let ok = install_packages(pm).await;
                if ok {
                    // Re-check after install
                    let still_missing = missing_deps();
                    if still_missing.is_empty() {
                        return true;
                    }
                    println!("  ⚠  Still missing after install: {:?}", still_missing);
                }
                false
            }
            None => {
                print_manual_install_instructions();
                false
            }
        }
    }
}

/// Soft check: print a warning if deps are missing but do not attempt install.
/// Used by `serve` at startup (we don't want to require sudo just to run).
pub fn warn_if_missing() {
    #[cfg(windows)]
    return;

    #[cfg(not(windows))]
    {
        let missing = missing_deps();
        if !missing.is_empty() {
            println!("  ⚠  Some system dependencies appear to be missing:");
            for dep in &missing {
                println!("       • {}", dep);
            }
            println!("     Run `pond-server setup` to attempt automatic installation,");
            println!("     or see docs/developer/linux-setup.md for manual instructions.");
        }
    }
}

#[cfg(not(windows))]
fn print_manual_install_instructions() {
    println!("  ℹ  No supported package manager found.");
    println!("     Please install the required packages manually:");
    println!();
    println!("     Ubuntu / Debian / Raspberry Pi OS:");
    println!("       sudo apt-get install -y libasound2-dev pkg-config libssl-dev build-essential");
    println!();
    println!("     Fedora / RHEL / CentOS Stream:");
    println!("       sudo dnf install -y alsa-lib-devel pkg-config openssl-devel gcc");
    println!();
    println!("     Arch Linux / Manjaro:");
    println!("       sudo pacman -Sy --noconfirm alsa-lib pkg-config openssl base-devel");
    println!();
    println!("     macOS (Homebrew):");
    println!("       brew install pkg-config openssl");
    println!();
    println!("     See also: docs/developer/linux-setup.md");
}
