//! Auto-start helper and binary locator for Piper TTS.
//!
//! Unlike whisper.cpp, Piper is a one-shot subprocess (not a long-running
//! server), so this module only provides binary lookup — no spawn guard.
//!
//! Binary lookup order:
//!   1. `<data_dir>/bin/piper[.exe]`  — managed by `pond-server setup`
//!   2. Anywhere on `PATH`

use std::path::{Path, PathBuf};

use crate::model_download;

/// Find the piper binary: `<data_dir>/bin/` first, then `PATH`.
pub fn find_binary(data_dir: &Path) -> Option<PathBuf> {
    // 1. Canonical install location managed by GIAP setup.
    let local = model_download::piper_binary_path(data_dir);
    if local.exists() {
        return Some(local);
    }

    // 2. Anywhere on PATH.
    #[cfg(windows)]
    let name = "piper.exe";
    #[cfg(not(windows))]
    let name = "piper";

    if let Ok(path_var) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in path_var.split(sep) {
            let candidate = Path::new(dir).join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}
