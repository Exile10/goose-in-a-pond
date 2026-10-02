//! Points goose's LiteRT-LM backend at an installed library before any provider is built. It only
//! ever sets `GOOSE_LITERT_LIB_DIR`, only when that is unset, and downloads nothing.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The variable goose's `litert` backend searches first.
const LIB_DIR_ENV: &str = "GOOSE_LITERT_LIB_DIR";
/// Beside the executable: where the app bundle carries the library, and where the backend looks
/// when the variable is unset.
const BUNDLE_SUBDIR: &str = "litert-lm";
/// Overrides `~/.giap/litert-lm`, as it does for `scripts/lib/litert-setup.sh`.
const HOME_ENV: &str = "GIAP_LITERT_HOME";

#[cfg(target_os = "macos")]
const LIB_FILE: &str = "liblitert-lm.dylib";
#[cfg(windows)]
const LIB_FILE: &str = "litert-lm.dll";
#[cfg(not(any(target_os = "macos", windows)))]
const LIB_FILE: &str = "liblitert-lm.so";

/// Which library the backend loads. An explicit `GOOSE_LITERT_LIB_DIR` wins. A library in
/// `litert-lm/` beside the executable is the one an app was built and verified with, so it is
/// left to the backend, which finds it there; otherwise the variable is set to the newest
/// package under `<data_dir>/lib/litert-lm/` (a device), then to the one
/// `~/.giap/litert-lm/.path-<platform>` records (a development Mac). Nothing found leaves it unset.
pub fn ensure_library_dir(data_dir: &Path) {
    if let Some(dir) = std::env::var_os(LIB_DIR_ENV).filter(|d| !d.is_empty()) {
        tracing::info!(
            dir = %Path::new(&dir).display(),
            "LiteRT-LM library directory from {LIB_DIR_ENV}"
        );
        return;
    }
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(BUNDLE_SUBDIR)));
    match choose(bundled.as_deref(), data_dir, record_file().as_deref()) {
        Choice::Bundled(dir) => tracing::info!(
            dir = %dir.display(),
            "LiteRT-LM library beside the executable; left to the litert backend"
        ),
        Choice::Found(dir, from) => {
            tracing::info!(dir = %dir.display(), from, "LiteRT-LM library directory");
            std::env::set_var(LIB_DIR_ENV, &dir);
        }
        Choice::Nothing => tracing::info!(
            "no LiteRT-LM library beside the executable, in the data directory or recorded; \
             the litert backend falls back to its own search"
        ),
    }
}

/// Where the library is, by who should point the backend at it.
#[derive(Debug, PartialEq, Eq)]
enum Choice {
    /// Beside the executable: the backend finds it without being told.
    Bundled(PathBuf),
    /// Elsewhere, so `GOOSE_LITERT_LIB_DIR` must name it; with where it was found.
    Found(PathBuf, &'static str),
    Nothing,
}

fn choose(bundled: Option<&Path>, data_dir: &Path, record: Option<&Path>) -> Choice {
    if let Some(dir) = bundled.filter(|dir| dir.join(LIB_FILE).is_file()) {
        return Choice::Bundled(dir.to_path_buf());
    }
    match find_library_dir(data_dir, record) {
        Some((dir, from)) => Choice::Found(dir, from),
        None => Choice::Nothing,
    }
}

/// The library's directory and where it was found.
fn find_library_dir(data_dir: &Path, record: Option<&Path>) -> Option<(PathBuf, &'static str)> {
    newest_package(&data_dir.join("lib").join("litert-lm"))
        .map(|dir| (dir, "data directory"))
        .or_else(|| {
            record
                .and_then(recorded_dir)
                .map(|dir| (dir, "recorded package"))
        })
        .map(|(dir, from)| (std::fs::canonicalize(&dir).unwrap_or(dir), from))
}

/// The subdirectory of `root` whose library was modified last; ties go to the later name.
fn newest_package(root: &Path) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        let Ok(meta) = std::fs::metadata(dir.join(LIB_FILE)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(t, d)| (modified, &dir) > (*t, d))
        {
            newest = Some((modified, dir));
        }
    }
    newest.map(|(_, dir)| dir)
}

