fn main() {
    // On Linux, Tauri compiles against the system WebKitGTK stack. Verify those
    // libraries are present up front and fail with an actionable message that
    // points at scripts/install-desktop-deps.sh, instead of letting the build
    // die deep inside webkit2gtk-sys with a cryptic pkg-config error. No-op on
    // macOS/Windows, which use a built-in WebView (no GTK dependency).
    #[cfg(target_os = "linux")]
    check_webkitgtk_deps();

    tauri_build::build()
}

/// pkg-config modules the desktop build needs, paired with their apt packages.
/// Kept in sync with scripts/install-desktop-deps.sh (the single source of truth
/// that actually installs them).
#[cfg(target_os = "linux")]
fn check_webkitgtk_deps() {
    const REQUIRED: &[(&str, &str)] = &[
        ("webkit2gtk-4.1", "libwebkit2gtk-4.1-dev"),
        ("gtk+-3.0", "libgtk-3-dev"),
        ("libsoup-3.0", "libsoup-3.0-dev"),
        ("javascriptcoregtk-4.1", "libjavascriptcoregtk-4.1-dev"),
    ];

    // If pkg-config itself is unavailable we cannot check reliably — leave it to
    // the downstream sys-crate build rather than emitting a false failure.
    if std::process::Command::new("pkg-config")
        .arg("--version")
        .output()
        .is_err()
    {
        println!("cargo:warning=pkg-config not found; skipping WebKitGTK dependency preflight");
        return;
    }

    let missing: Vec<&str> = REQUIRED
        .iter()
        .filter(|(module, _)| {
            !std::process::Command::new("pkg-config")
                .args(["--exists", module])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .map(|(_, apt_pkg)| *apt_pkg)
        .collect();

    if !missing.is_empty() {
        panic!(
            "\n\n\
             GIAP desktop build: missing system WebKitGTK libraries: {}\n\
             Tauri on Linux needs these to compile. Install them with:\n\n\
             \x20   bash scripts/install-desktop-deps.sh\n\n\
             or manually:\n\n\
             \x20   sudo apt-get install -y {}\n\n",
            missing.join(", "),
            missing.join(" "),
        );
    }
}
