//! Writes the [`HostCredential`] where only this OS account can read it.
//!
//! The file is replaced at every start, so a credential copied out of it, or a sign-in link
//! printed from it, stops working when the server restarts.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pond_api::host_guard::HostCredential;

/// File name inside the data directory, next to `.runtime_api_port`.
pub const FILE: &str = ".runtime_host_credential";

/// Where the running server keeps its credential.
pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// Generate a fresh credential and atomically replace the file with it, mode `0600`.
///
/// The temporary file is created `0600` in the same directory and renamed over the old one,
/// so no reader ever sees a partial value and a symlink planted at the path is replaced
/// rather than followed.
pub fn install(data_dir: &Path) -> Result<HostCredential> {
    let credential = HostCredential::generate();
    let mut file = tempfile::Builder::new()
        .prefix(".runtime_host_credential.")
        .tempfile_in(data_dir)
        .context("create the host credential file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(credential.as_str().as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path(data_dir))
        .context("install the host credential file")?;
    Ok(credential)
}

/// The running server's credential, for this account's CLI commands.
pub fn read(data_dir: &Path) -> Result<String> {
    let file = path(data_dir);
    let value = std::fs::read_to_string(&file).with_context(|| {
        format!(
            "could not read {}: is the Pond running under this account?",
            file.display()
        )
    })?;
    Ok(value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_install_replaces_the_credential_privately() {
        let dir = tempfile::tempdir().unwrap();
        let first = install(dir.path()).unwrap();
        assert_eq!(read(dir.path()).unwrap(), first.as_str());
        let second = install(dir.path()).unwrap();
        assert_ne!(first.as_str(), second.as_str());
        assert_eq!(read(dir.path()).unwrap(), second.as_str());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "a temporary file was left behind");
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_symlink_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, path(dir.path())).unwrap();
        install(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        assert!(!std::fs::symlink_metadata(path(dir.path()))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