/// The package directory on the record's first line, if it holds the library. The line may
/// also name the library file itself.
fn recorded_dir(record: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(record).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let named = PathBuf::from(line);
    let dir = if named.is_file() {
        named.parent()?.to_path_buf()
    } else {
        named
    };
    dir.join(LIB_FILE).is_file().then_some(dir)
}

/// `~/.giap/litert-lm/.path-<platform>`, for the platforms a package is built for.
fn record_file() -> Option<PathBuf> {
    let platform = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "macos-arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-arm64"
    } else {
        return None;
    };
    let home = match std::env::var_os(HOME_ENV).filter(|h| !h.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => dirs::home_dir()?.join(".giap").join("litert-lm"),
    };
    Some(home.join(format!(".path-{platform}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn package(root: &Path, name: &str, age_secs: u64) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let lib = dir.join(LIB_FILE);
        std::fs::write(&lib, b"lib").unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&lib)
            .unwrap()
            .set_modified(when)
            .unwrap();
        dir
    }

    #[test]
    fn the_newest_library_under_the_data_dir_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib").join("litert-lm");
        package(&root, "1.0.0-aaaaaaaa", 3600);
        let newer = package(&root, "1.0.0-bbbbbbbb", 60);
        std::fs::create_dir_all(root.join("1.0.1-empty")).unwrap();
        assert_eq!(newest_package(&root), Some(newer));
    }

    #[test]
    fn the_data_dir_comes_before_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let installed = package(&tmp.path().join("lib").join("litert-lm"), "1.0.0-a", 10);
        let elsewhere = package(&tmp.path().join("home"), "macos-arm64", 10);
        let record = tmp.path().join(".path-macos-arm64");
        std::fs::write(&record, format!("{}\n", elsewhere.display())).unwrap();

        let (dir, from) = find_library_dir(tmp.path(), Some(&record)).unwrap();
        assert_eq!(dir, std::fs::canonicalize(installed).unwrap());
        assert_eq!(from, "data directory");
    }

    #[test]
    fn the_record_names_a_package_directory_or_its_library() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = package(tmp.path(), "macos-arm64", 10);
        let record = tmp.path().join(".path-macos-arm64");

        std::fs::write(&record, format!("{}\n", pkg.display())).unwrap();
        assert_eq!(recorded_dir(&record), Some(pkg.clone()));

        std::fs::write(&record, format!("{}\n", pkg.join(LIB_FILE).display())).unwrap();
        assert_eq!(recorded_dir(&record), Some(pkg.clone()));

        let (dir, from) = find_library_dir(&tmp.path().join("no-pond"), Some(&record)).unwrap();
        assert_eq!(dir, std::fs::canonicalize(&pkg).unwrap());
        assert_eq!(from, "recorded package");
    }

    /// An app must load the library it was built with, not a development package.
    #[test]
    fn the_library_beside_the_executable_comes_first() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = package(&tmp.path().join("Resources"), "litert-lm", 10);
        package(&tmp.path().join("lib").join("litert-lm"), "1.0.0-a", 10);
        let elsewhere = package(&tmp.path().join("home"), "macos-arm64", 10);
        let record = tmp.path().join(".path-macos-arm64");
        std::fs::write(&record, format!("{}\n", elsewhere.display())).unwrap();

        assert_eq!(
            choose(Some(&bundled), tmp.path(), Some(&record)),
            Choice::Bundled(bundled)
        );
        let empty = tmp.path().join("Resources-without").join("litert-lm");
        assert!(matches!(
            choose(Some(&empty), tmp.path(), Some(&record)),
            Choice::Found(_, "data directory")
        ));
        assert_eq!(
            choose(Some(&empty), &tmp.path().join("no-pond"), None),
            Choice::Nothing
        );
    }

    #[test]
    fn a_record_without_a_library_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let record = tmp.path().join(".path-macos-arm64");
        assert_eq!(recorded_dir(&record), None, "no record file");

        std::fs::write(&record, "\n").unwrap();
        assert_eq!(recorded_dir(&record), None, "empty record");

        std::fs::write(&record, format!("{}\n", tmp.path().display())).unwrap();
        assert_eq!(
            recorded_dir(&record),
            None,
            "a directory without the library"
        );

        assert_eq!(find_library_dir(tmp.path(), Some(&record)), None);
        assert_eq!(find_library_dir(tmp.path(), None), None);
    }
}
